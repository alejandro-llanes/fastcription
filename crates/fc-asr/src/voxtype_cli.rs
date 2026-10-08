//! The adapter that actually talks to voxtype: `voxtype -q transcribe <wav>`
//! run once per chunk, no daemon involved (ARCHITECTURE.md D1).
//!
//! This is the one place in the workspace that parses voxtype's stdout, and
//! the one place that should ever need to change if voxtype's output does.
//! The contract below was measured directly against voxtype 1.0.1 and is
//! explicitly **not** a versioned interface (ARCHITECTURE.md §1, §7):
//!
//! ```text
//! Loading audio file: "sp.wav"
//! Audio format: 16000 Hz, 1 channel(s), Int
//! Processing 110924 samples (6.93s)...
//!
//! The quarterly roadmap review is scheduled for next Tuesday, and we still
//! need owners for the migration work.
//! ```
//!
//! Three banner lines, a blank line, then the transcript (stdout). All of
//! whisper.cpp's own logging goes to stderr and is ignored. An empty
//! transcript is a legitimate outcome (voxtype logs a WARN to stderr and
//! returns nothing for music/noise); anything that doesn't match this shape
//! is a parse error, never silently treated as an empty transcript, because
//! that would quietly drop real meeting audio.

use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fc_core::{EngineInfo, Segment};

use crate::transcriber::{AsrError, Transcriber};

const LOADING_PREFIX: &str = "Loading audio file:";
const FORMAT_PREFIX: &str = "Audio format:";
const PROCESSING_PREFIX: &str = "Processing ";

/// What voxtype prints on stderr when `--model` names something it has never
/// heard of. It then **exits 0** having transcribed with its own default model,
/// so without this check the `EngineInfo` recorded with the conversation claims
/// a model that never ran. Measured on voxtype 1.0.1: the string lives in the
/// binary (`strings` finds `Unknown model '…', using default model '…'`) but
/// `-q` suppressed it on this machine's `transcribe` path, so this is a guard
/// against the builds and engines that do print it, not a verified capture.
const UNKNOWN_MODEL_MARKER: &str = "Unknown model";

/// Floor under the duration-scaled default timeout, so a very short chunk
/// (which still pays model-load cost) isn't given an unreasonably tight
/// budget.
const MIN_TIMEOUT_SECS: f64 = 30.0;
/// voxtype measured at ~9x realtime (ARCHITECTURE.md §1); 10x realtime is a
/// generous multiple above that measured cost, so exceeding it means
/// something is actually wrong rather than just "a bit slow".
const TIMEOUT_REALTIME_MULTIPLE: f64 = 10.0;

/// How much of stdout/stderr to keep in an error message. Generous enough to
/// show the real problem, bounded so a runaway process doesn't produce a
/// multi-megabyte log line.
const ERROR_SNIPPET_MAX_CHARS: usize = 2_000;

fn truncate_for_error(s: &str) -> String {
    if s.chars().count() <= ERROR_SNIPPET_MAX_CHARS {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(ERROR_SNIPPET_MAX_CHARS).collect();
        format!("{truncated}... [truncated]")
    }
}

/// Parses the stdout of `voxtype -q transcribe`, returning the transcript
/// text (possibly empty). Pure function, no process involved, so the exact
/// shape above is covered by golden tests without needing the real binary.
fn parse_transcript_output(stdout: &str) -> Result<String, AsrError> {
    let mut lines = stdout.lines();

    let unexpected = |reason: &str, stdout: &str| AsrError::UnexpectedOutput {
        reason: reason.to_string(),
        stdout: truncate_for_error(stdout),
    };

    let l1 = lines
        .next()
        .ok_or_else(|| unexpected("stdout is empty, expected the banner", stdout))?;
    if !l1.starts_with(LOADING_PREFIX) {
        return Err(unexpected(
            &format!("first line does not start with {LOADING_PREFIX:?}"),
            stdout,
        ));
    }

    let l2 = lines
        .next()
        .ok_or_else(|| unexpected("stdout ended after the first banner line", stdout))?;
    if !l2.starts_with(FORMAT_PREFIX) {
        return Err(unexpected(
            &format!("second line does not start with {FORMAT_PREFIX:?}"),
            stdout,
        ));
    }

    let l3 = lines
        .next()
        .ok_or_else(|| unexpected("stdout ended after the second banner line", stdout))?;
    if !l3.starts_with(PROCESSING_PREFIX) {
        return Err(unexpected(
            &format!("third line does not start with {PROCESSING_PREFIX:?}"),
            stdout,
        ));
    }

    let blank = lines.next().ok_or_else(|| {
        unexpected(
            "stdout ended right after the banner, with no blank-line separator",
            stdout,
        )
    })?;
    if !blank.trim().is_empty() {
        return Err(unexpected(
            "expected a blank line after the banner, found more text instead",
            stdout,
        ));
    }

    // Everything after the blank line is the transcript, verbatim, however
    // many lines it spans. Lines here are never re-checked against the
    // banner prefixes: a transcript that happens to start with "Processing "
    // (someone talking about "processing the request") must not be mistaken
    // for a fourth banner line, because position, not content, decides what
    // the banner is.
    let transcript: String = lines.collect::<Vec<_>>().join("\n");
    Ok(transcript.trim().to_string())
}

