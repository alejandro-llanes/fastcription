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

use crate::{levels, sources};

/// One read's worth of normalized samples, in capture order.
pub type PcmFrame = Vec<f32>;

const SAMPLE_RATE: usize = 16_000;
const LEVEL_HZ: usize = 20;
const LEVEL_WINDOW_SAMPLES: usize = SAMPLE_RATE / LEVEL_HZ;
const BASE_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(4);
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(5);
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
                .spawn(move || run_capture_loop(source, pcm_tx, event_tx, stop_flag, current_child))
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

fn run_capture_loop(
    descriptor: AudioSource,
    pcm_tx: Sender<PcmFrame>,
    event_tx: Sender<SessionEvent>,
    stop_flag: Arc<AtomicBool>,
    current_child: Arc<Mutex<Option<Child>>>,
) {
    let mut backoff = BASE_BACKOFF;
    let mut recovering = false;

    while !stop_flag.load(Ordering::SeqCst) {
        let resolved = match sources::resolve(&descriptor) {
            Ok(s) => s,
            Err(e) => {
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

    let mut command = Command::new("parec");
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

    let watchdog = Watchdog::spawn(event_tx.clone(), source.label());
    let mut leftover: Option<u8> = None;
    let mut level_window: Vec<f32> = Vec::with_capacity(LEVEL_WINDOW_SAMPLES * 2);
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

        watchdog.touch();
        if *recovering {
            let _ = event_tx.send(SessionEvent::SourceRecovered);
            *recovering = false;
        }
        *backoff = BASE_BACKOFF;

        level_window.extend_from_slice(&samples);
        while level_window.len() >= LEVEL_WINDOW_SAMPLES {
            let (peak, rms) = {
                let window = &level_window[..LEVEL_WINDOW_SAMPLES];
                (levels::peak(window), levels::rms(window))
            };
            let _ = event_tx.send(SessionEvent::Level { peak, rms });
            level_window.drain(..LEVEL_WINDOW_SAMPLES);
        }

        if pcm_tx.send(samples).is_err() {
            pcm_receiver_gone = true;
            break;
        }
    }

    watchdog.join();

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
struct Watchdog {
    last_frame: Arc<Mutex<Instant>>,
    done: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Watchdog {
    fn spawn(event_tx: Sender<SessionEvent>, label: String) -> Self {
        let last_frame = Arc::new(Mutex::new(Instant::now()));
        let done = Arc::new(AtomicBool::new(false));
        let handle = {
            let last_frame = last_frame.clone();
            let done = done.clone();
            thread::spawn(move || {
                let mut stalled = false;
                while !done.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(500));
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    let elapsed = last_frame
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .elapsed();
                    if elapsed >= WATCHDOG_TIMEOUT {
                        if !stalled {
                            stalled = true;
                            let _ = event_tx.send(SessionEvent::Failed {
                                stage: "capture",
                                message: format!(
                                    "no audio received from {label} for {}s; it may be suspended",
                                    WATCHDOG_TIMEOUT.as_secs()
                                ),
                            });
                        }
                    } else {
                        stalled = false;
                    }
                }
            })
        };
        Self {
            last_frame,
            done,
            handle,
        }
    }

    fn touch(&self) {
        *self
            .last_frame
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    fn join(self) {
        self.done.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
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
