//! The session supervisor: the only place that knows how capture,
//! transcription and storage fit together.
//!
//! One [`Session`] records one conversation. Per track it runs a capture
//! backend producing PCM and one thread that feeds a
//! [`fc_asr::TranscriptStream`], turning its updates into events for the
//! interface and rows in the store.
//!
//! ## Why there is no chunk queue any more
//!
//! The first design cut audio into disjoint chunks and transcribed each once,
//! which meant reading a sentence about eight seconds after it was spoken —
//! useless for following a live conversation. The stream instead re-transcribes
//! the current utterance roughly once a second and commits words once two
//! consecutive passes agree, which puts text on screen about a second and a
//! half behind the speaker. Measured on this machine: 100% of 66 words correct
//! across a 30 second sample at 23% of one CPU's time.
//!
//! ## Why audio is still never dropped
//!
//! The PCM channel is unbounded and `push` blocks for the length of one
//! transcription, so when transcription falls behind, captured audio waits in
//! the channel rather than being discarded — 16 kHz mono f32 costs 64 KB per
//! second of lag, which is cheap next to losing part of a meeting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{unbounded, Receiver, Sender};
use fc_asr::{StreamConfig, Transcriber, TranscriptStream, Update};
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
    Box::new(|source, pcm_tx, event_tx| Box::new(ParecCapture::start(source, pcm_tx, event_tx)))
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
    pub stream: StreamConfig,
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
    /// Asks every stream thread to finalise the utterance it has in flight.
    ///
    /// Only the stream thread may touch its `TranscriptStream`, so a pause
    /// cannot cut the sentence itself: it leaves this flag, and the thread acts
    /// on it before dropping the next frame.
    cut_pending: Arc<AtomicBool>,
    tracks: Vec<TrackRuntime>,
    store: SharedStore,
}

struct TrackRuntime {
    capture: Box<dyn CaptureHandle>,
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

        let conversation =
            store
                .lock()
                .expect("store mutex")
                .create_conversation(&NewConversation {
                    title: cfg.title.clone(),
                    group: cfg.group,
                    started_at: now,
                    source: cfg.source.clone(),
                    mic_track: cfg.mic_source.is_some(),
                    engine: cfg.engine.clone(),
                    voxtype_meeting_id: None,
                })?;

        let (event_tx, events) = unbounded::<SessionEvent>();
        let paused = Arc::new(AtomicBool::new(false));
        let cut_pending = Arc::new(AtomicBool::new(false));
        let transcriber = Arc::new(transcriber);

        let mut tracks = Vec::new();
        tracks.push(spawn_track(
            Track::Selected,
            cfg.source.clone(),
            cfg.stream.clone(),
            conversation,
            Arc::clone(&store),
            event_tx.clone(),
            Arc::clone(&paused),
            Arc::clone(&cut_pending),
            Arc::clone(&transcriber),
            captures,
            true,
        ));
        if let Some(mic) = cfg.mic_source.clone() {
            tracks.push(spawn_track(
                Track::Microphone,
                mic,
                cfg.stream.clone(),
                conversation,
                Arc::clone(&store),
                event_tx.clone(),
                Arc::clone(&paused),
                Arc::clone(&cut_pending),
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
            cut_pending,
            tracks,
            store,
        })
    }

