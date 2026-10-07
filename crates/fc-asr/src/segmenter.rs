//! Turns a continuous stream of 16 kHz mono f32 PCM into [`Chunk`]s for the
//! ASR worker.
//!
//! voxtype has no live transcript feed (ARCHITECTURE.md §1), so this module is
//! what makes the app feel realtime at all: it decides when there is "enough"
//! audio to hand to `voxtype -q transcribe`, and it does that without ever
//! throwing audio away, because a dropped chunk is a hole in a meeting
//! transcript that cannot be recovered.
//!
//! Two cuts happen on independent schedules over the same incoming audio:
//! - A **committed** cut, preferring a silence boundary once past the target
//!   length, forced at the max length regardless of silence. Committed chunks
//!   carry a short overlap tail into the next chunk so a word split across the
//!   boundary is never truncated in either half (see [`dedup`](crate::dedup)
//!   for how the overlap is reconciled on the transcript side).
//! - An optional **provisional** cut, short and fixed-length, so the UI has
//!   something to show within ~2 s. Provisional output is superseded by the
//!   committed chunk that later covers the same audio and is never persisted.
//!
//! Backpressure (§3 "Backpressure") grows the committed target length and
//! turns provisional chunks off, so a slow transcriber gets fewer, longer
//! chunks to work through instead of a backlog. It never discards audio: the
//! committed buffer keeps every sample until a cut actually happens.

use fc_core::Track;

/// All audio this crate works with is 16 kHz mono, matching voxtype's own
/// `transcribe` input contract (ARCHITECTURE.md §1). [`Chunk::pcm`] is always
/// at this rate; nothing in [`Chunk`] carries a sample rate because there is
/// only ever one.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Width of the window used for RMS/silence detection. Small enough that a
/// cut lands within one window of the true silence boundary, large enough
/// that a single loud sample doesn't flip the verdict.
const FRAME_MS: u64 = 20;
const FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ as u64 * FRAME_MS / 1_000) as usize;

/// A ladder of target chunk lengths backpressure climbs under load. Level 0 is
/// the steady-state target; the top level is reached before `max_chunk_ms`
/// forces a cut regardless. The default matches the exact numbers called out
/// in ARCHITECTURE.md §3: 7 s → 10 s → 15 s.
const DEFAULT_GROWTH_LADDER_MS: [u64; 3] = [7_000, 10_000, 15_000];

/// Tunables for [`Segmenter`]. Defaults match ARCHITECTURE.md §3.
#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// Never cut a committed chunk shorter than this, even on silence.
    /// `flush()` is the one exception: it returns whatever is left, however
    /// short, because there is nowhere else for that audio to go.
    pub min_chunk_ms: u64,
    /// Steady-state and backpressure-grown committed target lengths, in
    /// ascending order. `growth_ladder_ms[0]` is the length used with no
    /// backpressure. Must be non-empty; [`Default`] gives `[7000, 10000,
    /// 15000]`.
    pub growth_ladder_ms: Vec<u64>,
    /// Hard ceiling: a committed cut happens here even mid-sentence, with no
    /// silence required. Keeps one slow/noisy stretch of audio from growing a
    /// chunk without bound.
    pub max_chunk_ms: u64,
    /// How much trailing audio a committed cut carries into the next chunk,
    /// so a word spanning the boundary appears whole in both chunks.
    pub overlap_ms: u64,
    /// RMS below this is "silence" for cut-boundary purposes. Matches
    /// voxtype's own `vad_threshold` default; re-verify against the installed
    /// voxtype version if this ever needs to track it exactly, since it's not
    /// exposed by `voxtype config`'s current output.
    pub silence_rms_threshold: f32,
    /// How long the trailing silence run must be, once past the target
    /// length, before a committed cut is taken there instead of waiting for
    /// `max_chunk_ms`.
    pub silence_hangover_ms: u64,
    /// Whether the two-tier provisional pass runs at all. Off saves half the
    /// transcription work; backpressure also turns it off temporarily (see
    /// [`Segmenter::grow_target`]).
    pub provisional_enabled: bool,
    /// Length of a provisional chunk. Spec range is 1.5-2 s; the default sits
    /// in the middle.
    pub provisional_chunk_ms: u64,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            min_chunk_ms: 1_500,
            growth_ladder_ms: DEFAULT_GROWTH_LADDER_MS.to_vec(),
            max_chunk_ms: 15_000,
            overlap_ms: 500,
            silence_rms_threshold: 0.01,
            silence_hangover_ms: 300,
            provisional_enabled: true,
            provisional_chunk_ms: 1_800,
        }
    }
}

