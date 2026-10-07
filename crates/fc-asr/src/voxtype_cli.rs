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

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fc_core::{EngineInfo, Segment};

use crate::transcriber::{AsrError, Transcriber};

const LOADING_PREFIX: &str = "Loading audio file:";
const FORMAT_PREFIX: &str = "Audio format:";
const PROCESSING_PREFIX: &str = "Processing ";

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

fn write_wav(path: &Path, pcm: &[f32], sample_rate: u32) -> Result<(), AsrError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
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
    Ok(())
}

/// Runs `child` to completion, reading stdout/stderr on background threads so
/// a chatty process can't deadlock the pipe, and killing it if `timeout`
/// elapses first.
///
/// On timeout the reader threads are deliberately **not** joined. `child.kill()`
/// only terminates the direct child; if it had forked a descendant that
/// inherited our stdout/stderr pipes (the ordinary case for e.g. a wrapper
/// shell script: the shell itself dies, but a command it ran keeps running
/// and keeps the pipe's write end open), the pipe never sees EOF until that
/// orphan exits on its own -- which may be long after `timeout`, or never.
/// Joining here would silently turn a bounded timeout into an unbounded hang,
/// which is worse than the thing it's meant to prevent. The reader threads
/// are abandoned instead: each one still exits and is cleaned up by the OS
/// the moment its pipe actually closes, we just don't wait around for it.
fn run_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, AsrError> {
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Not joined -- see the doc comment above.
                    return Err(AsrError::Timeout { timeout });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Adapter that shells out to `voxtype -q transcribe` for each chunk.
///
/// Never writes to `~/.config/voxtype/config.toml` (ARCHITECTURE.md D6): all
/// overrides go through per-invocation CLI flags, and a `None` override
/// leaves voxtype to use whatever its own config file says.
#[derive(Debug, Clone)]
pub struct VoxtypeCli {
    binary: String,
    engine: Option<String>,
    model: Option<String>,
    language: Option<String>,
    threads: Option<u32>,
    translate: bool,
    /// `None` means "compute a generous default from the chunk's duration"
    /// (see [`TIMEOUT_REALTIME_MULTIPLE`]); `Some` pins an exact timeout,
    /// mainly useful for tests.
    timeout: Option<Duration>,
}

impl Default for VoxtypeCli {
    fn default() -> Self {
        Self {
            binary: "voxtype".to_string(),
            engine: None,
            model: None,
            language: None,
            threads: None,
            translate: false,
            timeout: None,
        }
    }
}

impl VoxtypeCli {
    /// The arguments that precede the `transcribe` subcommand.
    ///
    /// Kept as a pure function so the ordering is testable without spawning a
    /// process: every override voxtype accepts is a global option, and putting
    /// one after the subcommand makes voxtype exit 2 with a usage error instead
    /// of transcribing. That failure mode is silent enough (a usage message on
    /// stderr, no transcript) to be worth a regression test.
    fn global_args(&self) -> Vec<String> {
        let mut args = vec!["-q".to_string()];
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

        let tmp = tempfile::Builder::new()
            .prefix("fc-asr-")
            .suffix(".wav")
            .tempfile()
            .map_err(AsrError::TempFile)?;
        write_wav(tmp.path(), pcm, sample_rate)?;

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
        // `tmp` is dropped (and deleted) here regardless of the outcome
        // below, since nothing after this point holds a borrow of it.
        drop(tmp);

        if !output.status.success() {
            return Err(AsrError::NonZeroExit {
                status: output.status.code().unwrap_or(-1),
                stderr: truncate_for_error(&String::from_utf8_lossy(&output.stderr)),
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
        use std::fs;
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("hang.sh");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").expect("write fake binary");
        let mut perms = fs::metadata(&script).expect("stat script").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("chmod script");

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
            .with_engine("parakeet")
            .with_model("large-v3-turbo")
            .with_language("es")
            .with_threads(8)
            .with_translate(true);
        assert_eq!(
            cli.global_args(),
            vec![
                "-q",
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
    fn default_invocation_passes_only_quiet() {
        assert_eq!(VoxtypeCli::new().global_args(), vec!["-q"]);
    }
}