    /// Audio arriving while paused is discarded, matching what voxtype's own
    /// meeting pause does. The consequence is that segment timestamps close the
    /// gap rather than preserving it: a transcript measures speech, not the
    /// wall clock.
    ///
    /// Which is exactly why pausing also cuts the utterance in flight. With the
    /// gap closed and no cut, the sentence spoken before the pause and the one
    /// spoken after it become a single utterance -- one row in the store, read
    /// as though they were said together. The cut happens on the stream thread,
    /// which owns the stream; this only asks for it.
    pub fn pause(&self) {
        if !self.paused.swap(true, Ordering::SeqCst) {
            self.cut_pending.store(true, Ordering::SeqCst);
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
    /// stream thread drain what is left and finalise the utterance in flight
    /// before exiting. No step can skip the audio already recorded.
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
    cfg: StreamConfig,
    conversation: ConversationId,
    store: SharedStore,
    event_tx: Sender<SessionEvent>,
    paused: Arc<AtomicBool>,
    cut_pending: Arc<AtomicBool>,
    transcriber: Arc<TranscriberFactory>,
    captures: &CaptureFactory,
    report_levels: bool,
) -> TrackRuntime {
    // Unbounded on purpose: `push` blocks while a pass runs, so this is where
    // captured audio waits instead of being thrown away.
    let (pcm_tx, pcm_rx) = unbounded::<PcmFrame>();

    // A capture that reports levels needs the real event channel; the mic track
    // gets a sink that only carries failures, so it cannot fight for the meter.
    let capture_events = if report_levels {
        event_tx.clone()
    } else {
        level_filtered(event_tx.clone())
    };
    let capture = captures(source, pcm_tx, capture_events);

    let worker = thread::Builder::new()
        .name(format!("fc-stream-{}", track.as_str()))
        .spawn(move || {
            let stream = TranscriptStream::new(Boxed(transcriber(track)), cfg);
            run_stream(
                track,
                conversation,
                store,
                pcm_rx,
                event_tx,
                paused,
                cut_pending,
                stream,
            )
        })
        .expect("spawn transcription thread");

    TrackRuntime { capture, worker }
}

/// Lets a boxed transcriber satisfy [`TranscriptStream`]'s generic bound. The
/// factory hands out `Box<dyn Transcriber>` so the backend can be chosen at
/// runtime; the stream wants a concrete type.
struct Boxed(Box<dyn Transcriber>);

impl Transcriber for Boxed {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<Vec<Segment>, fc_asr::AsrError> {
        self.0.transcribe(pcm, sample_rate)
    }

    fn describe(&self) -> EngineInfo {
        self.0.describe()
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

#[allow(clippy::too_many_arguments)]
fn run_stream(
    track: Track,
    conversation: ConversationId,
    store: SharedStore,
    pcm_rx: Receiver<PcmFrame>,
    event_tx: Sender<SessionEvent>,
    paused: Arc<AtomicBool>,
    cut_pending: Arc<AtomicBool>,
    mut stream: TranscriptStream<Boxed>,
) {
    let mut state = TrackState::default();

    for frame in &pcm_rx {
        if cut_pending.swap(false, Ordering::SeqCst) {
            // `Session::pause` asked for this. It runs here because only this
            // thread may touch the stream, and before the frame is dropped
            // below, so the utterance ends where the user paused rather than
            // absorbing the first words spoken after they resume.
            if let Some(update) = stream.cut() {
                apply(&mut state, update, track, conversation, &store, &event_tx);
            }
        }
        if paused.load(Ordering::SeqCst) {
            // Audio arriving while paused is discarded, matching voxtype's own
            // meeting pause.
            continue;
        }
        if let Some(update) = stream.push(&frame) {
            apply(&mut state, update, track, conversation, &store, &event_tx);
        }
    }

    // Capture has ended; whatever is mid-utterance is still the user's words.
    if let Some(update) = stream.finish() {
        apply(&mut state, update, track, conversation, &store, &event_tx);
    }
}

#[derive(Default)]
struct TrackState {
    /// Words agreed so far in the utterance being spoken. Shown as the live
    /// line, replaced wholesale on each update.
    stable: String,
    /// Sequence numbers are assigned here rather than by the stream, because
    /// `(conversation, track, seq)` is unique in the store and only this side
    /// knows how many rows it has written.
    next_seq: u64,
    lagging: bool,
}

/// Turns one [`Update`] into events and rows, in that order: every failure, then
/// every finalised utterance, then the live line.
///
/// Nothing here is mutually exclusive. The stream's give-up path reports a
/// failure *and* hands back the words it had already agreed on, and a single
/// push can finalise more than one utterance and still leave a hypothesis in
/// flight. An earlier version checked the error first and returned, so a
/// permanently broken engine threw away the only text it managed to salvage --
/// it reached neither the interface nor the store.
fn apply(
    state: &mut TrackState,
    update: Update,
    track: Track,
    conversation: ConversationId,
    store: &SharedStore,
    event_tx: &Sender<SessionEvent>,
) {
    for message in update.errors {
        // A failing engine must be visible: silence from a broken transcriber
        // is indistinguishable from a quiet room, which is the worst way for
        // this app to fail. Unless it gave up, the utterance stays buffered and
        // the next pass retries it, so nothing is lost by carrying on.
        let _ = event_tx.send(SessionEvent::Failed {
            stage: "transcribe",
            message,
        });
    }

    if update.lagging != state.lagging {
        state.lagging = update.lagging;
        let pressure = if update.lagging {
            Pressure::Lagging
        } else {
            Pressure::Keeping
        };
        let _ = event_tx.send(SessionEvent::PressureChanged(pressure));
    }

    if !update.stable.is_empty() {
        if !state.stable.is_empty() {
            state.stable.push(' ');
        }
        state.stable.push_str(update.stable.trim());
    }

    for finished in update.finished {
        // Each finalised utterance is its own row, in the order it was spoken.
        state.stable.clear();
        if finished.text.trim().is_empty() {
            continue;
        }
        let segment = Segment {
            track,
            seq: state.next_seq,
            start_ms: finished.start_ms,
            end_ms: finished.end_ms,
            text: finished.text,
            translation: None,
            speaker: None,
            confidence: None,
            provisional: false,
        };
        state.next_seq += 1;
        // Shown whether or not the write succeeded: a database failure is worth
        // reporting, but it is not a reason to hide words that were said.
        persist(store, conversation, &segment, event_tx);
        let _ = event_tx.send(SessionEvent::Committed(segment));
    }

    // The live line: what is agreed, plus the tail that is not yet. It is
    // replaced on every update and never stored.
    let line = match (state.stable.as_str(), update.unstable.trim()) {
        ("", "") => return,
        (stable, "") => stable.to_owned(),
        ("", tail) => tail.to_owned(),
        (stable, tail) => format!("{stable} {tail}"),
    };
    // Stamped with where the sentence being spoken began, which is the stream's
    // to know: this used to be a flat zero, so every live row read `00:00`. A
    // line still being spoken has no end yet, so the span is a point.
    let start_ms = update.utterance_start_ms.unwrap_or(0);
    let _ = event_tx.send(SessionEvent::Provisional(Segment {
        track,
        seq: state.next_seq,
        start_ms,
        end_ms: start_ms,
        text: line,
        translation: None,
        speaker: None,
        confidence: None,
        provisional: true,
    }));
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
    ///
    /// `stop` really stops it, like the real one: a test that asks for more
    /// audio than it consumes would otherwise have `Session::stop` wait out the
    /// whole script, since the stream thread only exits when the PCM sender is
    /// dropped.
    struct ScriptedCapture(Arc<AtomicBool>);

    impl CaptureHandle for ScriptedCapture {
        fn stop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
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
            let stopped = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stopped);
            thread::spawn(move || {
                for _ in 0..repeats {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
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
            Box::new(ScriptedCapture(stopped))
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

    /// Transcribes a prefix of a fixed script, one word per half second of
    /// audio it is given.
    ///
    /// Consecutive passes over a growing utterance must agree on their prefix,
    /// because agreement is what makes a word stable. A fake that returned
    /// different text each call would never stabilise and every assertion here
    /// would pass vacuously while testing nothing.
    const SCRIPT: [&str; 8] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ];

    struct FakeTranscriber {
        delay: Duration,
        samples_seen: Arc<AtomicUsize>,
        fail: bool,
    }

    impl Transcriber for FakeTranscriber {
        fn transcribe(
            &self,
            pcm: &[f32],
            _sample_rate: u32,
        ) -> Result<Vec<Segment>, fc_asr::AsrError> {
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
            let half_seconds = pcm.len() / (fc_asr::SAMPLE_RATE_HZ as usize / 2);
            let count = half_seconds.clamp(1, SCRIPT.len());
            Ok(vec![Segment {
                track: Track::Selected,
                seq: 0,
                start_ms: 0,
                end_ms: 1_000,
                text: SCRIPT[..count].join(" "),
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

    /// Works for its first `succeed_for` passes, then fails for ever: a
    /// transcription server that has gone away mid-meeting.
    ///
    /// The text is the same every pass, so two consecutive passes agree and the
    /// stream reports it stable — which is the only reason there is anything to
    /// salvage when it later gives up.
    struct DyingTranscriber {
        calls: Arc<AtomicUsize>,
        succeed_for: usize,
    }

    impl Transcriber for DyingTranscriber {
        fn transcribe(
            &self,
            _pcm: &[f32],
            _sample_rate: u32,
        ) -> Result<Vec<Segment>, fc_asr::AsrError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) >= self.succeed_for {
                return Err(fc_asr::AsrError::Io(std::io::Error::other("server gone")));
            }
            Ok(vec![Segment {
                track: Track::Selected,
                seq: 0,
                start_ms: 0,
                end_ms: 1_000,
                text: SALVAGED.into(),
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

    const SALVAGED: &str = "the words that were agreed";

    fn dying_transcribers(succeed_for: usize) -> (TranscriberFactory, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let factory: TranscriberFactory = Box::new(move |_track| {
            Box::new(DyingTranscriber {
                calls: Arc::clone(&counter),
                succeed_for,
            })
        });
        (factory, calls)
    }

    fn transcribers(delay: Duration, fail: bool) -> (TranscriberFactory, Arc<AtomicUsize>) {
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let factory: TranscriberFactory = Box::new(move |_track| {
            Box::new(FakeTranscriber {
                delay,
                samples_seen: Arc::clone(&counter),
                fail,
            })
        });
        (factory, seen)
    }

    fn config(stream: StreamConfig) -> SessionConfig {
        SessionConfig {
            title: "Test".into(),
            group: None,
            source: AudioSource::named(SourceKind::SinkMonitor, "sink.monitor", "A sink"),
            mic_source: None,
            stream,
            engine: engine_info(),
        }
    }

    /// A stream tuned for tests: re-transcribe often so a short scripted
    /// capture produces several passes, and end an utterance quickly so the
    /// assertions do not wait on a realistic silence hold.
    fn brisk() -> StreamConfig {
        StreamConfig {
            step: Duration::from_millis(300),
            silence_hold: Duration::from_millis(300),
            max_utterance: Duration::from_secs(8),
            ..Default::default()
        }
    }

    fn temp_store() -> (tempfile::TempDir, SharedStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("library.db")).expect("open store");
        (dir, Arc::new(Mutex::new(store)))
    }

    fn drain_until<F>(
        events: &Receiver<SessionEvent>,
        timeout: Duration,
        mut done: F,
    ) -> Vec<SessionEvent>
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
        let (factory, _) = transcribers(Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
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
        assert!(
            !stored.is_empty(),
            "segments must be persisted as they commit"
        );
        assert!(stored.iter().all(|s| !s.provisional));
        let conversation = guard.get_conversation(id).expect("conversation");
        assert_eq!(conversation.status, ConversationStatus::Completed);
        assert_eq!(conversation.ended_at, Some(1_700_000_060_000));
    }

    #[test]
    fn sequence_numbers_are_unique_per_track() {
        let (_dir, store) = temp_store();
        let (factory, _) = transcribers(Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
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
        let (factory, seen) = transcribers(Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
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
        let (factory, seen) = transcribers(Duration::from_millis(400), false);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
            factory,
            &scripted_captures(2.0, 0.6, 6, Duration::from_millis(10)),
            1_700_000_000_000,
        )
        .expect("start");

        let events = drain_until(&session.events, Duration::from_secs(20), |seen| {
            seen.iter()
                .any(|e| matches!(e, SessionEvent::PressureChanged(Pressure::Lagging)))
                && committed(seen).len() >= 2
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::PressureChanged(Pressure::Lagging))),
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
        let (factory, seen) = transcribers(Duration::ZERO, true);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
            factory,
            &scripted_captures(2.0, 1.0, 3, Duration::ZERO),
            1_700_000_000_000,
        )
        .expect("start");
        let id = session.conversation;

        let events = drain_until(&session.events, Duration::from_secs(10), |seen| {
            seen.iter()
                .filter(|e| {
                    matches!(
                        e,
                        SessionEvent::Failed {
                            stage: "transcribe",
                            ..
                        }
                    )
                })
                .count()
                >= 2
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            events.iter().any(|e| matches!(
                e,
                SessionEvent::Failed {
                    stage: "transcribe",
                    ..
                }
            )),
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
        let (factory, _) = transcribers(Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
            factory,
            &scripted_captures(4.0, 1.0, 2, Duration::ZERO),
            1_700_000_000_000,
        )
        .expect("start");
        let id = session.conversation;

        let events = drain_until(&session.events, Duration::from_secs(10), |seen| {
            seen.iter()
                .any(|e| matches!(e, SessionEvent::Provisional(_)))
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

    /// An engine that dies mid-meeting reports a failure *and* hands back the
    /// words it had already agreed on. Those are the only text that survives the
    /// utterance, so they have to reach the user and the store — a version of
    /// `apply` that checked the error first and returned threw them away, and
    /// the failure made it look as though there had never been anything there.
    #[test]
    fn words_salvaged_when_the_engine_gives_up_are_shown_and_stored() {
        let (_dir, store) = temp_store();
        // Two agreeing passes make the text stable, then every pass fails: the
        // mid-utterance ones, and all three finalisation attempts.
        let (factory, _calls) = dying_transcribers(2);
        let session = Session::start(
            Arc::clone(&store),
            config(brisk()),
            factory,
            // One utterance, then enough silence for the failed finalisation to
            // be retried to exhaustion (0.3s hold plus two 0.3s waits).
            &scripted_captures(2.0, 1.4, 1, Duration::ZERO),
            1_700_000_000_000,
        )
        .expect("start session");
        let id = session.conversation;

        let events = drain_until(&session.events, Duration::from_secs(10), |seen| {
            !committed(seen).is_empty()
        });
        session.stop(1_700_000_060_000).expect("stop");

        assert!(
            events.iter().any(|e| matches!(
                e,
                SessionEvent::Failed {
                    stage: "transcribe",
                    ..
                }
            )),
            "the failure must still be reported, saw {events:?}"
        );
        let shown: Vec<String> = committed(&events).iter().map(|s| s.text.clone()).collect();
        assert!(
            shown.iter().any(|t| t == SALVAGED),
            "the salvaged words must reach the interface, saw {shown:?}"
        );

        let stored = store.lock().unwrap().load_segments(id).expect("load");
        assert!(
            stored.iter().any(|s| s.text == SALVAGED),
            "and they must be persisted, not only shown: {stored:?}"
        );
    }

    /// Audio arriving while paused is discarded, so without a cut the sentence
    /// before the pause and the sentence after it become one utterance with the
    /// gap closed — one row reading as though they were said together.
    #[test]
    fn pausing_mid_utterance_commits_the_half_already_spoken() {
        let (_dir, store) = temp_store();
        let (factory, _) = transcribers(Duration::ZERO, false);
        let session = Session::start(
            Arc::clone(&store),
            SessionConfig {
                stream: StreamConfig {
                    step: Duration::from_millis(300),
                    // Both boundaries out of reach: speech never stops and the
                    // test is over long before eight seconds, so a committed
                    // segment can only have come from the pause.
                    silence_hold: Duration::from_secs(30),
                    max_utterance: Duration::from_secs(8),
                    ..Default::default()
                },
                ..config(brisk())
            },
            factory,
            // Continuous speech in 0.5s frames, with no silence between them,
            // for far longer than this test runs: the capture ending would
            // itself finalise the utterance, so it must not be what happens.
            &scripted_captures(0.5, 0.0, 400, Duration::from_millis(60)),
            1_700_000_000_000,
        )
        .expect("start session");
        let id = session.conversation;

        // Wait until an utterance is genuinely in flight before pausing.
        let before = drain_until(&session.events, Duration::from_secs(10), |seen| {
            seen.iter()
                .any(|e| matches!(e, SessionEvent::Provisional(_)))
        });
        assert!(
            committed(&before).is_empty(),
            "nothing may have finalised on its own yet, saw {before:?}"
        );

        session.pause();
        // Short on purpose: the cut lands on the next frame, about 60ms later.
        // Nothing else in this configuration can finalise an utterance inside
        // two seconds -- the silence hold is thirty, the ceiling is eight, and
        // the capture runs for twenty.
        let after = drain_until(&session.events, Duration::from_secs(2), |seen| {
            !committed(seen).is_empty()
        });
        session.stop(1_700_000_060_000).expect("stop");

        let committed_after = committed(&after);
        assert!(
            !committed_after.is_empty(),
            "a pause must finalise the sentence in flight, saw {after:?}"
        );
        let first = &committed_after[0];
        assert!(
            !first.text.trim().is_empty(),
            "the committed half must carry the words spoken before the pause"
        );
        assert_eq!(
            first.start_ms, 0,
            "it is the first utterance of the session"
        );

        let stored = store.lock().unwrap().load_segments(id).expect("load");
        assert!(
            stored.iter().any(|s| s.text == first.text),
            "the cut utterance must be persisted like any other: {stored:?}"
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
    /// audio, segmented by the real stream, transcribed by the real voxtype
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
        const SPOKEN: &str = "The quarterly roadmap review is scheduled for next Tuesday, \
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
                stream: brisk(),
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
        assert!(
            played.success(),
            "paplay could not play into the default sink"
        );

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
