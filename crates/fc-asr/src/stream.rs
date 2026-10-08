//! Silence-anchored, re-transcribed utterances with LocalAgreement-2 stability.
//!
//! This replaces the old disjoint-chunk model (cut on silence, transcribe
//! each chunk once, dedup the overlap). That model cost `chunk_length + ~0.8s`
//! of latency before a sentence appeared at all, and needed a tail/head word
//! dedup hack at every chunk boundary. This module instead re-transcribes the
//! *entire* current utterance from its start on every `step`, and only ever
//! commits the longest common prefix two consecutive passes agree on. Numbers
//! measured on this machine (Core Ultra 9 285K, voxtype 1.0.1, `base.en`,
//! CPU) are why it's shaped this way:
//!
//! - A 2s clip costs 0.71s to transcribe; an 11s clip costs 0.78s. Whisper
//!   always pads to 30s internally, so re-transcribing a growing buffer from
//!   scratch costs almost the same each time as transcribing it once -- the
//!   "full context every pass" design is close to free.
//! - With `context_window_optimization = true` in voxtype's config, clips
//!   under 22.5s get 2.0x-2.8x faster (a 7s window: 0.75s -> 0.28s), which is
//!   why [`StreamConfig::max_utterance`] defaults to 20.0s rather than
//!   something closer to Whisper's 30s pad.
//! - Re-transcribing every 1.0s and committing the longest common prefix of
//!   the last two hypotheses measured 100% word accuracy (66/66 words) on a
//!   30.4s six-utterance sample, at 23% CPU duty, with words appearing
//!   ~1.5-2s after being spoken.
//!
//! Stable text is never revised once emitted (`TranscriptStream`'s core
//! trade-off): low latency and no UI flicker, at the cost of an occasional
//! early-committed word that a later pass would have corrected.

use std::time::{Duration, Instant};

use fc_core::Segment;

use crate::transcriber::Transcriber;

/// All audio this crate works with is 16 kHz mono, matching voxtype's own
/// `transcribe` input contract.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Width of the window used for RMS/silence detection. Small enough that an
/// utterance boundary lands within one window of the true silence edge,
/// large enough that a single loud sample doesn't flip the verdict. Matches
/// the old segmenter's granularity.
const FRAME_MS: u64 = 20;
const FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ as u64 * FRAME_MS / 1_000) as usize;

/// Tunables for [`TranscriptStream`]. Defaults match the measurements above.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// How much new audio accumulates before the whole current utterance is
    /// re-transcribed.
    pub step: Duration,
    /// RMS below this is silence for utterance-boundary purposes.
    pub silence_rms: f32,
    /// How long trailing silence must run before the current utterance is
    /// finalised.
    pub silence_hold: Duration,
    /// Hard ceiling: an utterance finalises here even with no silence, kept
    /// under Whisper's 22.5s context-optimisation threshold so every
    /// utterance benefits from it.
    pub max_utterance: Duration,
    /// Below this, a speech burst is noise, not an utterance: dropped
    /// entirely rather than finalised. Must stay smaller than `step` for
    /// every speech burst this short to be dropped before any transcription
    /// pass ever runs on it; a caller who sets it larger than `step` gets a
    /// burst that may have already had a pass run on it, finalised anyway
    /// rather than silently discarding text already handed to the caller
    /// (stable text is never revised, see the module doc).
    pub min_utterance: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            step: Duration::from_secs_f64(1.0),
            silence_rms: 0.01,
            silence_hold: Duration::from_secs_f64(0.4),
            max_utterance: Duration::from_secs_f64(20.0),
            min_utterance: Duration::from_secs_f64(0.4),
        }
    }
}

/// What [`TranscriptStream::push`] reports after absorbing new audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Update {
    /// Words newly agreed on by two consecutive passes. Append-only; never
    /// revised.
    pub stable: String,
    /// The current pass's tail that is not agreed yet. Replaces whatever the
    /// previous update's `unstable` was. Empty once the utterance finalises.
    pub unstable: String,
    /// Set when this update ends an utterance; carries its full final text
    /// and its span relative to the session start.
    pub finished: Option<Utterance>,
    /// Set when the pass that produced this update failed. The stream keeps
    /// going: the utterance stays buffered and the next pass retries it.
    pub error: Option<String>,
    /// True while transcription cannot keep up with `step`.
    pub lagging: bool,
}

/// One utterance's final, authoritative text.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Per-utterance accumulator. Lives only while speech is buffered; `None` in
/// [`TranscriptStream`] means "waiting for the next utterance to start".
struct UtteranceState {
    /// Every sample since this utterance's first speech frame, including any
    /// trailing silence not yet long enough to finalise.
    buf: Vec<f32>,
    /// Session-absolute sample offset where this utterance began.
    start_samples: u64,
    /// Consecutive trailing silence, in ms, since the last loud frame.
    silence_run_ms: u64,
    /// Samples appended to `buf` since the last transcription pass.
    samples_since_pass: u64,
    /// Set once finalisation has been attempted and failed, so the retry waits
    /// a full interval instead of firing on the very next frame.
    finalize_failed: bool,
    /// Failed finalisation attempts so far, so a permanently unavailable engine
    /// cannot keep an utterance buffered for ever.
    finalize_attempts: u32,
    /// Everything reported stable for this utterance, kept so that giving up on
    /// finalisation can still surface the words that were already agreed.
    stable_text: Vec<String>,
    /// The previous pass's full hypothesis (original casing), for
    /// LocalAgreement-2 comparison against the next one.
    prev_words: Vec<String>,
    /// How many of `prev_words`, from the front, have already been emitted
    /// as stable. Monotonically non-decreasing.
    stable_count: usize,
}

