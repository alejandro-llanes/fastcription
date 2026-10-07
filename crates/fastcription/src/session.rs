//! The session supervisor: the only place that knows how capture, segmentation,
//! transcription and storage fit together.
//!
//! One [`Session`] records one conversation. Per track it runs three things: a
//! capture backend producing PCM, a pipeline thread turning PCM into chunks,
//! and an ASR worker turning chunks into stored segments. The UI never touches
//! any of them — it reads [`fc_core::SessionEvent`]s off one channel and calls
//! [`Session::pause`], [`Session::resume`] and [`Session::stop`].
//!
//! ## Why audio is never dropped
//!
//! The PCM channel is unbounded and the chunk channel holds one. When
//! transcription falls behind, the pipeline thread blocks handing over a chunk,
//! so captured audio accumulates in the PCM channel instead of being discarded
//! — 16 kHz mono f32 costs 64 KB per second of lag, which is cheap next to
//! losing part of a meeting. Blocking is also the signal to grow the chunk
//! length, which is what actually lets the backlog drain.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TrySendError};
use fc_asr::{stamp_segment, Chunk, Segmenter, SegmenterConfig, Transcriber};
use fc_audio::{CaptureBackend, ParecCapture, PcmFrame};
use fc_core::{
    AudioSource, ConversationId, ConversationStatus, EngineInfo, GroupId, Pressure, Segment,
    SessionEvent, SessionState, Track, UnixMillis,
};
use fc_store::{NewConversation, Store};

/// The store is shared by both ASR workers and the UI. `Store` wraps a rusqlite
/// `Connection`, which is `Send` but not `Sync`, so it lives behind a mutex
/// rather than being handed out per thread: appends happen a few times per
/// chunk, so contention is irrelevant and a second connection would only invite
/// `SQLITE_BUSY`.
pub type SharedStore = Arc<Mutex<Store>>;

/// A running capture that can be told to stop.
///
/// [`fc_audio::CaptureBackend`] starts through an associated function
/// returning `Self`, which cannot be called through a trait object. This
/// narrower trait is what the supervisor actually needs, and having it means
/// the whole pipeline can be driven by a fake capture in tests — on a machine
/// with no working audio device, that is the difference between this file
/// being tested and not.
pub trait CaptureHandle: Send {
    fn stop(&mut self);
}

impl CaptureHandle for ParecCapture {
    fn stop(&mut self) {
        CaptureBackend::stop(self);
    }
}

/// Starts capture of a source, delivering PCM on one channel and events on the
/// other.
pub type CaptureFactory =
    Box<dyn Fn(AudioSource, Sender<PcmFrame>, Sender<SessionEvent>) -> Box<dyn CaptureHandle>>;

/// The real capture: `parec` on the chosen PipeWire source.
pub fn parec_captures() -> CaptureFactory {
    Box::new(|source, pcm_tx, event_tx| {
        Box::new(ParecCapture::start(source, pcm_tx, event_tx))
    })
}

/// Builds the transcriber for a track. A factory rather than one shared
/// instance because each ASR worker owns its own, and because the live-feed
/// backend (architecture §8) will want per-track construction.
pub type TranscriberFactory = Box<dyn Fn(Track) -> Box<dyn Transcriber> + Send + Sync>;

pub struct SessionConfig {
    pub title: String,
    pub group: Option<GroupId>,
    /// The source the user picked. Already resolved: [`Session::start`] does not
    /// re-resolve, because a source that has gone away is a question for the
    /// user, not something to guess at here.
    pub source: AudioSource,
    /// The microphone, when the user opted into a second track (decision D2).
    pub mic_source: Option<AudioSource>,
    pub segmenter: SegmenterConfig,
    /// Recorded with the conversation so an old transcript can be read in the
    /// light of how it was made. Composed by the caller from
    /// `fc_voxtype::cli::status` and the user's config, since the CLI adapter
    /// cannot know what voxtype will pick for itself.
    pub engine: EngineInfo,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("could not create the conversation: {0}")]
    Store(#[from] fc_store::StoreError),
    #[error("the selected source has no capture target: {0}")]
    UnusableSource(String),
}

/// A recording in progress.
pub struct Session {
    pub conversation: ConversationId,
    /// Everything the pipeline reports. The UI drains this each frame.
    pub events: Receiver<SessionEvent>,
    event_tx: Sender<SessionEvent>,
    paused: Arc<AtomicBool>,
    tracks: Vec<TrackRuntime>,
    store: SharedStore,
}