/// Rewrites `file` from the start as a 16-bit mono WAV of `pcm`.
///
/// Takes an open file rather than a path because one `VoxtypeCli` reuses a
/// single scratch file for every pass: a pass runs about once a second for the
/// length of a meeting, and a three-hour meeting creating and unlinking that
/// many temporary WAVs — each holding up to `max_utterance` of audio, ~640 KB
/// at 20 s — churns through gigabytes for no reason. Truncating first matters:
/// a shorter utterance must not leave the tail of a longer one behind the
/// header it just wrote.
fn rewrite_wav(file: &mut std::fs::File, pcm: &[f32], sample_rate: u32) -> Result<(), AsrError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    file.seek(SeekFrom::Start(0)).map_err(AsrError::TempFile)?;
    file.set_len(0).map_err(AsrError::TempFile)?;

    let mut writer = hound::WavWriter::new(&mut *file, spec)
        .map_err(|e| AsrError::TempFile(std::io::Error::other(e)))?;
    for &s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        let sample = (clamped * i16::MAX as f32).round() as i16;
        writer
            .write_sample(sample)
            .map_err(|e| AsrError::TempFile(std::io::Error::other(e)))?;
    }
    writer
        .finalize()
        .map_err(|e| AsrError::TempFile(std::io::Error::other(e)))?;
    // voxtype reads this file from another process, so the bytes have to be out
    // of our buffers before it is told the path.
    file.flush().map_err(AsrError::TempFile)?;
    Ok(())
}

/// Runs `child` to completion, reading stdout/stderr on background threads so
/// a chatty process can't deadlock the pipe, and giving up after `timeout`.
///
/// **`timeout` bounds the whole call, not just the wait for the child.**
/// `child.kill()` only terminates the direct child; a descendant that inherited
/// our stdout/stderr pipes keeps their write ends open, so the pipe sees no EOF
/// until that orphan exits on its own -- which may be long after `timeout`, or
/// never. That is reachable in normal use: `--gpu-isolation` runs the model in a
/// helper process, and a wrapper shell script around `voxtype` does the same
/// thing by accident. If the child's exit were followed by a plain `join` on the
/// readers, such a run would wedge `transcribe()` for ever, which wedges the
/// stream thread, which means `Session::stop` never returns. So the readers hand
/// their buffers over a channel and are waited on with whatever is left of the
/// budget; on expiry they are abandoned, each still exiting and being reaped by
/// the OS the moment its pipe actually closes.
fn run_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, AsrError> {
    let stdout_rx = read_to_end_on_a_thread(child.stdout.take());
    let stderr_rx = read_to_end_on_a_thread(child.stderr.take());

    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AsrError::Timeout { timeout });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };

    let stdout = collect_within(&stdout_rx, timeout.saturating_sub(start.elapsed()))
        .ok_or(AsrError::Timeout { timeout })?;
    let stderr = collect_within(&stderr_rx, timeout.saturating_sub(start.elapsed()))
        .ok_or(AsrError::Timeout { timeout })?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Drains one pipe on its own thread, handing the whole buffer back at EOF.
fn read_to_end_on_a_thread<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// `None` means the reader did not reach EOF in time -- someone still holds the
/// pipe. A disconnected channel (the reader thread panicked) is treated as an
/// empty read, since the caller's parse will reject it with the real stdout in
/// the message.
fn collect_within(rx: &mpsc::Receiver<Vec<u8>>, budget: Duration) -> Option<Vec<u8>> {
    match rx.recv_timeout(budget) {
        Ok(buf) => Some(buf),
        Err(RecvTimeoutError::Timeout) => None,
        Err(RecvTimeoutError::Disconnected) => Some(Vec::new()),
    }
}