impl UtteranceState {
    fn new(start_samples: u64) -> Self {
        Self {
            buf: Vec::new(),
            start_samples,
            silence_run_ms: 0,
            samples_since_pass: 0,
            finalize_failed: false,
            finalize_attempts: 0,
            stable_text: Vec::new(),
            prev_words: Vec::new(),
            stable_count: 0,
        }
    }
}

/// Collects everything that happened during one [`TranscriptStream::push`]
/// or [`TranscriptStream::finish`] call, across however many internal 20ms
/// frames and utterance boundaries it spanned, into the single [`Update`]
/// the API contract allows returning.
///
/// Normally a push spans at most one pass or one finalisation. The one case
/// where more than one event lands in the same `Merge` is a single call
/// carrying enough audio to finalise more than one utterance (only possible
/// with unusually large input slices, not realtime-sized frames): every
/// finalised utterance's text still reaches the caller, concatenated into
/// `stable`, but only the *last* one's [`Utterance`] (with its own
/// start/end) survives as `finished` -- the API has room for one.
#[derive(Default)]
struct Merge {
    stable_parts: Vec<String>,
    unstable: Option<String>,
    finished: Option<Utterance>,
    error: Option<String>,
}

impl Merge {
    fn into_update(self, lagging: bool) -> Option<Update> {
        if self.stable_parts.is_empty()
            && self.unstable.is_none()
            && self.finished.is_none()
            && self.error.is_none()
        {
            return None;
        }
        Some(Update {
            stable: self.stable_parts.join(" "),
            unstable: self.unstable.unwrap_or_default(),
            finished: self.finished,
            error: self.error,
            lagging,
        })
    }
}

/// Re-transcribes a growing utterance buffer and reports what has become
/// stable, generic over [`Transcriber`] so tests can drive it with a fake
/// (see the `tests` module) instead of the real `voxtype` subprocess.
pub struct TranscriptStream<T: Transcriber> {
    engine: T,
    config: StreamConfig,

    // Sample-count thresholds derived from `config` once, so the per-frame
    // hot path never redoes the float math.
    step_samples: u64,
    /// Audio between passes, never below `step_samples` and grown to at least
    /// the duration of the last pass.
    ///
    /// A pass is triggered by accumulated audio, so on a machine where a pass
    /// takes longer than the interval a fixed interval never catches up and the
    /// backlog grows without bound. Measured on a 30.4s sample with passes
    /// slowed past the interval: fixed gave 149% duty and never caught up,
    /// adapting gave 86% duty, a third fewer passes and better accuracy.
    /// Stretching costs nothing, because every pass transcribes the whole
    /// utterance anyway — fewer passes is less repeated work, not less text.
    effective_step_samples: u64,
    max_utterance_samples: u64,
    min_utterance_samples: u64,
    silence_hold_ms: u64,

    // Session-absolute count of every sample ever pushed, used to stamp
    // utterance boundaries in session time.
    total_samples_in: u64,
    utterance: Option<UtteranceState>,
    // Leftover samples not yet long enough to fill one 20ms RMS window.
    frame_acc: Vec<f32>,

    lagging: bool,
}

impl<T: Transcriber> TranscriptStream<T> {
    pub fn new(engine: T, config: StreamConfig) -> Self {
        let step_samples = duration_to_samples(config.step);
        let max_utterance_samples = duration_to_samples(config.max_utterance);
        let min_utterance_samples = duration_to_samples(config.min_utterance);
        let silence_hold_ms = config.silence_hold.as_millis() as u64;
        Self {
            engine,
            config,
            step_samples,
            effective_step_samples: step_samples,
            max_utterance_samples,
            min_utterance_samples,
            silence_hold_ms,
            total_samples_in: 0,
            utterance: None,
            frame_acc: Vec::with_capacity(FRAME_SAMPLES),
            lagging: false,
        }
    }

    /// Feeds captured audio. Returns `None` when nothing changed, otherwise
    /// the update to apply. May block for the length of one transcription:
    /// RMS-window boundaries crossed by `samples` are processed in order,
    /// and a pass or finalisation that falls on one of them runs
    /// synchronously before this returns.
    pub fn push(&mut self, samples: &[f32]) -> Option<Update> {
        let mut merge = Merge::default();
        for &s in samples {
            self.frame_acc.push(s);
            self.total_samples_in += 1;
            if self.frame_acc.len() == FRAME_SAMPLES {
                let frame =
                    std::mem::replace(&mut self.frame_acc, Vec::with_capacity(FRAME_SAMPLES));
                self.process_frame(&frame, &mut merge);
            }
        }
        merge.into_update(self.lagging)
    }

    /// Ends the session: finalises any utterance in flight, plus whatever
    /// partial 20ms window of trailing samples never completed.
    pub fn finish(&mut self) -> Option<Update> {
        let mut merge = Merge::default();
        if !self.frame_acc.is_empty() {
            let frame = std::mem::take(&mut self.frame_acc);
            self.process_frame(&frame, &mut merge);
        }
        if let Some(utt) = self.utterance.take() {
            // If finalisation itself fails, `finalize` hands the utterance
            // back rather than dropping it -- there is no "next frame" at
            // end of session, but the caller can still call `finish` again
            // (e.g. after the engine recovers) without having lost the audio.
            if let Some(restored) = self.finalize(utt, &mut merge) {
                self.utterance = Some(restored);
            }
        }
        merge.into_update(self.lagging)
    }