struct TrackRuntime {
    capture: Box<dyn CaptureHandle>,
    pipeline: JoinHandle<()>,
    worker: JoinHandle<()>,
}

impl Session {
    /// Creates the conversation row, then starts a capture, a pipeline and an
    /// ASR worker per track. Returns as soon as the threads are running; the
    /// first segment arrives on `events` a few seconds later.
    pub fn start(
        store: SharedStore,
        cfg: SessionConfig,
        transcriber: TranscriberFactory,
        captures: &CaptureFactory,
        now: UnixMillis,
    ) -> Result<Self, SessionError> {
        if cfg.source.parec_target().is_none() {
            return Err(SessionError::UnusableSource(cfg.source.label()));
        }

        let conversation = store.lock().expect("store mutex").create_conversation(
            &NewConversation {
                title: cfg.title.clone(),
                group: cfg.group,
                started_at: now,
                source: cfg.source.clone(),
                mic_track: cfg.mic_source.is_some(),
                engine: cfg.engine.clone(),
                voxtype_meeting_id: None,
            },
        )?;

        let (event_tx, events) = unbounded::<SessionEvent>();
        let paused = Arc::new(AtomicBool::new(false));
        let transcriber = Arc::new(transcriber);

        let mut tracks = Vec::new();
        tracks.push(spawn_track(
            Track::Selected,
            cfg.source.clone(),
            cfg.segmenter.clone(),
            conversation,
            Arc::clone(&store),
            event_tx.clone(),
            Arc::clone(&paused),
            Arc::clone(&transcriber),
            captures,
            true,
        ));
        if let Some(mic) = cfg.mic_source.clone() {
            tracks.push(spawn_track(
                Track::Microphone,
                mic,
                cfg.segmenter.clone(),
                conversation,
                Arc::clone(&store),
                event_tx.clone(),
                Arc::clone(&paused),
                Arc::clone(&transcriber),
                captures,
                // Only the selected track reports levels: two tracks driving one
                // meter would make it jitter between unrelated signals.
                false,
            ));
        }

        let _ = event_tx.send(SessionEvent::StateChanged(SessionState::Recording));

        Ok(Self {
            conversation,
            events,
            event_tx,
            paused,
            tracks,
            store,
        })
    }

    /// Audio arriving while paused is discarded, matching what voxtype's own
    /// meeting pause does. The consequence is that segment timestamps close the
    /// gap rather than preserving it: a transcript measures speech, not the
    /// wall clock.
    pub fn pause(&self) {
        if !self.paused.swap(true, Ordering::SeqCst) {
            let _ = self
                .event_tx
                .send(SessionEvent::StateChanged(SessionState::Paused));
        }
    }

    pub fn resume(&self) {
        if self.paused.swap(false, Ordering::SeqCst) {
            let _ = self
                .event_tx
                .send(SessionEvent::StateChanged(SessionState::Recording));
        }
    }

    /// Stops capture, then waits for the audio already recorded to finish
    /// transcribing before marking the conversation complete.
    ///
    /// Shutdown runs on channel disconnection rather than a stop flag: stopping
    /// a capture ends its thread, which drops the PCM sender, which lets the
    /// pipeline thread drain what is left and flush its tail, which drops the
    /// chunk sender, which lets the ASR worker finish the backlog and exit. No
    /// step can skip the audio still in flight.
    /// Stops without blocking the caller.
    ///
    /// Finishing a session waits for the audio already captured to be
    /// transcribed, which takes as long as the backlog does. Doing that on the
    /// interface thread would freeze the window at exactly the moment the user
    /// is watching for their last words, so the wait happens on its own thread
    /// and progress arrives as events: `Finishing` immediately, `Idle` once the
    /// conversation is closed.
    pub fn stop_async(self, now: UnixMillis) {
        let events = self.event_tx.clone();
        let spawned = thread::Builder::new()
            .name("fc-session-stop".into())
            .spawn(move || {
                if let Err(err) = self.stop(now) {
                    let _ = events.send(SessionEvent::Failed {
                        stage: "finish",
                        message: err.to_string(),
                    });
                }
            });
        if let Err(err) = spawned {
            tracing::error!(%err, "could not spawn the session shutdown thread");
        }
    }