/// Adapter that shells out to `voxtype -q transcribe` for each chunk.
///
/// Never writes to `~/.config/voxtype/config.toml` (ARCHITECTURE.md D6): all
/// overrides go through per-invocation CLI flags, and a `None` override
/// leaves voxtype to use whatever its own config file says.
///
/// Not `Clone`: the scratch WAV every pass rewrites is part of the adapter, and
/// two clones transcribing at once would overwrite each other's audio. One
/// adapter per stream, which is what `session.rs`'s per-track factory already
/// builds.
#[derive(Debug)]
pub struct VoxtypeCli {
    binary: String,
    /// A config file passed as `-c`, for the settings voxtype exposes only
    /// through a file. The caller owns that file; this never writes one.
    config: Option<std::path::PathBuf>,
    engine: Option<String>,
    model: Option<String>,
    language: Option<String>,
    /// `--threads`. Always `Some` by default (see [`Default`] below) rather
    /// than leaving voxtype to pick its own: measured on this machine (Core
    /// Ultra 9 285K, `base.en`, CPU), 8 threads transcribes a 7s window in
    /// 0.21s versus 0.28s when voxtype schedules across all 24 cores --
    /// whisper.cpp stops scaling past ~8 threads for a model this size, so
    /// more cores just adds scheduling overhead. [`Self::with_threads`] still
    /// overrides this for a caller that knows better (a bigger model, a
    /// different engine, a machine with few cores).
    threads: Option<u32>,
    translate: bool,
    /// `None` means "compute a generous default from the chunk's duration"
    /// (see [`TIMEOUT_REALTIME_MULTIPLE`]); `Some` pins an exact timeout,
    /// mainly useful for tests.
    timeout: Option<Duration>,
    /// The one WAV every pass rewrites, created on the first pass and unlinked
    /// when this adapter is dropped. Behind a mutex because [`Transcriber`] is
    /// `&self`: the lock is held for the whole subprocess run, so a second
    /// caller waits rather than rewriting the file voxtype is reading.
    scratch: Mutex<Option<tempfile::NamedTempFile>>,
}

impl Default for VoxtypeCli {
    fn default() -> Self {
        Self {
            binary: "voxtype".to_string(),
            config: None,
            engine: None,
            model: None,
            language: None,
            threads: Some(default_thread_count()),
            translate: false,
            timeout: None,
            scratch: Mutex::new(None),
        }
    }
}

/// `min(8, available_parallelism())`: see the doc comment on
/// [`VoxtypeCli::threads`] for the measurement behind the cap. The
/// `available_parallelism` floor keeps a machine with fewer cores from being
/// told to use more threads than it has; the `unwrap_or(4)` fallback only
/// matters on a platform where the OS can't report core count at all.
fn default_thread_count() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4)
        .min(8)
}

impl VoxtypeCli {
    /// The arguments that precede the `transcribe` subcommand.
    ///
    /// Kept as a pure function so the ordering is testable without spawning a
    /// process: every override voxtype accepts is a global option, and putting
    /// one after the subcommand makes voxtype exit 2 with a usage error instead
    /// of transcribing. That failure mode is silent enough (a usage message on
    /// stderr, no transcript) to be worth a regression test.
    /// Runs voxtype against a specific config file.
    ///
    /// Some of voxtype's most useful settings have no command-line flag —
    /// `context_window_optimization`, which more than doubles the speed of the
    /// short passes realtime transcription needs, is one. `voxtype -c <file>`
    /// is how to reach them without touching the user's own configuration.
    pub fn with_config(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.config = Some(path.into());
        self
    }