    /// Processes exactly one RMS window's worth of samples (always 20ms,
    /// except the final partial window `finish` flushes).
    fn process_frame(&mut self, frame: &[f32], merge: &mut Merge) {
        let loud = rms(frame) >= self.config.silence_rms;

        let mut utt = match self.utterance.take() {
            Some(utt) => utt,
            None => {
                if !loud {
                    // Silence before any speech: discarded, never buffered.
                    return;
                }
                UtteranceState::new(self.total_samples_in - frame.len() as u64)
            }
        };

        utt.buf.extend_from_slice(frame);
        utt.samples_since_pass += frame.len() as u64;
        if loud {
            utt.silence_run_ms = 0;
        } else {
            utt.silence_run_ms += FRAME_MS;
        }

        let should_finalize = utt.buf.len() as u64 >= self.max_utterance_samples
            || utt.silence_run_ms >= self.silence_hold_ms;

        if should_finalize {
            if utt.finalize_failed && utt.samples_since_pass < self.effective_step_samples {
                // A failed finalisation waits an interval before retrying.
                self.utterance = Some(utt);
                return;
            }
            if let Some(restored) = self.finalize(utt, merge) {
                self.utterance = Some(restored);
            }
            return;
        }

        if utt.samples_since_pass >= self.effective_step_samples {
            self.run_pass(&mut utt, merge);
        }
        self.utterance = Some(utt);
    }

    /// Records how long a pass took, setting `lagging` and stretching the
    /// interval so the next pass cannot be triggered before the engine could
    /// plausibly finish it.
    fn note_pass(&mut self, elapsed: Duration) {
        self.lagging = elapsed > self.config.step;
        // A fifth of headroom: enough that a pass whose cost is creeping up
        // does not spend every interval exactly at the limit.
        let needed = duration_to_samples(elapsed.mul_f64(1.2));
        self.effective_step_samples = self.step_samples.max(needed);
    }

    /// Re-transcribes the whole utterance buffer and advances the stable
    /// prefix by however much the new hypothesis agrees with the previous
    /// one (LocalAgreement-2). On a transcription error, the hypothesis and
    /// stable prefix are left exactly as they were: the next pass retries
    /// against the same (now slightly longer) buffer.
    fn run_pass(&mut self, utt: &mut UtteranceState, merge: &mut Merge) {
        utt.samples_since_pass = 0;
        let started = Instant::now();
        let result = self.engine.transcribe(&utt.buf, SAMPLE_RATE_HZ);
        self.note_pass(started.elapsed());

        let segments = match result {
            Ok(segments) => segments,
            Err(err) => {
                merge.error = Some(err.to_string());
                return;
            }
        };

        let new_words = split_words(&hypothesis_text(&segments));
        let lcp = longest_common_prefix(&utt.prev_words, &new_words);
        // Never shrinks: a word already reported stable stays stable even if
        // a later hypothesis disagrees (the module doc's deliberate trade).
        let agreed = lcp.max(utt.stable_count).min(new_words.len());
        if agreed > utt.stable_count {
            let words = new_words[utt.stable_count..agreed].join(" ");
            utt.stable_text.push(words.clone());
            merge.stable_parts.push(words);
            utt.stable_count = agreed;
        }
        let tail_start = utt.stable_count.min(new_words.len());
        merge.unstable = Some(new_words[tail_start..].join(" "));
        utt.prev_words = new_words;
    }

    /// Ends an utterance: one last transcription pass over the full buffer
    /// is authoritative, so everything not yet reported stable is force-
    /// committed, bypassing the two-pass agreement `run_pass` requires.
    ///
    /// Returns `Some(utt)` to mean "not actually finalised, hand it back" --
    /// either the burst was noise (shorter than `min_utterance`, so it was
    /// never transcribed at all and is simply dropped: that case returns
    /// `None` instead, there being nothing left to hand back) or the final
    /// pass errored, in which case the utterance is kept exactly as it was
    /// so the next frame retries finalising it.
    fn finalize(&mut self, utt: UtteranceState, merge: &mut Merge) -> Option<UtteranceState> {
        // Exclude the trailing silence run that triggered this finalisation
        // (or whatever pause happens to be in progress at `max_utterance`)
        // from the length check, so a short real burst followed by the
        // silence that ends it isn't inflated past `min_utterance` by that
        // silence's own duration.
        let trailing_silence_samples =
            duration_to_samples(Duration::from_millis(utt.silence_run_ms))
                .min(utt.buf.len() as u64);
        let speech_span_samples = utt.buf.len() as u64 - trailing_silence_samples;
        if speech_span_samples < self.min_utterance_samples {
            return None;
        }

        let started = Instant::now();
        let result = self.engine.transcribe(&utt.buf, SAMPLE_RATE_HZ);
        self.note_pass(started.elapsed());

        let segments = match result {
            Ok(segments) => segments,
            Err(err) => {
                merge.error = Some(err.to_string());
                let mut utt = utt;
                utt.finalize_attempts += 1;

                if utt.finalize_attempts >= MAX_FINALIZE_ATTEMPTS {
                    // Giving up matters most when the engine is a server on
                    // another machine: keeping the utterance buffered would grow
                    // memory and make every retry upload a larger recording,
                    // for ever. Surface the words already agreed, drop the rest,
                    // and start the next utterance clean. The error goes with it,
                    // so this is never silent.
                    let text = utt.stable_text.join(" ").trim().to_string();
                    if !text.is_empty() {
                        let start_ms = samples_to_ms(utt.start_samples);
                        let end_ms = samples_to_ms(utt.start_samples + utt.buf.len() as u64);
                        merge.finished = Some(Utterance {
                            text,
                            start_ms,
                            end_ms,
                        });
                    }
                    merge.unstable = Some(String::new());
                    return None;
                }

                // Wait an interval before trying again: a persistently broken
                // engine would otherwise be re-invoked on every frame.
                utt.finalize_failed = true;
                utt.samples_since_pass = 0;
                return Some(utt);
            }
        };

        let final_text = hypothesis_text(&segments);
        let final_words = split_words(&final_text);
        if final_words.len() > utt.stable_count {
            merge
                .stable_parts
                .push(final_words[utt.stable_count..].join(" "));
        }
        merge.unstable = Some(String::new());

        let start_ms = samples_to_ms(utt.start_samples);
        let end_ms = samples_to_ms(utt.start_samples + utt.buf.len() as u64);
        merge.finished = Some(Utterance {
            text: final_text,
            start_ms,
            end_ms,
        });
        None
    }
}