    pub fn stop(mut self, now: UnixMillis) -> Result<(), SessionError> {
        let _ = self
            .event_tx
            .send(SessionEvent::StateChanged(SessionState::Finishing));

        for track in &mut self.tracks {
            track.capture.stop();
        }
        for track in self.tracks.drain(..) {
            let _ = track.pipeline.join();
            let _ = track.worker.join();
        }

        self.store
            .lock()
            .expect("store mutex")
            .finish_conversation(self.conversation, now, ConversationStatus::Completed)?;

        let _ = self
            .event_tx
            .send(SessionEvent::StateChanged(SessionState::Idle));
        Ok(())
    }

}

#[allow(clippy::too_many_arguments)]
fn spawn_track(
    track: Track,
    source: AudioSource,
    cfg: SegmenterConfig,
    conversation: ConversationId,
    store: SharedStore,
    event_tx: Sender<SessionEvent>,
    paused: Arc<AtomicBool>,
    transcriber: Arc<TranscriberFactory>,
    captures: &CaptureFactory,
    report_levels: bool,
) -> TrackRuntime {
    let (pcm_tx, pcm_rx) = unbounded::<PcmFrame>();
    // One slot: enough to keep the worker fed without letting a backlog build
    // where it cannot be seen. Fullness is the backpressure signal.
    let (chunk_tx, chunk_rx) = bounded::<Chunk>(1);

    // A capture that reports levels needs the real event channel; the mic track
    // gets a sink that only carries failures, so it cannot fight for the meter.
    let capture_events = if report_levels {
        event_tx.clone()
    } else {
        level_filtered(event_tx.clone())
    };
    let capture = captures(source, pcm_tx, capture_events);

    let pipeline = {
        let event_tx = event_tx.clone();
        thread::Builder::new()
            .name(format!("fc-segment-{}", track.as_str()))
            .spawn(move || run_pipeline(track, cfg, pcm_rx, chunk_tx, paused, event_tx))
            .expect("spawn segmenter thread")
    };

    let worker = thread::Builder::new()
        .name(format!("fc-asr-{}", track.as_str()))
        .spawn(move || {
            let engine = transcriber(track);
            run_worker(track, conversation, store, chunk_rx, event_tx, engine)
        })
        .expect("spawn asr thread");

    TrackRuntime {
        capture,
        pipeline,
        worker,
    }
}

/// Drops `Level` events, forwarding everything else. Used for the microphone
/// track so only the selected source drives the meter.
fn level_filtered(downstream: Sender<SessionEvent>) -> Sender<SessionEvent> {
    let (tx, rx) = unbounded::<SessionEvent>();
    thread::Builder::new()
        .name("fc-level-filter".into())
        .spawn(move || {
            for event in rx {
                if matches!(event, SessionEvent::Level { .. }) {
                    continue;
                }
                if downstream.send(event).is_err() {
                    break;
                }
            }
        })
        .expect("spawn level filter");
    tx
}

fn run_pipeline(
    track: Track,
    cfg: SegmenterConfig,
    pcm_rx: Receiver<PcmFrame>,
    chunk_tx: Sender<Chunk>,
    paused: Arc<AtomicBool>,
    event_tx: Sender<SessionEvent>,
) {
    let mut segmenter = Segmenter::new(track, cfg);
    let mut lagging = false;

    for frame in &pcm_rx {
        if paused.load(Ordering::SeqCst) {
            continue;
        }
        for chunk in segmenter.push(&frame) {
            if !hand_over(chunk, &chunk_tx, &mut segmenter, &mut lagging, &event_tx) {
                return;
            }
        }
    }

    // Capture has ended. Whatever is buffered is still the user's audio.
    for chunk in segmenter.flush() {
        if !hand_over(chunk, &chunk_tx, &mut segmenter, &mut lagging, &event_tx) {
            return;
        }
    }
}

