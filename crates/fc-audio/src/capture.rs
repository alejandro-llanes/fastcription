//! Capture of one [`AudioSource`] as 16 kHz mono f32 PCM.
//!
//! `parec` is a subprocess today and a trait tomorrow: [`CaptureBackend`] is
//! the seam a native `pipewire-rs` backend would sit behind, without
//! disturbing whatever drives it. A dropped [`ParecCapture`] never leaves an
//! orphan `parec` running — `Drop` calls the same `stop()` the caller would.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use fc_core::{AudioSource, SessionEvent};

use crate::{levels, sources, spectrum};

/// One read's worth of normalized samples, in capture order.
pub type PcmFrame = Vec<f32>;

use crate::SAMPLE_RATE;
const LEVEL_HZ: usize = 20;
const LEVEL_WINDOW_SAMPLES: usize = SAMPLE_RATE / LEVEL_HZ;
const BASE_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(4);
/// No frames for this long is worth telling the user about: a suspended or
/// corked source produces nothing at all, which looks exactly like a quiet room
/// until someone says so. Silence itself still arrives as near-zero frames and
/// keeps the watchdog happy.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(5);
/// A stall this long is not a pause in the conversation, so `parec` is
/// restarted. A corked `--monitor-stream` source — what a paused application
/// leaves behind — never delivers another byte and never closes the pipe
/// either, so `read` would wait on it for the rest of the meeting. Longer than
/// [`WATCHDOG_TIMEOUT`] on purpose: the warning comes first, and a source that
/// recovers on its own in between is not restarted for nothing.
const STALL_RESTART_TIMEOUT: Duration = Duration::from_secs(15);
const STDERR_TAIL_LINES: usize = 8;
/// `i16::MIN.abs()`, not `i16::MAX`: dividing by the max would push the most
/// negative sample just past `-1.0`, outside the contract every downstream
/// consumer assumes.
const I16_FULL_SCALE: f32 = 32_768.0;

/// Something that can capture one [`AudioSource`] and hand back PCM.
///
/// `start` takes `self` by value because starting *is* construction: there is
/// no useful half-started state. It never fails synchronously — a source that
/// cannot be opened is reported as [`SessionEvent::SourceLost`] on `event_tx`
/// and retried, the same as a source that disappears mid-capture, so callers
/// have exactly one failure path to handle instead of two.
pub trait CaptureBackend: Send + 'static {
    fn start(source: AudioSource, pcm_tx: Sender<PcmFrame>, event_tx: Sender<SessionEvent>) -> Self
    where
        Self: Sized;

    /// Stops capture and blocks until the capture thread has exited. Never
    /// hangs: a running child is killed, not waited on to finish by itself.
    fn stop(&mut self);
}