/// How many times finalising an utterance may fail before the stream gives up
/// on it. Three is enough to ride out a transient failure — a server restart, a
/// momentary network drop — without letting a lasting one pin audio in memory.
const MAX_FINALIZE_ATTEMPTS: u32 = 3;

/// Joins every returned segment's text into one hypothesis string. Real
/// adapters return at most one segment today, but a `Transcriber` is free to
/// return several; treating them as one hypothesis keeps this generic.
fn hypothesis_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn split_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// Normalises a word for comparison only: lowercased, with leading/trailing
/// punctuation stripped but internal characters kept (so `"don't"` stays
/// `"don't"`, not `"dont"`). Identical rule to the old `dedup::normalize_word`
/// this replaces.
fn normalize_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn longest_common_prefix(a: &[String], b: &[String]) -> usize {
    let mut i = 0;
    while i < a.len() && i < b.len() && normalize_word(&a[i]) == normalize_word(&b[i]) {
        i += 1;
    }
    i
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    (sum_sq / frame.len() as f32).sqrt()
}

fn duration_to_samples(d: Duration) -> u64 {
    (d.as_secs_f64() * SAMPLE_RATE_HZ as f64).round() as u64
}

fn samples_to_ms(samples: u64) -> u64 {
    samples * 1_000 / SAMPLE_RATE_HZ as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc_core::{EngineInfo, Track};
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::collections::VecDeque;
    use std::process::Command;

    use crate::transcriber::AsrError;
    use crate::voxtype_cli::VoxtypeCli;

    fn placeholder_segment(text: &str) -> Segment {
        Segment {
            track: Track::Selected,
            seq: 0,
            start_ms: 0,
            end_ms: 0,
            text: text.to_string(),
            translation: None,
            speaker: None,
            confidence: None,
            provisional: false,
        }
    }

    /// One scripted outcome per call to `transcribe`, in order. Running out
    /// of script returns an empty hypothesis rather than panicking, so a
    /// test only has to script as many calls as it cares about.
    enum Script {
        Hyp(&'static str),
        Err(&'static str),
        /// Sleeps before returning `Hyp`, to exercise the `lagging` path.
        Slow(Duration, &'static str),
    }

    struct ScriptedTranscriber {
        calls: RefCell<VecDeque<Script>>,
        /// Shared so a test can count invocations after handing the engine to
        /// the stream — which is how "a broken engine is not retried on every
        /// frame" becomes an assertion rather than a hope.
        invocations: Arc<AtomicUsize>,
    }

    impl ScriptedTranscriber {
        fn new(script: Vec<Script>) -> Self {
            Self {
                calls: RefCell::new(script.into()),
                invocations: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn counter(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.invocations)
        }
    }

    impl Transcriber for ScriptedTranscriber {
        fn transcribe(&self, _pcm: &[f32], _sample_rate: u32) -> Result<Vec<Segment>, AsrError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            match self.calls.borrow_mut().pop_front() {
                None => Ok(Vec::new()),
                Some(Script::Hyp(text)) => Ok(vec![placeholder_segment(text)]),
                Some(Script::Slow(dur, text)) => {
                    std::thread::sleep(dur);
                    Ok(vec![placeholder_segment(text)])
                }
                Some(Script::Err(msg)) => Err(AsrError::Io(std::io::Error::other(msg))),
            }
        }

        fn describe(&self) -> EngineInfo {
            EngineInfo {
                engine: "scripted".into(),
                model: "scripted".into(),
                language: "en".into(),
                backend: None,
            }
        }
    }

    fn tone(amplitude: f32, ms: u64) -> Vec<f32> {
        let n = (ms * SAMPLE_RATE_HZ as u64 / 1_000) as usize;
        (0..n)
            .map(|i| amplitude * ((i as f32 * 0.3).sin()))
            .collect()
    }

    fn silence(ms: u64) -> Vec<f32> {
        vec![0.0; (ms * SAMPLE_RATE_HZ as u64 / 1_000) as usize]
    }

    /// Generous bounds so passes/finalisations land on predictable 20ms-frame
    /// boundaries in the tests below without being so tight that scheduling
    /// jitter in `Slow`/timing tests flakes.
    fn relaxed_config() -> StreamConfig {
        StreamConfig {
            step: Duration::from_millis(100),
            silence_rms: 0.01,
            silence_hold: Duration::from_millis(10_000), // effectively off
            max_utterance: Duration::from_millis(10_000), // effectively off
            min_utterance: Duration::from_millis(40),
        }
    }

    #[test]
    fn a_word_agreed_twice_becomes_stable_exactly_once() {
        let engine = ScriptedTranscriber::new(vec![
            Script::Hyp("hello world"),
            Script::Hyp("hello world there"),
            Script::Hyp("hello world there now"),
        ]);
        let mut stream = TranscriptStream::new(engine, relaxed_config());

        // 100ms steps: push 300ms of continuous tone, one pass per 100ms.
        let u1 = stream.push(&tone(0.5, 100)).expect("first pass");
        assert_eq!(u1.stable, "", "needs two hypotheses to agree first");
        assert_eq!(u1.unstable, "hello world");

        let u2 = stream.push(&tone(0.5, 100)).expect("second pass");
        assert_eq!(u2.stable, "hello world");
        assert_eq!(u2.unstable, "there");

        let u3 = stream.push(&tone(0.5, 100)).expect("third pass");
        assert_eq!(u3.stable, "there");
        assert_eq!(u3.unstable, "now");

        // "hello" and "world" must never reappear in a later `stable`.
        assert!(!u3.stable.contains("hello"));
        assert!(!u3.stable.contains("world"));
    }

    #[test]
    fn a_word_that_changes_next_pass_is_not_stable_and_shows_in_unstable() {
        let engine =
            ScriptedTranscriber::new(vec![Script::Hyp("the cat sat"), Script::Hyp("the cat ran")]);
        let mut stream = TranscriptStream::new(engine, relaxed_config());

        let u1 = stream.push(&tone(0.5, 100)).expect("first pass");
        assert_eq!(u1.stable, "");
        assert_eq!(u1.unstable, "the cat sat");

        let u2 = stream.push(&tone(0.5, 100)).expect("second pass");
        assert_eq!(u2.stable, "the cat", "only the agreeing prefix commits");
        assert_eq!(u2.unstable, "ran", "the changed word surfaces as unstable");
        assert!(!u2.stable.contains("sat"));
        assert!(!u2.stable.contains("ran"));
    }

    #[test]
    fn finished_utterance_carries_the_final_pass_even_for_never_agreed_words() {
        let cfg = StreamConfig {
            step: Duration::from_millis(100),
            silence_hold: Duration::from_millis(100),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(40),
            ..relaxed_config()
        };
        // Only one mid-utterance pass happens before silence finalises, so
        // "actual final text" is never confirmed by a second agreeing pass
        // mid-utterance -- finalisation must still trust it completely.
        let engine = ScriptedTranscriber::new(vec![
            Script::Hyp("partial guess"),
            Script::Hyp("actual final text with extra words"),
        ]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let u1 = stream.push(&tone(0.5, 100)).expect("mid-utterance pass");
        assert_eq!(u1.unstable, "partial guess");
        assert_eq!(u1.stable, "");

        let u2 = stream.push(&silence(150)).expect("silence should finalise");
        let fin = u2.finished.expect("utterance must finalise");
        assert_eq!(fin.text, "actual final text with extra words");
        assert_eq!(
            u2.stable, "actual final text with extra words",
            "everything not yet stable is force-committed at finalisation"
        );
        assert_eq!(u2.unstable, "");
    }

    #[test]
    fn utterance_timestamps_are_monotonic_and_exclude_the_gap_between_them() {
        let cfg = StreamConfig {
            step: Duration::from_millis(10_000), // no mid-utterance passes
            silence_hold: Duration::from_millis(100),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(40),
            silence_rms: 0.01,
        };
        let engine = ScriptedTranscriber::new(vec![Script::Hyp("first"), Script::Hyp("second")]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let mut u1 = None;
        u1 = u1.or(stream.push(&tone(0.5, 400)));
        u1 = u1.or(stream.push(&silence(300))); // 100ms hold + 200ms pure gap
        let u1 = u1.expect("first utterance finalises");
        let fin1 = u1.finished.expect("finished");
        assert_eq!(fin1.start_ms, 0);
        // Recorded span includes the 100ms hold silence that triggered the
        // cut: 400ms speech + 100ms trailing silence.
        assert_eq!(fin1.end_ms, 500);

        let mut u2 = None;
        u2 = u2.or(stream.push(&tone(0.5, 300)));
        u2 = u2.or(stream.push(&silence(150)));
        let u2 = u2.expect("second utterance finalises");
        let fin2 = u2.finished.expect("finished");

        // The 200ms pure gap beyond the first utterance's hold silence must
        // not appear in the second utterance's span.
        assert_eq!(fin2.start_ms, 700, "gap silence excluded from next start");
        assert!(fin2.start_ms >= fin1.end_ms);
        assert!(fin2.end_ms > fin2.start_ms);
    }

    #[test]
    fn silence_shorter_than_hold_does_not_finalize_but_hold_does() {
        let cfg = StreamConfig {
            step: Duration::from_millis(10_000),
            silence_hold: Duration::from_millis(100),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(40),
            silence_rms: 0.01,
        };
        let engine = ScriptedTranscriber::new(vec![Script::Hyp("one continuous utterance")]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let mut saw_finish = false;
        if let Some(u) = stream.push(&tone(0.5, 200)) {
            saw_finish |= u.finished.is_some();
        }
        // 80ms < 100ms hold: must not finalise.
        if let Some(u) = stream.push(&silence(80)) {
            saw_finish |= u.finished.is_some();
        }
        assert!(!saw_finish, "silence under the hold must not finalise");

        // More speech resumes the SAME utterance (not split by the brief
        // pause): the eventual finalisation's start_ms must be 0.
        assert!(
            stream.push(&tone(0.5, 100)).is_none(),
            "more speech alone must not itself produce an update here"
        );
        // Now push silence at/over the hold.
        let u = stream
            .push(&silence(120))
            .expect("hold reached, must finalise");
        let fin = u.finished.expect("finalised");
        assert_eq!(
            fin.start_ms, 0,
            "the brief pause did not split the utterance"
        );
    }

    #[test]
    fn max_utterance_finalizes_with_continuous_speech_and_no_silence() {
        let cfg = StreamConfig {
            step: Duration::from_millis(10_000),
            silence_hold: Duration::from_millis(10_000),
            max_utterance: Duration::from_millis(300),
            min_utterance: Duration::from_millis(40),
            silence_rms: 0.01,
        };
        let engine = ScriptedTranscriber::new(vec![Script::Hyp("forced cut text")]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let mut finished = None;
        for _ in 0..5 {
            if let Some(u) = stream.push(&tone(0.5, 100)) {
                if u.finished.is_some() {
                    finished = u.finished;
                    break;
                }
            }
        }
        let fin = finished.expect("max_utterance must force a finalisation");
        assert_eq!(fin.text, "forced cut text");
        assert!(fin.end_ms >= 300);
    }

    #[test]
    fn silence_before_speech_never_starts_an_utterance() {
        let engine = ScriptedTranscriber::new(vec![Script::Hyp("should never be used")]);
        let mut stream = TranscriptStream::new(engine, relaxed_config());

        for _ in 0..10 {
            assert!(
                stream.push(&silence(100)).is_none(),
                "silence alone must never produce an update"
            );
        }
    }

    #[test]
    fn burst_shorter_than_min_utterance_produces_nothing() {
        let cfg = StreamConfig {
            step: Duration::from_millis(10_000), // no mid-burst pass
            silence_hold: Duration::from_millis(100),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(100),
            silence_rms: 0.01,
        };
        // If the burst were (wrongly) treated as a real utterance, this
        // would be returned and the test would catch it.
        let engine = ScriptedTranscriber::new(vec![Script::Hyp("should not be transcribed")]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let mut saw_anything = false;
        // 50ms speech + 100ms trailing silence: speech span (50ms) is well
        // under min_utterance (100ms), even though the buffered span (150ms)
        // is not.
        if stream.push(&tone(0.5, 50)).is_some() {
            saw_anything = true;
        }
        if stream.push(&silence(120)).is_some() {
            saw_anything = true;
        }
        assert!(
            !saw_anything,
            "a sub-min_utterance burst must be fully silent"
        );
    }

    #[test]
    fn every_sample_is_accounted_for_across_randomized_frame_sizes() {
        struct Xorshift32(u32);
        impl Xorshift32 {
            fn next_u32(&mut self) -> u32 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                self.0 = x;
                x
            }
            fn range(&mut self, lo: usize, hi: usize) -> usize {
                lo + (self.next_u32() as usize) % (hi - lo + 1)
            }
        }

        let cfg = StreamConfig {
            step: Duration::from_millis(200),
            silence_hold: Duration::from_millis(100),
            max_utterance: Duration::from_millis(500),
            min_utterance: Duration::from_millis(50),
            silence_rms: 0.01,
        };
        let max_utterance_samples = duration_to_samples(cfg.max_utterance) as usize;
        let engine = ScriptedTranscriber::new(Vec::new()); // always falls back to Ok(vec![])
        let mut stream = TranscriptStream::new(engine, cfg);
        let mut rng = Xorshift32(0xC0FFEE);

        let mut fed_total: u64 = 0;
        let mut utterances: Vec<Utterance> = Vec::new();
        for i in 0..400usize {
            let frame_len = match i % 7 {
                0 => 1,
                1 => 2,
                6 => max_utterance_samples * 2 + 37,
                _ => rng.range(1, max_utterance_samples / 3 + 1),
            };
            let is_speech = i % 3 != 0;
            let frame: Vec<f32> = (0..frame_len)
                .map(|j| {
                    if is_speech {
                        0.4 * (j as f32 * 0.37).sin()
                    } else {
                        0.0
                    }
                })
                .collect();
            fed_total += frame.len() as u64;
            if let Some(u) = stream.push(&frame) {
                utterances.extend(u.finished);
            }
        }
        if let Some(u) = stream.finish() {
            utterances.extend(u.finished);
        }

        // Internal bookkeeping must have seen exactly the samples fed in --
        // never fewer (lost) nor more (double-counted) -- and nothing must
        // be left stuck in the partial-frame accumulator after `finish`.
        assert_eq!(stream.total_samples_in, fed_total);
        assert!(stream.frame_acc.is_empty());
        assert!(
            stream.utterance.is_none(),
            "finish must resolve any in-flight utterance"
        );

        // Whatever utterances did get reported must be ordered, non-
        // overlapping, and within the total input span.
        let fed_total_ms = samples_to_ms(fed_total);
        for pair in utterances.windows(2) {
            assert!(pair[1].start_ms >= pair[0].end_ms);
        }
        for u in &utterances {
            assert!(u.end_ms <= fed_total_ms);
            assert!(u.start_ms <= u.end_ms);
        }
    }

    #[test]
    fn a_slow_pass_sets_lagging_and_stretches_the_interval() {
        // `step` and the push sizes are exact multiples of the 20ms RMS frame,
        // so each push completes whole frames and the pass-triggering sample
        // count lands exactly where the test expects.
        let cfg = StreamConfig {
            step: Duration::from_millis(40),
            silence_hold: Duration::from_millis(10_000),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(10),
            silence_rms: 0.01,
        };
        let engine = ScriptedTranscriber::new(vec![
            Script::Slow(Duration::from_millis(150), "slow pass"),
            Script::Hyp("fast pass"),
        ]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let u1 = stream.push(&tone(0.5, 40)).expect("first pass");
        assert!(u1.lagging, "a pass slower than `step` must set lagging");

        // The interval has stretched to about the duration of that pass, so
        // another 40ms of audio is no longer enough to trigger one. This is the
        // whole point: triggering on a fixed interval the engine cannot meet is
        // how the backlog grows without bound.
        assert!(
            stream.push(&tone(0.5, 40)).is_none(),
            "a stretched interval must suppress the next pass"
        );

        // Once enough audio has accumulated to cover the slow pass, it runs
        // again — and the engine is quick this time, so lagging clears.
        let u3 = stream
            .push(&tone(0.5, 160))
            .expect("a pass once the stretched interval is covered");
        assert!(!u3.lagging, "catching up must clear lagging again");

        // Audio integrity: every push survived, including the one that
        // deliberately ran no pass.
        let fin = stream
            .finish()
            .and_then(|u| u.finished)
            .expect("utterance should finalise on finish()");
        assert_eq!(fin.end_ms, 240, "40 + 40 + 160 ms of audio must all be kept");
    }

    /// A broken engine must not be re-invoked on every frame. Finalisation is
    /// attempted once, then waits an interval, so a persistent failure costs
    /// one process spawn per interval rather than one per 20ms frame.
    #[test]
    fn a_failed_finalisation_waits_before_retrying() {
        let cfg = StreamConfig {
            step: Duration::from_millis(200),
            silence_hold: Duration::from_millis(40),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(20),
            silence_rms: 0.01,
        };
        let engine = ScriptedTranscriber::new(vec![
            Script::Err("engine exploded"),
            Script::Hyp("finally working"),
        ]);
        let invocations = engine.counter();
        let mut stream = TranscriptStream::new(engine, cfg);

        // Speech, then enough silence to finalise. The first attempt fails.
        stream.push(&tone(0.5, 60));
        let failed = stream.push(&silence(40)).expect("a failed finalisation");
        assert!(failed.error.is_some(), "the failure must reach the caller");
        assert!(failed.finished.is_none(), "a failed pass must not finalise");

        // The next frames must not each trigger another attempt.
        let attempts_before = invocations.load(Ordering::SeqCst);
        for _ in 0..5 {
            stream.push(&silence(20));
        }
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            attempts_before,
            "a failed finalisation must not be retried on every frame"
        );

        // Once an interval of audio has passed, it tries again and succeeds.
        let ok = stream.push(&silence(200)).expect("a retried finalisation");
        assert!(ok.error.is_none(), "the retry should have succeeded");
        assert!(ok.finished.is_some(), "the utterance should now finalise");
    }

    /// An engine that never recovers — a transcription server that has gone
    /// away — must not pin audio in memory for the rest of the meeting, and
    /// must not make every retry upload a bigger recording.
    #[test]
    fn a_permanently_broken_engine_gives_up_and_keeps_what_was_agreed() {
        let cfg = StreamConfig {
            step: Duration::from_millis(40),
            silence_hold: Duration::from_millis(40),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(20),
            silence_rms: 0.01,
        };
        // Two agreeing passes make "hello world" stable, then the engine dies.
        let engine = ScriptedTranscriber::new(vec![
            Script::Hyp("hello world"),
            Script::Hyp("hello world"),
            Script::Err("server gone"),
            Script::Err("server gone"),
            Script::Err("server gone"),
            Script::Err("server gone"),
        ]);
        let invocations = engine.counter();
        let mut stream = TranscriptStream::new(engine, cfg);

        stream.push(&tone(0.5, 40));
        stream.push(&tone(0.5, 40));

        // Now silence, so it tries to finalise, and keeps failing.
        let mut finished = None;
        for _ in 0..60 {
            if let Some(update) = stream.push(&silence(40)) {
                if update.finished.is_some() {
                    finished = update.finished;
                    break;
                }
            }
        }

        let finished = finished.expect("the stream must give up rather than retry for ever");
        assert_eq!(
            finished.text, "hello world",
            "the words already agreed must survive giving up"
        );
        assert!(
            invocations.load(Ordering::SeqCst) <= 6,
            "giving up must bound the attempts, saw {}",
            invocations.load(Ordering::SeqCst)
        );

        // The buffer was reset, so the next utterance starts clean rather than
        // carrying the abandoned audio.
        let next = stream.push(&tone(0.5, 40));
        assert!(
            next.is_none() || next.unwrap().finished.is_none(),
            "a fresh utterance must not immediately finalise the old audio"
        );
    }

    #[test]
    fn transcriber_error_surfaces_and_the_next_pass_still_works() {
        let cfg = StreamConfig {
            step: Duration::from_millis(100),
            silence_hold: Duration::from_millis(10_000),
            max_utterance: Duration::from_millis(10_000),
            min_utterance: Duration::from_millis(40),
            silence_rms: 0.01,
        };
        let engine = ScriptedTranscriber::new(vec![
            Script::Err("engine exploded"),
            Script::Hyp("recovered text"),
        ]);
        let mut stream = TranscriptStream::new(engine, cfg);

        let u1 = stream
            .push(&tone(0.5, 100))
            .expect("error must still produce an update");
        assert_eq!(
            u1.error.as_deref(),
            Some("i/o error running voxtype: engine exploded")
        );
        assert_eq!(
            u1.stable, "",
            "a failed pass must not advance the stable prefix"
        );
        assert_eq!(
            u1.unstable, "",
            "a failed pass must not publish a hypothesis"
        );
        assert!(u1.finished.is_none(), "a failed pass must not finalise");

        let u2 = stream
            .push(&tone(0.5, 100))
            .expect("next pass should succeed");
        assert!(u2.error.is_none());
        assert_eq!(u2.unstable, "recovered text");
    }

    // --- End-to-end against the real voxtype binary -------------------

    /// Generates several short sentences separated by true digital silence,
    /// feeds the result through `TranscriptStream<VoxtypeCli>` in 100ms
    /// frames (simulating realtime capture, sleeping out any slack so wall
    /// time tracks audio time), and checks the transcript against what was
    /// spoken. Needs `espeak-ng`, `ffmpeg` and a working voxtype install
    /// with `base.en` downloaded; run explicitly with:
    /// `cargo test -p fc-asr -- --ignored end_to_end_realtime_against_real_voxtype --nocapture`
    #[test]
    #[ignore = "requires espeak-ng, ffmpeg and a real voxtype install with base.en downloaded"]
    fn end_to_end_realtime_against_real_voxtype() {
        let sentences = [
            "The quarterly roadmap review is scheduled for next Tuesday.",
            "We still need owners for the migration work.",
            "Customer feedback from the pilot was mostly positive.",
            "Engineering will present the latency numbers on Thursday.",
            "Finance asked for an updated budget by end of month.",
            "Let's circle back once the design doc is finalized.",
        ];
        let keywords = [
            "roadmap",
            "migration",
            "feedback",
            "latency",
            "budget",
            "circle",
        ];

        let dir = tempfile::tempdir().expect("tempdir");
        let mut pcm: Vec<f32> = Vec::new();
        // Leading silence, like a real capture starting before anyone speaks.
        pcm.extend(vec![0.0_f32; SAMPLE_RATE_HZ as usize / 2]);

        for (i, sentence) in sentences.iter().enumerate() {
            let raw_wav = dir.path().join(format!("raw_{i}.wav"));
            let sp_wav = dir.path().join(format!("sp_{i}.wav"));
            let status = Command::new("espeak-ng")
                .args(["-v", "en-us", "-s", "150", "-w"])
                .arg(&raw_wav)
                .arg(sentence)
                .status()
                .expect("espeak-ng must be installed for this test");
            assert!(status.success(), "espeak-ng failed on sentence {i}");

            let status = Command::new("ffmpeg")
                .arg("-y")
                .arg("-i")
                .arg(&raw_wav)
                .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
                .arg(&sp_wav)
                .args(["-loglevel", "error"])
                .status()
                .expect("ffmpeg must be installed for this test");
            assert!(status.success(), "ffmpeg failed on sentence {i}");

            let mut reader = hound::WavReader::open(&sp_wav).expect("read generated wav");
            let spec = reader.spec();
            assert_eq!(spec.sample_rate, 16_000);
            assert_eq!(spec.channels, 1);
            let sentence_pcm: Vec<f32> = reader
                .samples::<i16>()
                .map(|s| s.expect("sample") as f32 / i16::MAX as f32)
                .collect();
            pcm.extend(sentence_pcm);
            // True digital silence between utterances, comfortably past the
            // default silence_hold (0.4s), so each sentence is its own
            // utterance.
            pcm.extend(vec![0.0_f32; SAMPLE_RATE_HZ as usize]);
        }

        let cli = VoxtypeCli::new().with_model("base.en").with_language("en");
        let mut stream = TranscriptStream::new(cli, StreamConfig::default());

        let frame_samples = SAMPLE_RATE_HZ as usize / 10; // 100ms
        let frame_duration = Duration::from_millis(100);
        let session_start = Instant::now();
        let mut busy = Duration::ZERO;
        let mut finished_texts: Vec<String> = Vec::new();
        let mut latencies: Vec<Duration> = Vec::new();

        // Audio is produced on its own thread, paced against an absolute
        // deadline, and consumed here — which is how the application works: a
        // `parec` process keeps producing regardless of how long a pass takes,
        // and frames wait in an unbounded channel.
        //
        // Feeding synchronously instead would stall the audio clock whenever a
        // pass overran its frame budget, so the measured latency would be the
        // harness's own drift rather than how far behind the speaker the text
        // actually is. It read as a steadily growing lag, which is exactly what
        // a pipeline that cannot keep up looks like — worth not measuring by
        // accident.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        let frames: Vec<Vec<f32>> = pcm.chunks(frame_samples).map(<[f32]>::to_vec).collect();
        let feeder = std::thread::spawn(move || {
            let start = Instant::now();
            for (index, frame) in frames.into_iter().enumerate() {
                let due = start + frame_duration * (index as u32);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });

        while let Ok(frame) = rx.recv() {
            let before = Instant::now();
            let update = stream.push(&frame);
            busy += before.elapsed();
            if let Some(update) = update {
                if let Some(err) = &update.error {
                    eprintln!("transcription error during e2e run: {err}");
                }
                if let Some(fin) = update.finished {
                    let audio_time = session_start + Duration::from_millis(fin.end_ms);
                    let latency = Instant::now().saturating_duration_since(audio_time);
                    latencies.push(latency);
                    finished_texts.push(fin.text);
                }
            }
        }
        let _ = feeder.join();
        if let Some(update) = stream.finish() {
            if let Some(fin) = update.finished {
                let latency = Instant::now()
                    .saturating_duration_since(session_start + Duration::from_millis(fin.end_ms));
                latencies.push(latency);
                finished_texts.push(fin.text);
            }
        }

        let total_wall = session_start.elapsed();
        let duty = busy.as_secs_f64() / total_wall.as_secs_f64();
        eprintln!("--- end-to-end realtime numbers ---");
        eprintln!(
            "utterances expected: {}, finalised: {}",
            sentences.len(),
            finished_texts.len()
        );
        eprintln!(
            "wall time: {total_wall:?}, busy time: {busy:?}, duty cycle: {:.1}%",
            duty * 100.0
        );
        for (i, (text, latency)) in finished_texts.iter().zip(latencies.iter()).enumerate() {
            eprintln!("utterance {i}: latency {latency:?}, text: {text:?}");
        }

        let expected_words: usize = sentences.iter().map(|s| s.split_whitespace().count()).sum();
        let got_words: usize = finished_texts
            .iter()
            .map(|t| t.split_whitespace().count())
            .sum();
        eprintln!("expected ~{expected_words} words, transcribed {got_words} words");

        assert_eq!(
            finished_texts.len(),
            sentences.len(),
            "expected one finalised utterance per sentence, got: {finished_texts:?}"
        );
        for (i, text) in finished_texts.iter().enumerate() {
            let lower = text.to_lowercase();
            assert!(
                lower.contains(keywords[i]),
                "utterance {i} missing expected keyword {:?}: {text:?}",
                keywords[i]
            );
        }
    }
}