/// One stretch of PCM ready for the ASR worker.
///
/// `start_ms` is relative to the start of this track's session, assigned by
/// the segmenter as it counts samples in; it is not wall-clock time. `seq` is
/// one monotonic counter per `Segmenter` shared by committed and provisional
/// chunks alike, so ordering between them (e.g. "this provisional chunk was
/// superseded by that committed one") survives being read out of order.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub track: Track,
    pub seq: u64,
    pub pcm: Vec<f32>,
    pub start_ms: u64,
    pub provisional: bool,
}

impl Chunk {
    /// Duration implied by sample count at [`SAMPLE_RATE_HZ`].
    pub fn duration_ms(&self) -> u64 {
        (self.pcm.len() as u64 * 1_000) / SAMPLE_RATE_HZ as u64
    }
}

fn ms_to_samples(ms: u64) -> usize {
    ((ms * SAMPLE_RATE_HZ as u64) / 1_000) as usize
}

fn samples_to_ms(samples: usize) -> u64 {
    (samples as u64 * 1_000) / SAMPLE_RATE_HZ as u64
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    (sum_sq / frame.len() as f32).sqrt()
}

/// Streaming chunker for one audio track. One instance per track (selected
/// source, and optionally the microphone), since cut decisions and sequence
/// numbers are per-track.
pub struct Segmenter {
    track: Track,
    cfg: SegmenterConfig,
    next_seq: u64,

    // Committed buffer: holds everything since the last committed cut,
    // including the overlap tail carried from the previous one.
    main_buf: Vec<f32>,
    main_buf_start_ms: u64,
    growth_level: usize,

    // Provisional buffer: independent short-window accumulator. Cleared
    // (not flushed as a chunk) whenever provisional is inactive, since its
    // audio is never lost -- it's already in `main_buf`.
    provisional_active: bool,
    prov_buf: Vec<f32>,
    prov_buf_start_ms: u64,

    // RMS bookkeeping for silence-boundary cuts.
    frame_acc: Vec<f32>,
    silence_run_ms: u64,

    // Absolute position in samples, used to re-anchor the provisional
    // buffer's start time when provisional resumes after backpressure
    // recovers.
    total_samples_in: u64,
}

impl Segmenter {
    pub fn new(track: Track, cfg: SegmenterConfig) -> Self {
        let provisional_active = cfg.provisional_enabled;
        Self {
            track,
            next_seq: 0,
            main_buf: Vec::new(),
            main_buf_start_ms: 0,
            growth_level: 0,
            provisional_active,
            prov_buf: Vec::new(),
            prov_buf_start_ms: 0,
            frame_acc: Vec::with_capacity(FRAME_SAMPLES),
            silence_run_ms: 0,
            total_samples_in: 0,
            cfg,
        }
    }

    fn current_target_ms(&self) -> u64 {
        self.cfg
            .growth_ladder_ms
            .get(self.growth_level)
            .copied()
            .unwrap_or(self.cfg.max_chunk_ms)
    }