    fn global_args(&self) -> Vec<String> {
        let mut args = vec!["-q".to_string()];
        if let Some(config) = &self.config {
            args.push("-c".into());
            args.push(config.display().to_string());
        }
        if let Some(engine) = &self.engine {
            args.push("--engine".into());
            args.push(engine.clone());
        }
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(language) = &self.language {
            args.push("--language".into());
            args.push(language.clone());
        }
        if let Some(threads) = self.threads {
            args.push("--threads".into());
            args.push(threads.to_string());
        }
        if self.translate {
            args.push("--translate".into());
        }
        args
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the binary name/path looked up on `PATH` (or an absolute
    /// path). Mainly for tests pointing at a fake binary.
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn with_engine(mut self, engine: impl Into<String>) -> Self {
        self.engine = Some(engine.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    pub fn with_threads(mut self, threads: u32) -> Self {
        self.threads = Some(threads);
        self
    }

    pub fn with_translate(mut self, translate: bool) -> Self {
        self.translate = translate;
        self
    }

    /// Pins an exact timeout instead of the duration-scaled default.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn default_timeout_for(&self, pcm_len: usize, sample_rate: u32) -> Duration {
        let audio_secs = pcm_len as f64 / sample_rate.max(1) as f64;
        Duration::from_secs_f64((audio_secs * TIMEOUT_REALTIME_MULTIPLE).max(MIN_TIMEOUT_SECS))
    }
}

impl Transcriber for VoxtypeCli {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<Vec<Segment>, AsrError> {
        if pcm.is_empty() {
            return Ok(Vec::new());
        }

        // Checked here rather than once at construction: the file is written by
        // the app at session start and could be removed underneath a running
        // meeting. `voxtype -c <missing>` exits 0 with its own defaults, so this
        // is the only way the caller learns the optimisation stopped applying.
        if let Some(config) = &self.config {
            if !config.exists() {
                return Err(AsrError::ConfigMissing(config.clone()));
            }
        }

        // Held across the whole run: the path handed to voxtype must still hold
        // this pass's audio when voxtype opens it.
        let mut scratch = self
            .scratch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if scratch.is_none() {
            *scratch = Some(
                tempfile::Builder::new()
                    .prefix("fc-asr-")
                    .suffix(".wav")
                    .tempfile()
                    .map_err(AsrError::TempFile)?,
            );
        }
        let tmp = scratch.as_mut().expect("just created above");
        rewrite_wav(tmp.as_file_mut(), pcm, sample_rate)?;

        let timeout = self
            .timeout
            .unwrap_or_else(|| self.default_timeout_for(pcm.len(), sample_rate));

        let mut cmd = Command::new(&self.binary);
        // Every override is a GLOBAL option of `voxtype`, not an option of the
        // `transcribe` subcommand: `voxtype -q --model base.en transcribe f.wav`
        // works, while `voxtype -q transcribe f.wav --model base.en` exits 2
        // with a usage error. Verified against voxtype 1.0.1 — hence the
        // ordering here and the regression test below.
        cmd.args(self.global_args())
            .arg("transcribe")
            .arg(tmp.path());
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                AsrError::BinaryNotFound {
                    binary: self.binary.clone(),
                    source,
                }
            } else {
                AsrError::Io(source)
            }
        })?;

        let output = run_with_timeout(child, timeout)?;
        // The scratch file stays; the next pass rewrites it. Releasing the lock
        // here keeps it held for exactly as long as voxtype held the file open.
        drop(scratch);

        if !output.status.success() {
            return Err(AsrError::NonZeroExit {
                status: output.status.code().unwrap_or(-1),
                stderr: truncate_for_error(&String::from_utf8_lossy(&output.stderr)),
            });
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains(UNKNOWN_MODEL_MARKER) {
            return Err(AsrError::UnknownModel {
                requested: self.model.clone().unwrap_or_else(|| "default".to_string()),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let transcript = parse_transcript_output(&stdout)?;

        if transcript.is_empty() {
            // Legitimate degenerate-transcript outcome (music/noise): zero
            // segments, not an error.
            return Ok(Vec::new());
        }

        let duration_ms = (pcm.len() as u64 * 1_000) / sample_rate.max(1) as u64;
        Ok(vec![Segment {
            // Placeholders: see `transcriber::stamp_segment` for who fills
            // these in and why.
            track: fc_core::Track::Selected,
            seq: 0,
            start_ms: 0,
            end_ms: duration_ms,
            text: transcript,
            translation: None,
            speaker: None,
            confidence: None,
            provisional: false,
        }])
    }

    fn describe(&self) -> EngineInfo {
        EngineInfo {
            // "default" when not overridden: this adapter never reads
            // voxtype's own config.toml (D6), so it genuinely doesn't know
            // what voxtype will use. Resolving that is `fc-voxtype`'s job
            // (`voxtype info engines|models`), not this one.
            engine: self.engine.clone().unwrap_or_else(|| "default".to_string()),
            model: self.model.clone().unwrap_or_else(|| "default".to_string()),
            language: self
                .language
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            backend: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes an executable shell script and hands back its path, for the
    /// behaviours only an actual child process can produce (a wedged pipe, a
    /// WARN on stderr, the argv a pass was given).
    ///
    /// The spawn-and-kill at the end is not pointless. Writing an executable in
    /// one test thread while another test's `Command` forks leaves a window
    /// where the forked child still holds an inherited write descriptor to the
    /// new file, and the kernel refuses to exec a file anyone has open for
    /// writing (`ETXTBSY`). That window is absorbed here, where retrying is
    /// free, rather than surfacing as a baffling failure in whichever test lost
    /// the race.
    fn fake_binary(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fake binary");
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&path).expect("stat").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod");
        }

        for _ in 0..200 {
            match Command::new(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        path
    }

    const GOLDEN_OK: &str = "Loading audio file: \"sp.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 110924 samples (6.93s)...\n\
\n\
The quarterly roadmap review is scheduled for next Tuesday, and we still need owners for the migration work.\n";

    #[test]
    fn parses_the_measured_golden_output() {
        let text = parse_transcript_output(GOLDEN_OK).expect("should parse");
        assert_eq!(
            text,
            "The quarterly roadmap review is scheduled for next Tuesday, and we still need owners for the migration work."
        );
    }

    #[test]
    fn empty_transcript_is_not_an_error() {
        let stdout = "Loading audio file: \"noise.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 48000 samples (3.00s)...\n\
\n\
\n";
        let text = parse_transcript_output(stdout).expect("empty transcript is legitimate");
        assert_eq!(text, "");
    }

    #[test]
    fn empty_transcript_with_no_trailing_line_at_all() {
        let stdout = "Loading audio file: \"noise.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 48000 samples (3.00s)...\n\
\n";
        let text = parse_transcript_output(stdout).expect("empty transcript is legitimate");
        assert_eq!(text, "");
    }

    #[test]
    fn multi_line_transcript_is_preserved() {
        let stdout = "Loading audio file: \"long.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 320000 samples (20.00s)...\n\
\n\
First sentence here.\n\
Second sentence continues on its own line.\n";
        let text = parse_transcript_output(stdout).expect("should parse");
        assert_eq!(
            text,
            "First sentence here.\nSecond sentence continues on its own line."
        );
    }

    #[test]
    fn banner_without_blank_line_is_an_error() {
        let stdout = "Loading audio file: \"sp.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 110924 samples (6.93s)...\n\
The transcript runs right into the banner.\n";
        let err = parse_transcript_output(stdout).unwrap_err();
        assert!(matches!(err, AsrError::UnexpectedOutput { .. }));
    }

    #[test]
    fn completely_unexpected_output_is_an_error() {
        let stdout = "error: model not found\n";
        let err = parse_transcript_output(stdout).unwrap_err();
        assert!(matches!(err, AsrError::UnexpectedOutput { .. }));
    }

    #[test]
    fn totally_empty_stdout_is_an_error_not_an_empty_transcript() {
        let err = parse_transcript_output("").unwrap_err();
        assert!(matches!(err, AsrError::UnexpectedOutput { .. }));
    }

    #[test]
    fn transcript_line_starting_with_processing_is_not_mistaken_for_banner() {
        let stdout = "Loading audio file: \"sp.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 12345 samples (1.00s)...\n\
\n\
Processing payroll should be done by Friday.\n";
        let text = parse_transcript_output(stdout).expect("should parse");
        assert_eq!(text, "Processing payroll should be done by Friday.");
    }

    #[test]
    fn wrong_first_line_is_an_error() {
        let stdout = "Something else entirely\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 1 samples (0.00s)...\n\
\n\
hi\n";
        let err = parse_transcript_output(stdout).unwrap_err();
        assert!(matches!(err, AsrError::UnexpectedOutput { .. }));
    }

    #[test]
    fn blank_line_in_the_middle_of_the_transcript_is_preserved() {
        // A transcript that itself contains a blank line (e.g. a pause
        // between two spoken paragraphs) must not be mistaken for a second
        // banner-separator or otherwise truncated -- only the single blank
        // line right after the banner is the separator.
        let stdout = "Loading audio file: \"sp.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 1 samples (0.00s)...\n\
\n\
First paragraph.\n\
\n\
Second paragraph after a pause.\n";
        let text = parse_transcript_output(stdout).expect("should parse");
        assert_eq!(text, "First paragraph.\n\nSecond paragraph after a pause.");
    }

    #[test]
    fn leading_blank_line_in_the_transcript_itself_is_trimmed() {
        // Banner separator consumed, then the transcript's own first line
        // happens to be blank before the real text starts.
        let stdout = "Loading audio file: \"sp.wav\"\n\
Audio format: 16000 Hz, 1 channel(s), Int\n\
Processing 1 samples (0.00s)...\n\
\n\
\n\
Real text starts on the second line.\n";
        let text = parse_transcript_output(stdout).expect("should parse");
        assert_eq!(text, "Real text starts on the second line.");
    }

    #[test]
    fn crlf_line_endings_parse_the_same_as_lf() {
        let stdout = "Loading audio file: \"sp.wav\"\r\n\
Audio format: 16000 Hz, 1 channel(s), Int\r\n\
Processing 110924 samples (6.93s)...\r\n\
\r\n\
The transcript line.\r\n";
        let text = parse_transcript_output(stdout).expect("should parse CRLF");
        assert_eq!(text, "The transcript line.");
    }

    #[test]
    fn wrong_second_line_is_an_error() {
        let stdout = "Loading audio file: \"sp.wav\"\n\
Not the format line\n\
Processing 1 samples (0.00s)...\n\
\n\
hi\n";
        let err = parse_transcript_output(stdout).unwrap_err();
        assert!(matches!(err, AsrError::UnexpectedOutput { .. }));
    }

    #[test]
    fn missing_binary_produces_actionable_error() {
        let cli = VoxtypeCli::new().with_binary("fc-asr-definitely-not-a-real-binary");
        let err = cli.transcribe(&[0.1, 0.2, 0.3], 16_000).unwrap_err();
        assert!(matches!(err, AsrError::BinaryNotFound { .. }));
    }

    #[test]
    fn empty_pcm_yields_no_segments_without_running_anything() {
        let cli = VoxtypeCli::new().with_binary("fc-asr-definitely-not-a-real-binary");
        let segments = cli
            .transcribe(&[], 16_000)
            .expect("empty pcm is not an error");
        assert!(segments.is_empty());
    }

    #[test]
    fn describe_reports_overrides_and_falls_back_to_default() {
        let cli = VoxtypeCli::new()
            .with_engine("whisper")
            .with_model("base.en");
        let info = cli.describe();
        assert_eq!(info.engine, "whisper");
        assert_eq!(info.model, "base.en");
        assert_eq!(info.language, "default");
    }

    /// Covers the timeout path end to end: a child that hangs well past its
    /// budget must be killed, reaped (no zombie left behind), and reported as
    /// `AsrError::Timeout`, all without the caller waiting out the child's
    /// actual (much longer) runtime. Before this test existed, nothing in the
    /// suite exercised `run_with_timeout`'s timeout branch at all.
    #[test]
    fn timeout_kills_and_reaps_a_hanging_child_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_binary(dir.path(), "hang.sh", "#!/bin/sh\nexec sleep 30\n");

        let cli = VoxtypeCli::new()
            .with_binary(script.to_str().expect("utf8 path"))
            .with_timeout(Duration::from_millis(200));

        let start = Instant::now();
        let err = cli
            .transcribe(&[0.1, -0.1, 0.2, -0.2], 16_000)
            .expect_err("a hanging child must be reported as a timeout");
        let elapsed = start.elapsed();

        assert!(matches!(err, AsrError::Timeout { .. }), "got {err:?}");
        // The child sleeps for 30s; if it were not killed and reaped
        // promptly, this call would take that long (or hang forever waiting
        // on a zombie). Bounding well under that proves both happened.
        assert!(
            elapsed < Duration::from_secs(5),
            "transcribe() took {elapsed:?}; the timed-out child was not killed/reaped promptly"
        );
    }

    /// The case a plain `join` on the reader threads turns into an unbounded
    /// hang: the child exits promptly, but a descendant it left behind still
    /// holds the stdout/stderr pipes, so EOF never comes. `--gpu-isolation` and
    /// a wrapper shell script both produce exactly this shape, and a wedge here
    /// wedges the stream thread, so `Session::stop` would never return.
    #[test]
    fn a_descendant_holding_the_pipe_does_not_outlast_the_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_binary(dir.path(), "orphan.sh", "#!/bin/sh\nsleep 6 &\nexit 0\n");

        let cli = VoxtypeCli::new()
            .with_binary(script.to_str().expect("utf8 path"))
            .with_timeout(Duration::from_millis(200));

        let start = Instant::now();
        let err = cli
            .transcribe(&[0.1, -0.1, 0.2, -0.2], 16_000)
            .expect_err("an unread pipe past the budget is a timeout");
        let elapsed = start.elapsed();

        assert!(matches!(err, AsrError::Timeout { .. }), "got {err:?}");
        assert!(
            elapsed < Duration::from_secs(2),
            "transcribe() took {elapsed:?}; it waited on the orphan's pipe \
             instead of abandoning the reader"
        );
    }

    /// A pass runs about once a second for the length of a meeting, so creating
    /// and unlinking a temporary WAV per pass churns gigabytes over three hours.
    #[test]
    fn consecutive_passes_reuse_one_scratch_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("argv.log");
        let script = fake_binary(
            dir.path(),
            "log.sh",
            &format!(
                "#!/bin/sh\n\
                 for a in \"$@\"; do echo \"$a\" >> {log}; done\n\
                 echo 'Loading audio file: \"x.wav\"'\n\
                 echo 'Audio format: 16000 Hz, 1 channel(s), Int'\n\
                 echo 'Processing 4 samples (0.00s)...'\n\
                 echo ''\n\
                 echo 'hello'\n",
                log = log.display()
            ),
        );

        let cli = VoxtypeCli::new().with_binary(script.to_str().expect("utf8 path"));
        for _ in 0..2 {
            cli.transcribe(&[0.1, -0.1, 0.2, -0.2], 16_000)
                .expect("the fake binary produces a parseable transcript");
        }

        let logged = std::fs::read_to_string(&log).expect("read argv log");
        let wavs: Vec<&str> = logged
            .lines()
            .filter(|line| line.ends_with(".wav"))
            .collect();
        assert_eq!(
            wavs.len(),
            2,
            "expected one wav path per pass, got {wavs:?}"
        );
        assert_eq!(wavs[0], wavs[1], "each pass created its own temporary WAV");
    }

    /// Truncation, not just rewinding: a short pass after a long one must not
    /// leave the tail of the long one behind its own header.
    #[test]
    fn a_shorter_pass_does_not_inherit_the_previous_pass_audio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("sizes.log");
        let script = fake_binary(
            dir.path(),
            "size.sh",
            &format!(
                "#!/bin/sh\n\
                 for a in \"$@\"; do case \"$a\" in *.wav) wc -c < \"$a\" >> {log};; esac; done\n\
                 echo 'Loading audio file: \"x.wav\"'\n\
                 echo 'Audio format: 16000 Hz, 1 channel(s), Int'\n\
                 echo 'Processing 4 samples (0.00s)...'\n\
                 echo ''\n\
                 echo 'hello'\n",
                log = log.display()
            ),
        );

        let cli = VoxtypeCli::new().with_binary(script.to_str().expect("utf8 path"));
        cli.transcribe(&vec![0.1_f32; 8_000], 16_000).expect("long");
        cli.transcribe(&vec![0.1_f32; 100], 16_000).expect("short");

        let sizes: Vec<u64> = std::fs::read_to_string(&log)
            .expect("read size log")
            .lines()
            .map(|l| l.trim().parse().expect("numeric size"))
            .collect();
        assert_eq!(sizes.len(), 2);
        assert!(
            sizes[1] < sizes[0],
            "the short pass's WAV was {} bytes after a {} byte pass: the file \
             was not truncated",
            sizes[1],
            sizes[0]
        );
    }

    /// `--model <unknown>` exits 0 and transcribes with voxtype's own default,
    /// so without this the engine recorded with the conversation is false.
    #[test]
    fn an_unknown_model_is_an_error_not_a_silent_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_binary(
            dir.path(),
            "fallback.sh",
            "#!/bin/sh\n\
             echo \"WARN Unknown model 'nope', using default model 'base.en'\" >&2\n\
             echo 'Loading audio file: \"x.wav\"'\n\
             echo 'Audio format: 16000 Hz, 1 channel(s), Int'\n\
             echo 'Processing 4 samples (0.00s)...'\n\
             echo ''\n\
             echo 'transcribed with the wrong model'\n",
        );

        let cli = VoxtypeCli::new()
            .with_binary(script.to_str().expect("utf8 path"))
            .with_model("nope");
        let err = cli
            .transcribe(&[0.1, -0.1], 16_000)
            .expect_err("a silent model fallback must not look like success");
        match err {
            AsrError::UnknownModel { requested } => assert_eq!(requested, "nope"),
            other => panic!("expected UnknownModel, got {other:?}"),
        }
    }

    /// `voxtype -c <missing file>` also exits 0 with its own defaults, which
    /// silently drops the context-window optimisation and remote mode.
    #[test]
    fn a_missing_config_file_fails_before_anything_is_spawned() {
        // The binary name is deliberately absent too: reaching the spawn would
        // report `BinaryNotFound`, so `ConfigMissing` proves the check ran first.
        let cli = VoxtypeCli::new()
            .with_binary("fc-asr-definitely-not-a-real-binary")
            .with_config("/nonexistent/fastcription/voxtype.toml");
        let err = cli.transcribe(&[0.1, 0.2], 16_000).unwrap_err();
        match err {
            AsrError::ConfigMissing(path) => {
                assert_eq!(
                    path,
                    std::path::PathBuf::from("/nonexistent/fastcription/voxtype.toml")
                );
            }
            other => panic!("expected ConfigMissing, got {other:?}"),
        }
    }

    /// Runs the real `voxtype` CLI, which this crate otherwise never touches
    /// in the regular test run. Needs `espeak-ng`, `ffmpeg` and a working
    /// voxtype install with a model already downloaded; `#[ignore]`d so
    /// `cargo test -p fc-asr` stays hermetic. Run explicitly with:
    /// `cargo test -p fc-asr -- --ignored end_to_end_against_real_voxtype`.
    ///
    /// Verified manually against voxtype 1.0.1 / base.en on this machine:
    /// ~0.8s wall time for a ~4.5s generated utterance, transcript correct.
    #[test]
    #[ignore = "requires espeak-ng, ffmpeg and a real voxtype install with a model downloaded"]
    fn end_to_end_against_real_voxtype() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw_wav = dir.path().join("out.wav");
        let sp_wav = dir.path().join("sp.wav");

        let text = "The quarterly roadmap review is scheduled for next Tuesday, \
                     and we still need owners for the migration work.";
        let status = Command::new("espeak-ng")
            .args(["-v", "en-us", "-s", "150", "-w"])
            .arg(&raw_wav)
            .arg(text)
            .status()
            .expect("espeak-ng must be installed for this test");
        assert!(status.success(), "espeak-ng failed to generate speech");

        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(&raw_wav)
            .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
            .arg(&sp_wav)
            .args(["-loglevel", "error"])
            .status()
            .expect("ffmpeg must be installed for this test");
        assert!(status.success(), "ffmpeg failed to resample to 16kHz mono");

        // Round-trip the generated WAV into the same f32 PCM shape the
        // segmenter hands to `Transcriber::transcribe`, to exercise the real
        // adapter path end to end rather than feeding it a WAV directly.
        let mut reader = hound::WavReader::open(&sp_wav).expect("read generated wav");
        let spec = reader.spec();
        assert_eq!(
            spec.sample_rate, 16_000,
            "ffmpeg should have resampled to 16kHz"
        );
        assert_eq!(spec.channels, 1, "ffmpeg should have downmixed to mono");
        let pcm: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.expect("sample") as f32 / i16::MAX as f32)
            .collect();
        assert!(!pcm.is_empty(), "round-tripped PCM must not be empty");