/// Captures via `parec --raw --format=s16le --rate=16000 --channels=1`.
pub struct ParecCapture {
    stop_flag: Arc<AtomicBool>,
    current_child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureBackend for ParecCapture {
    fn start(
        source: AudioSource,
        pcm_tx: Sender<PcmFrame>,
        event_tx: Sender<SessionEvent>,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let current_child = Arc::new(Mutex::new(None));
        let thread = {
            let stop_flag = stop_flag.clone();
            let current_child = current_child.clone();
            thread::Builder::new()
                .name("fc-audio-capture".into())
                .spawn(move || {
                    run_capture_loop(
                        source,
                        CaptureSpec::default(),
                        pcm_tx,
                        event_tx,
                        stop_flag,
                        current_child,
                    )
                })
                .expect("spawning the capture thread")
        };
        Self {
            stop_flag,
            current_child,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(mut child) = take_child(&self.current_child) {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ParecCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn take_child(current_child: &Arc<Mutex<Option<Child>>>) -> Option<Child> {
    current_child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Sleeps for `duration`, checking `stop_flag` often enough that `stop()`
/// never waits out a multi-second backoff.
fn sleep_cancellably(stop_flag: &AtomicBool, duration: Duration) {
    const STEP: Duration = Duration::from_millis(50);
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        let step = remaining.min(STEP);
        thread::sleep(step);
        remaining -= step;
    }
}

enum SessionEnd {
    /// The caller asked to stop, or the PCM receiver went away.
    StopRequested,
    /// The child exited or could not be started; capture will retry.
    ChildExited(String),
}

/// The parts of capture a test replaces: which binary produces the PCM, how a
/// stored descriptor becomes a live source, and how patient the watchdog is.
///
/// Capture is otherwise untestable on a machine with no recordable device — the
/// state this one is in (ARCHITECTURE.md §5) — and the paths worth testing here
/// are exactly the ones that need a source that misbehaves on cue.
struct CaptureSpec {
    binary: String,
    resolve: Resolver,
    stall_warn: Duration,
    stall_restart: Duration,
}

/// Turns a stored descriptor into a live source. [`sources::resolve`] in
/// production; a fake in tests.
type Resolver = Box<dyn Fn(&AudioSource) -> crate::Result<AudioSource> + Send>;

impl Default for CaptureSpec {
    fn default() -> Self {
        Self {
            binary: "parec".to_string(),
            resolve: Box::new(sources::resolve),
            stall_warn: WATCHDOG_TIMEOUT,
            stall_restart: STALL_RESTART_TIMEOUT,
        }
    }
}

fn run_capture_loop(
    descriptor: AudioSource,
    spec: CaptureSpec,
    pcm_tx: Sender<PcmFrame>,
    event_tx: Sender<SessionEvent>,
    stop_flag: Arc<AtomicBool>,
    current_child: Arc<Mutex<Option<Child>>>,
) {
    let mut backoff = BASE_BACKOFF;
    let mut recovering = false;

    while !stop_flag.load(Ordering::SeqCst) {
        let resolved = match (spec.resolve)(&descriptor) {
            Ok(s) => s,
            Err(e) => {
                // Covers `AudioError::Ambiguous` as much as a source that is
                // gone: when two live streams fit the stored descriptor there is
                // nothing to choose between them, so the reconnect reports which
                // ones they are and keeps retrying rather than recording a
                // coin-flip for the rest of the meeting.
                let _ = event_tx.send(SessionEvent::SourceLost {
                    reason: e.to_string(),
                });
                recovering = true;
                sleep_cancellably(&stop_flag, backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let end = run_one_session(
            &resolved,
            &spec,
            &pcm_tx,
            &event_tx,
            &stop_flag,
            &current_child,
            &mut recovering,
            &mut backoff,
        );

        match end {
            SessionEnd::StopRequested => break,
            SessionEnd::ChildExited(reason) => {
                let _ = event_tx.send(SessionEvent::SourceLost { reason });
                recovering = true;
                sleep_cancellably(&stop_flag, backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_one_session(
    source: &AudioSource,
    spec: &CaptureSpec,
    pcm_tx: &Sender<PcmFrame>,
    event_tx: &Sender<SessionEvent>,
    stop_flag: &Arc<AtomicBool>,
    current_child: &Arc<Mutex<Option<Child>>>,
    recovering: &mut bool,
    backoff: &mut Duration,
) -> SessionEnd {
    let Some(mut args) = source.parec_target() else {
        return SessionEnd::ChildExited(format!(
            "'{}' has no capture target (missing name or index)",
            source.label()
        ));
    };
    args.extend([
        "--raw".to_string(),
        "--format=s16le".to_string(),
        "--rate=16000".to_string(),
        "--channels=1".to_string(),
    ]);

    let mut command = Command::new(&spec.binary);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return SessionEnd::ChildExited(format!("failed to start parec: {e}")),
    };

    let mut stdout = child.stdout.take().expect("parec stdout is piped");
    let stderr = child.stderr.take().expect("parec stderr is piped");
    let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let stderr_thread = spawn_stderr_reader(stderr, stderr_tail.clone());

    *current_child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(child);

    let watchdog = Watchdog::spawn(
        event_tx.clone(),
        source.label(),
        Arc::clone(current_child),
        spec.stall_warn,
        spec.stall_restart,
    );
    let mut leftover: Option<u8> = None;
    let mut level_window: Vec<f32> = Vec::with_capacity(LEVEL_WINDOW_SAMPLES * 2);
    // Allocated once, outside the read loop: this is the audio path.
    let mut analyzer = spectrum::Analyzer::new();
    let mut buf = [0u8; 4096];
    let mut pcm_receiver_gone = false;

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        let n = match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let samples = i16le_bytes_to_f32(&buf[..n], &mut leftover);
        if samples.is_empty() {
            continue;
        }

        if watchdog.touch() {
            // Audio is flowing again after a stall. The stall was reported as a
            // failure, and nothing used to follow it: the user was left looking
            // at "no audio from ..." with a working capture behind it.
            let _ = event_tx.send(SessionEvent::SourceRecovered);
        }
        if *recovering {
            let _ = event_tx.send(SessionEvent::SourceRecovered);
            *recovering = false;
        }
        *backoff = BASE_BACKOFF;

        level_window.extend_from_slice(&samples);
        while level_window.len() >= LEVEL_WINDOW_SAMPLES {
            let (peak, rms) = {
                let window = &level_window[..LEVEL_WINDOW_SAMPLES];
                // The analyser keeps its own rolling history, so it is given
                // the whole block and transforms the newest 512 samples of it.
                analyzer.push(window);
                (levels::peak(window), levels::rms(window))
            };
            let _ = event_tx.send(SessionEvent::Level {
                peak,
                rms,
                bands: analyzer.bands(),
            });
            level_window.drain(..LEVEL_WINDOW_SAMPLES);
        }

        if pcm_tx.send(samples).is_err() {
            pcm_receiver_gone = true;
            break;
        }
    }

    let restarted_for_a_stall = watchdog.join();

    // Unconditional: covers the child still running (stop_flag / receiver
    // gone) and the child having already exited on its own (kill is then a
    // harmless no-op).
    let status = take_child(current_child).and_then(|mut c| {
        let _ = c.kill();
        c.wait().ok()
    });
    let _ = stderr_thread.join();

    if pcm_receiver_gone || stop_flag.load(Ordering::SeqCst) {
        return SessionEnd::StopRequested;
    }

    let tail = stderr_tail
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tail_text = tail.iter().cloned().collect::<Vec<_>>().join("; ");
    if restarted_for_a_stall {
        // The watchdog ended this child, so neither its exit status nor its
        // stderr says anything useful about why.
        return SessionEnd::ChildExited(format!(
            "no audio from {} for {:?}; restarting the capture",
            source.label(),
            spec.stall_restart
        ));
    }
    let reason = match (status, tail_text.is_empty()) {
        (Some(s), true) => format!("parec exited with {s}"),
        (Some(s), false) => format!("parec exited with {s}: {tail_text}"),
        (None, true) => "parec stopped unexpectedly".to_string(),
        (None, false) => format!("parec stopped unexpectedly: {tail_text}"),
    };
    SessionEnd::ChildExited(reason)
}

fn spawn_stderr_reader(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(std::result::Result::ok) {
            let mut guard = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.len() >= STDERR_TAIL_LINES {
                guard.pop_front();
            }
            guard.push_back(line);
        }
    })
}

/// Reports "no frames at all", which a suspended source produces, distinctly
/// from silence — a stream of near-zero frames still touches the watchdog.
///
/// It also ends a capture that has stalled past `restart_after`, because the
/// read loop cannot: a corked source leaves `read` blocked with no data and no
/// EOF, so killing the child is the only way to get back to the reconnect path.
struct Watchdog {
    last_frame: Arc<Mutex<Instant>>,
    /// Set when the stall was reported, cleared by the first frame after it, so
    /// the reader knows to announce the recovery.
    stalled: Arc<AtomicBool>,
    /// Set when this watchdog ended the capture, so the session reports the
    /// stall rather than the kill it caused.
    restarted: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Watchdog {
    fn spawn(
        event_tx: Sender<SessionEvent>,
        label: String,
        current_child: Arc<Mutex<Option<Child>>>,
        warn_after: Duration,
        restart_after: Duration,
    ) -> Self {
        let last_frame = Arc::new(Mutex::new(Instant::now()));
        let stalled = Arc::new(AtomicBool::new(false));
        let restarted = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        // Fine enough to notice a stall near its threshold, coarse enough to
        // cost nothing: a 5s warning is polled about four times a second.
        let poll = (warn_after / 4).clamp(Duration::from_millis(10), Duration::from_millis(500));
        let handle = {
            let last_frame = last_frame.clone();
            let stalled = stalled.clone();
            let restarted = restarted.clone();
            let done = done.clone();
            thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    thread::sleep(poll);
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    let elapsed = last_frame
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .elapsed();
                    if elapsed >= restart_after {
                        restarted.store(true, Ordering::SeqCst);
                        if let Some(mut child) = take_child(&current_child) {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        break;
                    }
                    if elapsed >= warn_after && !stalled.swap(true, Ordering::SeqCst) {
                        let _ = event_tx.send(SessionEvent::Failed {
                            stage: "capture",
                            message: format!(
                                "no audio received from {label} for {warn_after:?}; \
                                 it may be suspended"
                            ),
                        });
                    }
                }
            })
        };
        Self {
            last_frame,
            stalled,
            restarted,
            done,
            handle,
        }
    }

    /// Records a frame. `true` means this frame ended a reported stall, and the
    /// caller owes the user a [`SessionEvent::SourceRecovered`].
    fn touch(&self) -> bool {
        *self
            .last_frame
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
        self.stalled.swap(false, Ordering::SeqCst)
    }

    /// Stops the watchdog, reporting whether it had already ended the capture.
    fn join(self) -> bool {
        self.done.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
        self.restarted.load(Ordering::SeqCst)
    }
}

/// Converts raw little-endian 16-bit PCM to samples in `-1.0..=1.0`.
///
/// `leftover` carries a single byte across calls when a read ends in the
/// middle of a sample: a 2-byte sample straddling a read boundary is routine
/// (stdout reads do not respect sample alignment), and losing that byte would
/// shift every sample after it by one, turning the rest of the stream to
/// noise. It is restored at the front of the next call before anything else
/// is decoded.
fn i16le_bytes_to_f32(bytes: &[u8], leftover: &mut Option<u8>) -> PcmFrame {
    let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut iter = bytes.iter().copied();
    let mut pending = leftover.take();

    loop {
        let lo = match pending.take() {
            Some(b) => b,
            None => match iter.next() {
                Some(b) => b,
                None => break,
            },
        };
        let hi = match iter.next() {
            Some(b) => b,
            None => {
                *leftover = Some(lo);
                break;
            }
        };
        let sample = i16::from_le_bytes([lo, hi]);
        samples.push(sample as f32 / I16_FULL_SCALE);
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{unbounded, Receiver};
    use fc_core::SourceKind;

    fn fake_binary(dir: &std::path::Path, name: &str, body: &str) -> String {
        crate::bounded::fake_binary(dir, name, body)
            .to_str()
            .expect("utf8 path")
            .to_string()
    }

    fn a_source() -> AudioSource {
        AudioSource::named(SourceKind::SinkMonitor, "sink.monitor", "A sink")
    }

    /// Runs the capture loop on its own thread with `spec`, collecting events
    /// until `done` is satisfied or the deadline passes, then stops it.
    fn drive<F>(spec: CaptureSpec, timeout: Duration, mut done: F) -> Vec<SessionEvent>
    where
        F: FnMut(&[SessionEvent]) -> bool,
    {
        let (pcm_tx, pcm_rx) = unbounded::<PcmFrame>();
        let (event_tx, event_rx) = unbounded::<SessionEvent>();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let current_child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

        let thread = {
            let stop_flag = Arc::clone(&stop_flag);
            let current_child = Arc::clone(&current_child);
            thread::spawn(move || {
                run_capture_loop(a_source(), spec, pcm_tx, event_tx, stop_flag, current_child)
            })
        };

        let events = collect_until(&event_rx, timeout, &mut done);

        stop_flag.store(true, Ordering::SeqCst);
        if let Some(mut child) = take_child(&current_child) {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The PCM receiver stays alive until the thread is joined: dropping it
        // early would end the loop as "stop requested" and hide what it did.
        let _ = thread.join();
        drop(pcm_rx);
        events
    }

    fn collect_until<F>(
        events: &Receiver<SessionEvent>,
        timeout: Duration,
        done: &mut F,
    ) -> Vec<SessionEvent>
    where
        F: FnMut(&[SessionEvent]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
                seen.push(event);
                if done(&seen) {
                    break;
                }
            }
        }
        seen
    }

    /// A source that goes quiet and comes back: the watchdog reports the stall,
    /// and nothing used to follow it, so the user was left looking at a failure
    /// with a working capture behind it.
    #[test]
    fn a_source_that_resumes_after_a_stall_reports_the_recovery() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 320 samples, a pause well past the warning threshold, then more. Each
        // `head` is its own process, so each write reaches the pipe as it ends.
        let producer = fake_binary(
            dir.path(),
            "stalling-parec",
            // `exec` on the last line matters: a plain `sleep` would be a child of
            // this shell, and killing the shell would leave it holding the
            // stdout pipe, so the capture's `read` would never see EOF. Real
            // `parec` has no children.
            "#!/bin/sh\nhead -c 640 /dev/zero\nsleep 0.5\nhead -c 640 /dev/zero\nexec sleep 10\n",
        );
        let spec = CaptureSpec {
            binary: producer,
            resolve: Box::new(|source: &AudioSource| Ok(source.clone())),
            stall_warn: Duration::from_millis(150),
            // Long enough that this test is about the recovery, not the restart.
            stall_restart: Duration::from_secs(30),
        };

        let events = drive(spec, Duration::from_secs(5), |seen| {
            seen.iter().any(|e| {
                matches!(
                    e,
                    SessionEvent::Failed {
                        stage: "capture",
                        ..
                    }
                )
            }) && seen
                .iter()
                .any(|e| matches!(e, SessionEvent::SourceRecovered))
        });

        let stall_at = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    SessionEvent::Failed {
                        stage: "capture",
                        ..
                    }
                )
            })
            .unwrap_or_else(|| panic!("expected the stall to be reported, saw {events:?}"));
        let recovered_at = events
            .iter()
            .position(|e| matches!(e, SessionEvent::SourceRecovered))
            .unwrap_or_else(|| panic!("expected a recovery after the stall, saw {events:?}"));
        assert!(
            recovered_at > stall_at,
            "the recovery must follow the stall, saw {events:?}"
        );
    }

    /// A stall that never ends: a corked `--monitor-stream` source delivers no
    /// data and no EOF, so `read` blocks for ever unless something ends the
    /// child. The capture then reconnects, which is the only way back.
    #[test]
    fn a_stall_past_the_restart_threshold_restarts_the_capture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let producer = fake_binary(
            dir.path(),
            "corked-parec",
            "#!/bin/sh\nhead -c 640 /dev/zero\nexec sleep 10\n",
        );
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let spec = CaptureSpec {
            binary: producer,
            resolve: Box::new(move |source: &AudioSource| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(source.clone())
            }),
            stall_warn: Duration::from_millis(100),
            stall_restart: Duration::from_millis(300),
        };

        let seen_attempts = Arc::clone(&attempts);
        let events = drive(spec, Duration::from_secs(5), |seen| {
            seen.iter().any(
                |e| matches!(e, SessionEvent::SourceLost { reason } if reason.contains("restarting")),
            ) && seen_attempts.load(Ordering::SeqCst) >= 2
        });

        assert!(
            events.iter().any(
                |e| matches!(e, SessionEvent::SourceLost { reason } if reason.contains("restarting")),
            ),
            "a stall past the threshold must end the capture and say so, saw {events:?}"
        );
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "the capture must be started again after the restart, saw {} attempts",
            attempts.load(Ordering::SeqCst)
        );
    }

    /// Two live streams fit the stored descriptor. Recording either one is a
    /// coin flip on half a meeting, so the reconnect names them and waits.
    #[test]
    fn an_ambiguous_reconnect_reports_and_keeps_retrying() {
        let spec = CaptureSpec {
            // Never reached: resolution fails before anything is spawned.
            binary: "fc-audio-definitely-not-a-real-binary".to_string(),
            resolve: Box::new(|_source: &AudioSource| {
                Err(crate::AudioError::Ambiguous {
                    looked_for: "application stream 'Zoom'".to_string(),
                    candidates: vec!["Zoom — Call (#930)".into(), "Zoom — Share (#931)".into()],
                })
            }),
            ..CaptureSpec::default()
        };

        let events = drive(spec, Duration::from_secs(3), |seen| {
            seen.iter()
                .filter(|e| matches!(e, SessionEvent::SourceLost { .. }))
                .count()
                >= 2
        });

        let reasons: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::SourceLost { reason } => Some(reason),
                _ => None,
            })
            .collect();
        assert!(
            reasons.len() >= 2,
            "an ambiguous source must keep being retried, saw {events:?}"
        );
        assert!(
            reasons[0].contains("#930") && reasons[0].contains("#931"),
            "the candidates must reach the user: {}",
            reasons[0]
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::SourceRecovered)),
            "nothing may be recorded while the source is ambiguous: {events:?}"
        );
    }

    #[test]
    fn even_byte_count_decodes_cleanly() {
        let mut leftover = None;
        // -1.0, 0.0, just-under-1.0
        let bytes = [0x00, 0x80, 0x00, 0x00, 0xFF, 0x7F];
        let samples = i16le_bytes_to_f32(&bytes, &mut leftover);
        assert_eq!(samples.len(), 3);
        assert!((samples[0] - (-1.0)).abs() < 1e-6);
        assert_eq!(samples[1], 0.0);
        assert!((samples[2] - 0.999969).abs() < 1e-5);
        assert_eq!(leftover, None);
    }

    #[test]
    fn samples_never_exceed_the_unit_range() {
        let mut leftover = None;
        let bytes = [0x00, 0x80]; // i16::MIN
        let samples = i16le_bytes_to_f32(&bytes, &mut leftover);
        assert_eq!(samples[0], -1.0);
        assert!(samples[0] >= -1.0 && samples[0] <= 1.0);
    }

    #[test]
    fn a_sample_straddling_two_reads_is_not_corrupted() {
        // One sample (0x1234) split as [0x34] | [0x12], across two calls.
        let mut leftover = None;
        let first = i16le_bytes_to_f32(&[0x34], &mut leftover);
        assert!(first.is_empty());
        assert_eq!(leftover, Some(0x34));

        let second = i16le_bytes_to_f32(&[0x12], &mut leftover);
        assert_eq!(leftover, None);
        assert_eq!(second.len(), 1);
        let expected = i16::from_le_bytes([0x34, 0x12]) as f32 / I16_FULL_SCALE;
        assert_eq!(second[0], expected);
    }

    #[test]
    fn multiple_straddles_across_many_small_reads_stay_aligned() {
        // Five whole samples, fed one byte at a time, must decode to exactly
        // five samples with no left-over byte at the end.
        let source: [i16; 5] = [100, -100, 32767, -32768, 0];
        let mut bytes = Vec::new();
        for s in source {
            bytes.extend_from_slice(&s.to_le_bytes());
        }

        let mut leftover = None;
        let mut decoded = Vec::new();
        for byte in bytes {
            decoded.extend(i16le_bytes_to_f32(&[byte], &mut leftover));
        }

        assert_eq!(leftover, None);
        assert_eq!(decoded.len(), source.len());
        for (d, s) in decoded.iter().zip(source.iter()) {
            assert_eq!(*d, *s as f32 / I16_FULL_SCALE);
        }
    }

    #[test]
    fn empty_input_leaves_a_lone_leftover_byte_untouched() {
        let mut leftover = Some(0x42);
        let samples = i16le_bytes_to_f32(&[], &mut leftover);
        assert!(samples.is_empty());
        assert_eq!(leftover, Some(0x42));
    }
}