    fn next_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Feeds new samples in. May return zero, one, or several chunks if
    /// `samples` spans more than one cut.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Chunk> {
        let mut out = Vec::new();
        for &s in samples {
            self.main_buf.push(s);
            if self.provisional_active {
                self.prov_buf.push(s);
            }
            self.frame_acc.push(s);
            self.total_samples_in += 1;

            if self.frame_acc.len() == FRAME_SAMPLES {
                self.process_frame(&mut out);
                self.frame_acc.clear();
            }
        }
        out
    }

    fn process_frame(&mut self, out: &mut Vec<Chunk>) {
        let r = rms(&self.frame_acc);
        if r < self.cfg.silence_rms_threshold {
            self.silence_run_ms += FRAME_MS;
        } else {
            self.silence_run_ms = 0;
        }

        let main_dur_ms = samples_to_ms(self.main_buf.len());
        let forced = main_dur_ms >= self.cfg.max_chunk_ms;
        let silence_cut = main_dur_ms >= self.current_target_ms()
            && main_dur_ms >= self.cfg.min_chunk_ms
            && self.silence_run_ms >= self.cfg.silence_hangover_ms;
        if forced || silence_cut {
            out.push(self.cut_committed());
        }

        if self.provisional_active {
            let prov_dur_ms = samples_to_ms(self.prov_buf.len());
            if prov_dur_ms >= self.cfg.provisional_chunk_ms {
                out.push(self.cut_provisional());
            }
        }
    }

    fn cut_committed(&mut self) -> Chunk {
        let seq = self.next_seq();
        let start_ms = self.main_buf_start_ms;
        let pcm = std::mem::take(&mut self.main_buf);

        // Carry the overlap tail into the next chunk.
        let overlap_samples = ms_to_samples(self.cfg.overlap_ms).min(pcm.len());
        let tail_start = pcm.len() - overlap_samples;
        self.main_buf_start_ms = start_ms + samples_to_ms(tail_start);
        self.main_buf = pcm[tail_start..].to_vec();
        // The tail may itself be silence or speech; reset the hangover timer
        // rather than guess, so the next cut doesn't fire early on silence
        // that actually belongs to the carried-over tail. Documented
        // trade-off: this can delay the next silence-boundary cut by up to
        // `silence_hangover_ms`, never bring one forward.
        self.silence_run_ms = 0;

        Chunk {
            track: self.track,
            seq,
            pcm,
            start_ms,
            provisional: false,
        }
    }

    fn cut_provisional(&mut self) -> Chunk {
        let seq = self.next_seq();
        let start_ms = self.prov_buf_start_ms;
        let pcm = std::mem::take(&mut self.prov_buf);
        self.prov_buf_start_ms = start_ms + samples_to_ms(pcm.len());
        Chunk {
            track: self.track,
            seq,
            pcm,
            start_ms,
            provisional: true,
        }
    }

    /// Tells the segmenter transcription is falling behind: grows the
    /// committed target one rung (7s -> 10s -> 15s by default) and, as soon
    /// as any growth is in effect, stops provisional chunks so the ASR worker
    /// isn't doing double work while it's catching up. No audio is ever
    /// discarded by this -- committed chunks simply get longer and less
    /// frequent, so fewer of them queue up.
    pub fn grow_target(&mut self) {
        if self.growth_level + 1 < self.cfg.growth_ladder_ms.len() {
            self.growth_level += 1;
        }
        if self.growth_level > 0 {
            self.provisional_active = false;
            self.prov_buf.clear();
        }
    }

    /// The inverse of [`grow_target`](Self::grow_target): call once the ASR
    /// queue has caught up. Steps the target back down one rung; provisional
    /// chunks resume only once fully back at rung 0, and only if the config
    /// had them enabled in the first place.
    pub fn shrink_target(&mut self) {
        if self.growth_level > 0 {
            self.growth_level -= 1;
        }
        if self.growth_level == 0 && self.cfg.provisional_enabled {
            self.provisional_active = true;
            self.prov_buf_start_ms = samples_to_ms(self.total_samples_in as usize);
        }
    }

    /// End of session (or end of a track): returns whatever committed audio
    /// is still buffered, however short -- `min_chunk_ms` does not apply
    /// here, because there is no later chunk for this audio to join.
    ///
    /// Any leftover provisional buffer is dropped, not emitted: that audio is
    /// already covered by the committed chunk this returns, so emitting it
    /// too would just be duplicate work for the ASR worker on a session
    /// that's already ending.
    pub fn flush(&mut self) -> Vec<Chunk> {
        let mut out = Vec::new();
        if !self.main_buf.is_empty() {
            let seq = self.next_seq();
            let start_ms = self.main_buf_start_ms;
            let pcm = std::mem::take(&mut self.main_buf);
            out.push(Chunk {
                track: self.track,
                seq,
                pcm,
                start_ms,
                provisional: false,
            });
        }
        self.prov_buf.clear();
        self.frame_acc.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `amplitude` 0.0 is silence under the default threshold; anything
    /// meaningfully above `silence_rms_threshold` reads as speech.
    fn tone(amplitude: f32, ms: u64) -> Vec<f32> {
        let n = ms_to_samples(ms);
        (0..n)
            .map(|i| amplitude * ((i as f32 * 0.3).sin()))
            .collect()
    }

    fn silence(ms: u64) -> Vec<f32> {
        vec![0.0; ms_to_samples(ms)]
    }

    fn total_samples(chunks: &[Chunk]) -> usize {
        chunks.iter().map(|c| c.pcm.len()).sum()
    }

    #[test]
    fn min_length_is_respected_even_with_early_silence() {
        let cfg = SegmenterConfig {
            provisional_enabled: false,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        // Speech then silence well past hangover, but short of min_chunk_ms.
        let mut chunks = seg.push(&tone(0.5, 500));
        chunks.extend(seg.push(&silence(800)));
        assert!(
            chunks.is_empty(),
            "must not cut below min_chunk_ms even on a long silence run"
        );
    }

    #[test]
    fn cuts_on_silence_boundary_once_past_target() {
        let cfg = SegmenterConfig {
            growth_ladder_ms: vec![2_000, 3_000, 4_000],
            min_chunk_ms: 500,
            silence_hangover_ms: 200,
            provisional_enabled: false,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        let mut chunks = seg.push(&tone(0.5, 2_200)); // past target (2000ms)
        chunks.extend(seg.push(&silence(400))); // > hangover (200ms)
        assert_eq!(chunks.len(), 1, "expected exactly one silence-boundary cut");
        assert!(!chunks[0].provisional);
        // Cut should land once the hangover elapsed, not at max.
        assert!(chunks[0].duration_ms() < 4_000);
        assert!(chunks[0].duration_ms() >= 2_000);
    }

    #[test]
    fn forces_cut_at_max_length_without_silence() {
        let cfg = SegmenterConfig {
            growth_ladder_ms: vec![2_000],
            max_chunk_ms: 3_000,
            min_chunk_ms: 500,
            provisional_enabled: false,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        // Continuous speech, never silent, well past max.
        let chunks = seg.push(&tone(0.5, 3_200));
        assert!(!chunks.is_empty(), "must force a cut at max length");
        for c in &chunks {
            assert!(c.duration_ms() <= 3_020, "cut must not exceed max by much");
        }
    }

    #[test]
    fn overlap_tail_present_in_next_chunk() {
        let cfg = SegmenterConfig {
            growth_ladder_ms: vec![1_000],
            max_chunk_ms: 1_000,
            min_chunk_ms: 200,
            overlap_ms: 300,
            provisional_enabled: false,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        let chunks = seg.push(&tone(0.5, 2_500));
        assert!(chunks.len() >= 2, "expected multiple forced cuts");
        for pair in chunks.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            // The next chunk's start must be at or before the previous
            // chunk's end -- i.e. overlap, never a gap.
            assert!(
                b.start_ms <= a.start_ms + a.duration_ms(),
                "chunk {} must start at/before chunk {} ends (no gap)",
                b.seq,
                a.seq
            );
        }
    }

    #[test]
    fn flush_returns_buffered_audio_however_short() {
        let mut seg = Segmenter::new(Track::Selected, SegmenterConfig::default());
        seg.push(&tone(0.5, 100));
        let flushed = seg.flush();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].pcm.len(), ms_to_samples(100));
        // A second flush on an empty buffer yields nothing.
        assert!(seg.flush().is_empty());
    }

    #[test]
    fn flush_on_empty_segmenter_yields_nothing() {
        let mut seg = Segmenter::new(Track::Selected, SegmenterConfig::default());
        assert!(seg.flush().is_empty());
    }

    #[test]
    fn growing_target_climbs_the_ladder_and_disables_provisional() {
        let cfg = SegmenterConfig {
            growth_ladder_ms: vec![1_000, 2_000, 3_000],
            max_chunk_ms: 3_000,
            min_chunk_ms: 200,
            provisional_enabled: true,
            provisional_chunk_ms: 400,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        assert_eq!(seg.current_target_ms(), 1_000);

        seg.grow_target();
        assert_eq!(seg.current_target_ms(), 2_000);
        assert!(!seg.provisional_active, "provisional must stop once grown");

        seg.grow_target();
        assert_eq!(seg.current_target_ms(), 3_000);
        seg.grow_target(); // already at top rung, must not go further
        assert_eq!(seg.current_target_ms(), 3_000);

        seg.shrink_target();
        assert_eq!(seg.current_target_ms(), 2_000);
        assert!(
            !seg.provisional_active,
            "still grown, provisional stays off"
        );

        seg.shrink_target();
        assert_eq!(seg.current_target_ms(), 1_000);
        assert!(
            seg.provisional_active,
            "back at rung 0, provisional resumes"
        );

        seg.shrink_target(); // already at bottom, must not panic/underflow
        assert_eq!(seg.current_target_ms(), 1_000);
    }

    #[test]
    fn provisional_chunks_emitted_before_committed_supersedes() {
        let cfg = SegmenterConfig {
            growth_ladder_ms: vec![5_000],
            max_chunk_ms: 5_000,
            min_chunk_ms: 500,
            provisional_enabled: true,
            provisional_chunk_ms: 1_000,
            silence_hangover_ms: 100_000, // effectively disable silence cuts
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        let chunks = seg.push(&tone(0.5, 2_100));
        let provisional: Vec<_> = chunks.iter().filter(|c| c.provisional).collect();
        assert_eq!(provisional.len(), 2, "two 1s provisional chunks expected");
        assert!(
            chunks.iter().all(|c| c.provisional),
            "no committed cut is expected yet at 2.1s with a 5s target"
        );
    }

    #[test]
    fn provisional_disabled_means_no_provisional_chunks_ever() {
        let cfg = SegmenterConfig {
            provisional_enabled: false,
            growth_ladder_ms: vec![5_000],
            max_chunk_ms: 5_000,
            silence_hangover_ms: 100_000,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);
        let chunks = seg.push(&tone(0.5, 3_000));
        assert!(chunks.iter().all(|c| !c.provisional));
    }

    #[test]
    fn no_sample_is_ever_lost() {
        let cfg = SegmenterConfig {
            growth_ladder_ms: vec![1_300, 1_700, 2_100],
            max_chunk_ms: 2_100,
            min_chunk_ms: 400,
            overlap_ms: 250,
            silence_hangover_ms: 150,
            provisional_enabled: true,
            provisional_chunk_ms: 900,
            ..Default::default()
        };
        let mut seg = Segmenter::new(Track::Selected, cfg);

        let mut input_ms: u64 = 0;
        let mut chunks = Vec::new();
        // Alternate speech and silence to exercise both cut paths.
        for _ in 0..6 {
            chunks.extend(seg.push(&tone(0.5, 700)));
            input_ms += 700;
            chunks.extend(seg.push(&silence(400)));
            input_ms += 400;
        }
        chunks.extend(seg.flush());

        // Coverage check on committed chunks only: sorted by start_ms, the
        // next chunk must never start after the previous one ends (no gap --
        // overlap is fine, a hole is not), and the final committed chunk must
        // reach at least as far as all audio that was pushed.
        let mut committed: Vec<_> = chunks.iter().filter(|c| !c.provisional).collect();
        committed.sort_by_key(|c| c.start_ms);
        for pair in committed.windows(2) {
            assert!(
                pair[1].start_ms <= pair[0].start_ms + pair[0].duration_ms(),
                "gap between committed chunks: no audio may be lost"
            );
        }
        let last = committed.last().expect("at least one committed chunk");
        assert!(
            last.start_ms + last.duration_ms() >= input_ms,
            "committed coverage ({} ms) must reach the full input ({} ms)",
            last.start_ms + last.duration_ms(),
            input_ms
        );

        let _ = total_samples(&chunks); // sanity: doesn't panic / overflow
    }
}