        // Overrides on purpose: a default invocation passes no global flags,
        // so it cannot catch the flag-position mistake this adapter had.
        let cli = VoxtypeCli::new().with_model("base.en").with_language("en");
        let start = Instant::now();
        let segments = cli
            .transcribe(&pcm, 16_000)
            .expect("voxtype transcribe should succeed on generated speech");
        let elapsed = start.elapsed();
        eprintln!(
            "end-to-end: {:.2}s of audio transcribed in {:?}",
            pcm.len() as f64 / 16_000.0,
            elapsed
        );

        assert_eq!(
            segments.len(),
            1,
            "non-empty speech should yield one segment"
        );
        let lower = segments[0].text.to_lowercase();
        assert!(
            lower.contains("roadmap") || lower.contains("quarterly") || lower.contains("migration"),
            "transcript did not contain an expected word: {:?}",
            segments[0].text
        );
    }
    /// voxtype's overrides are global options, so they must appear before the
    /// `transcribe` subcommand. Getting this wrong makes voxtype exit 2 with a
    /// usage message and transcribe nothing, which is why the order is pinned
    /// here rather than left to the reader of `transcribe`.
    #[test]
    fn overrides_precede_the_subcommand() {
        let cli = VoxtypeCli::new()
            .with_config("/tmp/fastcription/voxtype.toml")
            .with_engine("parakeet")
            .with_model("large-v3-turbo")
            .with_language("es")
            .with_threads(8)
            .with_translate(true);
        assert_eq!(
            cli.global_args(),
            vec![
                // `-c` is the one override whose misplacement is invisible:
                // voxtype after the subcommand exits 2, but voxtype with no
                // `-c` transcribes happily with the user's own config, losing
                // the optimisation and remote mode without a word.
                "-q",
                "-c",
                "/tmp/fastcription/voxtype.toml",
                "--engine",
                "parakeet",
                "--model",
                "large-v3-turbo",
                "--language",
                "es",
                "--threads",
                "8",
                "--translate",
            ]
        );
    }

    #[test]
    fn default_invocation_passes_quiet_and_a_bounded_thread_count() {
        let args = VoxtypeCli::new().global_args();
        assert_eq!(args[0], "-q");
        assert_eq!(args[1], "--threads");
        let threads: u32 = args[2].parse().expect("threads value should be numeric");
        assert!((1..=8).contains(&threads), "got {threads}");
        assert_eq!(
            args.len(),
            3,
            "no other override should be present by default"
        );
    }

    #[test]
    fn default_thread_count_never_exceeds_eight() {
        assert!(default_thread_count() >= 1);
        assert!(default_thread_count() <= 8);
    }
}