/// Hands a chunk to the ASR worker, blocking if it is busy. Returns false when
/// the worker is gone and the pipeline should stop.
fn hand_over(
    chunk: Chunk,
    chunk_tx: &Sender<Chunk>,
    segmenter: &mut Segmenter,
    lagging: &mut bool,
    event_tx: &Sender<SessionEvent>,
) -> bool {
    match chunk_tx.try_send(chunk) {
        Ok(()) => {
            if *lagging {
                *lagging = false;
                segmenter.shrink_target();
                let _ = event_tx.send(SessionEvent::PressureChanged(Pressure::Keeping));
            }
            true
        }
        Err(TrySendError::Full(chunk)) => {
            // One blocked handover is enough to call it: with capture arriving
            // in realtime a chunk is offered every few seconds, so finding the
            // worker still busy means it genuinely is not keeping up. The
            // signal does assume realtime arrival — hand a session a burst of
            // buffered audio and it will report pressure that is really just
            // the burst.
            if !*lagging {
                *lagging = true;
                segmenter.grow_target();
                let _ = event_tx.send(SessionEvent::PressureChanged(Pressure::Lagging));
            }
            // Blocking here is what keeps audio: it waits in the unbounded PCM
            // channel instead of being thrown away.
            chunk_tx.send(chunk).is_ok()
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn run_worker(
    track: Track,
    conversation: ConversationId,
    store: SharedStore,
    chunk_rx: Receiver<Chunk>,
    event_tx: Sender<SessionEvent>,
    engine: Box<dyn Transcriber>,
) {
    // Sequence numbers are assigned here, not taken from the chunk: a chunk may
    // yield more than one segment, and `(conversation, track, seq)` is unique in
    // the store, so reusing the chunk's number would collide the moment a
    // backend returns two segments for one chunk.
    let mut next_seq: u64 = 0;
    let mut previous_text = String::new();

    for chunk in chunk_rx {
        let provisional = chunk.provisional;
        let segments = match engine.transcribe(&chunk.pcm, fc_asr::SAMPLE_RATE_HZ) {
            Ok(segments) => segments,
            Err(err) => {
                let _ = event_tx.send(SessionEvent::Failed {
                    stage: "transcribe",
                    message: err.to_string(),
                });
                continue;
            }
        };

        for segment in segments {
            let seq = if provisional { chunk.seq } else { next_seq };
            let mut segment = stamp_segment(segment, track, seq, chunk.start_ms, provisional);

            if provisional {
                if !segment.is_blank() {
                    let _ = event_tx.send(SessionEvent::Provisional(segment));
                }
                continue;
            }

            // Chunks overlap, so the tail of the last transcript reappears at
            // the head of this one.
            segment.text = fc_asr::dedup_overlap(&previous_text, &segment.text);
            if segment.is_blank() {
                continue;
            }
            previous_text = segment.text.clone();
            next_seq += 1;

            // Shown whether or not the write succeeded: a database failure is
            // worth reporting, but it is not a reason to hide from the user
            // words that were actually said.
            persist(&store, conversation, &segment, &event_tx);
            let _ = event_tx.send(SessionEvent::Committed(segment));
        }
    }
}

/// Writes one segment, reporting rather than panicking on failure: a database
/// error must not cost the user the rest of the meeting. The caller shows the
/// segment either way.
fn persist(
    store: &SharedStore,
    conversation: ConversationId,
    segment: &Segment,
    event_tx: &Sender<SessionEvent>,
) -> bool {
    let result = store
        .lock()
        .expect("store mutex")
        .append_segments(conversation, std::slice::from_ref(segment));
    match result {
        Ok(()) => true,
        Err(err) => {
            let _ = event_tx.send(SessionEvent::Failed {
                stage: "store",
                message: err.to_string(),
            });
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc_core::{SourceKind, Tag};
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    const SPEECH_RMS: f32 = 0.2;

    /// A capture that plays a fixed script of PCM and then ends, standing in
    /// for `parec` on a machine with no usable audio device.
    struct ScriptedCapture;

    impl CaptureHandle for ScriptedCapture {
        fn stop(&mut self) {}
    }

    /// Feeds `seconds` of loud samples followed by `silence_seconds` of quiet,
    /// repeated `repeats` times, then drops the sender so the pipeline drains.
    fn scripted_captures(
        seconds: f32,
        silence_seconds: f32,
        repeats: usize,
        delay: Duration,
    ) -> CaptureFactory {
        Box::new(move |_source, pcm_tx, _event_tx| {
            thread::spawn(move || {
                for _ in 0..repeats {
                    let speech = tone(seconds, SPEECH_RMS);
                    if pcm_tx.send(speech).is_err() {
                        return;
                    }
                    let quiet = tone(silence_seconds, 0.0);
                    if pcm_tx.send(quiet).is_err() {
                        return;
                    }
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                }
            });
            Box::new(ScriptedCapture)
        })
    }

    fn tone(seconds: f32, amplitude: f32) -> PcmFrame {
        let n = (seconds * fc_asr::SAMPLE_RATE_HZ as f32) as usize;
        (0..n)
            .map(|i| {
                let phase = i as f32 / 40.0;
                amplitude * phase.sin()
            })
            .collect()
    }

    /// Returns a distinct line per chunk, optionally after a delay, and counts
    /// how many samples it was asked to transcribe so a test can assert that no
    /// audio went missing.
    ///
    /// The text has to differ per chunk: returning a constant would make every
    /// chunk an exact duplicate of the previous one's tail, which is precisely
    /// what `dedup_overlap` strips, so the segments would legitimately vanish
    /// and the test would be measuring dedup rather than the pipeline.
    struct FakeTranscriber {
        text: String,
        delay: Duration,
        samples_seen: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    impl Transcriber for FakeTranscriber {
        fn transcribe(&self, pcm: &[f32], _sample_rate: u32) -> Result<Vec<Segment>, fc_asr::AsrError> {
            self.samples_seen.fetch_add(pcm.len(), Ordering::SeqCst);
            if !self.delay.is_zero() {
                thread::sleep(self.delay);
            }
            if self.fail {
                return Err(fc_asr::AsrError::UnexpectedOutput {
                    reason: "injected failure".into(),
                    stdout: String::new(),
                });
            }
            let nth = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![Segment {
                track: Track::Selected,
                seq: 0,
                start_ms: 0,
                end_ms: 1_000,
                text: format!("{} number {nth}", self.text),
                translation: None,
                speaker: None,
                confidence: None,
                provisional: false,
            }])
        }

        fn describe(&self) -> EngineInfo {
            engine_info()
        }
    }

    fn engine_info() -> EngineInfo {
        EngineInfo {
            engine: "fake".into(),
            model: "fake".into(),
            language: "en".into(),
            backend: None,
        }
    }

    fn transcribers(text: &str, delay: Duration, fail: bool) -> (TranscriberFactory, Arc<AtomicUsize>) {
        let seen = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let text = text.to_string();
        let counter = Arc::clone(&seen);
        let factory: TranscriberFactory = Box::new(move |_track| {
            Box::new(FakeTranscriber {
                text: text.clone(),
                delay,
                samples_seen: Arc::clone(&counter),
                calls: Arc::clone(&calls),
                fail,
            })
        });
        (factory, seen)
    }

    fn config(segmenter: SegmenterConfig) -> SessionConfig {
        SessionConfig {
            title: "Test".into(),
            group: None,
            source: AudioSource::named(SourceKind::SinkMonitor, "sink.monitor", "A sink"),
            mic_source: None,
            segmenter,
            engine: engine_info(),
        }
    }

    /// Provisional chunks are off by default in these tests: they are exercised
    /// on their own, and leaving them on would double every assertion count.
    fn committed_only() -> SegmenterConfig {
        SegmenterConfig {
            provisional_enabled: false,
            ..Default::default()
        }
    }

    fn temp_store() -> (tempfile::TempDir, SharedStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("library.db")).expect("open store");
        (dir, Arc::new(Mutex::new(store)))
    }

    fn drain_until<F>(events: &Receiver<SessionEvent>, timeout: Duration, mut done: F) -> Vec<SessionEvent>
    where
        F: FnMut(&[SessionEvent]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
                seen.push(event);
                if done(&seen) {
                    break;
                }
            }
        }
        seen
    }

    fn committed(events: &[SessionEvent]) -> Vec<Segment> {
        events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Committed(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn records_transcribes_and_persists_a_conversation() {
        let (_dir, store) = temp_store();
        let (factory, _) = transcribers("hello there", Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(committed_only()),
            factory,
            &scripted_captures(3.0, 1.0, 4, Duration::from_millis(50)),
            1_700_000_000_000,
        )
        .expect("start session");
        let id = session.conversation;

        let events = drain_until(&session.events, Duration::from_secs(10), |seen| {
            committed(seen).len() >= 2
        });
        assert!(
            committed(&events).len() >= 2,
            "expected committed segments, saw {events:?}"
        );

        session.stop(1_700_000_060_000).expect("stop");

        let guard = store.lock().unwrap();
        let stored = guard.load_segments(id).expect("load");
        assert!(!stored.is_empty(), "segments must be persisted as they commit");
        assert!(stored.iter().all(|s| !s.provisional));
        let conversation = guard.get_conversation(id).expect("conversation");
        assert_eq!(conversation.status, ConversationStatus::Completed);
        assert_eq!(conversation.ended_at, Some(1_700_000_060_000));
    }

    #[test]
    fn sequence_numbers_are_unique_per_track() {
        let (_dir, store) = temp_store();
        let (factory, _) = transcribers("line", Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(committed_only()),
            factory,
            &scripted_captures(2.0, 1.0, 4, Duration::ZERO),
            1_700_000_000_000,
        )
        .expect("start");
        let id = session.conversation;
        drain_until(&session.events, Duration::from_secs(10), |seen| {
            committed(seen).len() >= 3
        });
        session.stop(1_700_000_060_000).expect("stop");

        let stored = store.lock().unwrap().load_segments(id).expect("load");
        let mut seqs: Vec<u64> = stored.iter().map(|s| s.seq).collect();
        let before = seqs.len();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(before, seqs.len(), "sequence numbers collided: {stored:?}");
    }

    #[test]
    fn audio_captured_while_paused_is_discarded() {
        let (_dir, store) = temp_store();
        let (factory, seen) = transcribers("ignored", Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(committed_only()),
            factory,
            // A slow drip, so the pause lands between frames rather than after
            // everything has already been handed over.
            &scripted_captures(1.0, 0.5, 6, Duration::from_millis(120)),
            1_700_000_000_000,
        )
        .expect("start");

        session.pause();
        thread::sleep(Duration::from_millis(500));
        let during_pause = seen.load(Ordering::SeqCst);

        session.resume();
        drain_until(&session.events, Duration::from_secs(8), |s| {
            !committed(s).is_empty()
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            seen.load(Ordering::SeqCst) > during_pause,
            "transcription must resume after a pause"
        );
    }

    #[test]
    fn a_slow_transcriber_reports_pressure_without_losing_audio() {
        let (_dir, store) = temp_store();
        let (factory, seen) = transcribers("slow", Duration::from_millis(400), false);
        let session = Session::start(
            Arc::clone(&store),
            config(committed_only()),
            factory,
            &scripted_captures(2.0, 0.6, 6, Duration::from_millis(10)),
            1_700_000_000_000,
        )
        .expect("start");

        let events = drain_until(&session.events, Duration::from_secs(20), |seen| {
            seen.iter().any(|e| {
                matches!(e, SessionEvent::PressureChanged(Pressure::Lagging))
            }) && committed(seen).len() >= 2
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            events.iter().any(|e| matches!(
                e,
                SessionEvent::PressureChanged(Pressure::Lagging)
            )),
            "a transcriber slower than realtime must report pressure, saw {events:?}"
        );
        // 6 repeats of 2.0s speech + 0.6s silence at 16 kHz. Every sample is
        // meant to reach the transcriber: backpressure grows the chunk, it
        // never drops audio.
        let pushed = ((2.0 + 0.6) * 6.0 * fc_asr::SAMPLE_RATE_HZ as f32) as usize;
        let transcribed = seen.load(Ordering::SeqCst);
        assert!(
            transcribed as f32 >= pushed as f32 * 0.9,
            "expected ~{pushed} samples transcribed, saw {transcribed}"
        );
    }

    #[test]
    fn a_failing_transcriber_reports_and_keeps_going() {
        let (_dir, store) = temp_store();
        let (factory, seen) = transcribers("never", Duration::ZERO, true);
        let session = Session::start(
            Arc::clone(&store),
            config(committed_only()),
            factory,
            &scripted_captures(2.0, 1.0, 3, Duration::ZERO),
            1_700_000_000_000,
        )
        .expect("start");
        let id = session.conversation;

        let events = drain_until(&session.events, Duration::from_secs(10), |seen| {
            seen.iter()
                .filter(|e| matches!(e, SessionEvent::Failed { stage: "transcribe", .. }))
                .count()
                >= 2
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Failed { stage: "transcribe", .. })),
            "a transcription failure must be reported, saw {events:?}"
        );
        assert!(
            seen.load(Ordering::SeqCst) > 0,
            "the worker must keep taking chunks after a failure"
        );
        assert!(
            store.lock().unwrap().load_segments(id).unwrap().is_empty(),
            "a failed transcription must not store anything"
        );
    }

    #[test]
    fn provisional_segments_are_shown_but_never_stored() {
        let (_dir, store) = temp_store();
        let (factory, _) = transcribers("draft", Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(SegmenterConfig::default()),
            factory,
            &scripted_captures(4.0, 1.0, 2, Duration::ZERO),
            1_700_000_000_000,
        )
        .expect("start");
        let id = session.conversation;

        let events = drain_until(&session.events, Duration::from_secs(10), |seen| {
            seen.iter().any(|e| matches!(e, SessionEvent::Provisional(_)))
                && !committed(seen).is_empty()
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Provisional(_))),
            "provisional results are what make the view feel live, saw {events:?}"
        );
        let stored = store.lock().unwrap().load_segments(id).expect("load");
        assert!(
            stored.iter().all(|s| !s.provisional),
            "provisional segments must never be persisted"
        );
    }

    /// Unused import guard: `Tag` is re-exported through `fc_core` and the
    /// store's summaries carry it, so a change there should break here.
    #[test]
    fn tags_round_trip_through_the_store() {
        let (_dir, store) = temp_store();
        let guard = store.lock().unwrap();
        let tag = guard.create_tag("standup", Some("#ff0000")).expect("tag");
        let tags: Vec<Tag> = guard.list_tags().expect("list");
        assert!(tags.iter().any(|t| t.id == tag && t.name == "standup"));
    }
    /// The whole chain, with nothing faked: a real PipeWire capture of real
    /// audio, cut by the real segmenter, transcribed by the real voxtype
    /// binary, stored in a real database.
    ///
    /// Ignored by default because it needs a sound server, `espeak-ng`,
    /// `ffmpeg`, `paplay` and voxtype with a model installed. Run it with:
    /// `cargo test -p fastcription -- --ignored end_to_end`
    ///
    /// It plays speech into the default sink and records that sink's monitor,
    /// which is how the app is meant to be used for a meeting — and it works on
    /// a machine with no microphone at all.
    #[test]
    #[ignore = "needs a sound server, espeak-ng, ffmpeg and voxtype with a model"]
    fn end_to_end_through_the_real_pipeline() {
        const SPOKEN: &str =
            "The quarterly roadmap review is scheduled for next Tuesday, \
             and we still need owners for the migration work.";

        let dir = tempfile::tempdir().expect("tempdir");
        let raw = dir.path().join("speech.wav");
        let wav = dir.path().join("speech16k.wav");

        let spoken = std::process::Command::new("espeak-ng")
            .args(["-v", "en-us", "-s", "150", "-w"])
            .arg(&raw)
            .arg(SPOKEN)
            .status()
            .expect("run espeak-ng");
        assert!(spoken.success(), "espeak-ng could not synthesise speech");

        let resampled = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&raw)
            .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .status()
            .expect("run ffmpeg");
        assert!(resampled.success(), "ffmpeg could not resample");

        let source = fc_audio::default_source()
            .expect("list sources")
            .expect("the sound server offers no monitor source to record");

        let (_store_dir, store) = temp_store();
        let factory: TranscriberFactory = Box::new(|_track| Box::new(fc_asr::VoxtypeCli::new()));
        let session = Session::start(
            Arc::clone(&store),
            SessionConfig {
                title: "End to end".into(),
                group: None,
                source,
                mic_source: None,
                segmenter: committed_only(),
                engine: engine_info(),
            },
            factory,
            &parec_captures(),
            1_700_000_000_000,
        )
        .expect("start session");
        let id = session.conversation;

        // Give parec a moment to connect before there is anything to hear.
        thread::sleep(Duration::from_millis(700));
        let played = std::process::Command::new("paplay")
            .arg(&wav)
            .status()
            .expect("run paplay");
        assert!(played.success(), "paplay could not play into the default sink");

        let events = drain_until(&session.events, Duration::from_secs(45), |seen| {
            committed(seen)
                .iter()
                .any(|s| s.text.to_lowercase().contains("roadmap"))
        });
        session.stop(1_700_000_060_000).expect("stop");

        let heard: Vec<String> = committed(&events).iter().map(|s| s.text.clone()).collect();
        assert!(
            heard.iter().any(|t| t.to_lowercase().contains("roadmap")),
            "expected the spoken words back from the pipeline, heard {heard:?}"
        );
        let stored = store.lock().unwrap().load_segments(id).expect("load");
        assert!(
            !stored.is_empty(),
            "a transcript that reached the UI must also have been stored"
        );
    }

}
