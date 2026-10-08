//! SubRip (`.srt`): numbered cues, `HH:MM:SS,mmm --> HH:MM:SS,mmm` timing.
//!
//! SRT has no comment syntax, so [`ExportOptions::metadata`] has nothing to
//! attach to and is a no-op here -- WebVTT's `NOTE` block covers that case.

use fc_core::Segment;

use crate::cues;
use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(segments: &[Segment], options: &ExportOptions) -> String {
    let mut cues = collect(segments, options);
    clamp_overlaps(&mut cues);

    let mut out = String::new();
    for (n, (start, end, text)) in (1u32..).zip(cues) {
        out.push_str(&format!(
            "{n}\n{} --> {}\n{text}\n\n",
            timestamp::srt(start),
            timestamp::srt(end)
        ));
    }
    out
}

fn collect(segments: &[Segment], options: &ExportOptions) -> Vec<(u64, u64, String)> {
    let mut cues = Vec::new();
    for seg in segments {
        if seg.is_blank() {
            continue;
        }
        for (i, (start, end, chunk)) in cues::split(seg.start_ms, seg.end_ms, &seg.text)
            .into_iter()
            .enumerate()
        {
            let text = if options.speakers && i == 0 {
                format!("{}: {chunk}", seg.speaker_label())
            } else {
                chunk
            };
            cues.push((start, end, text));
        }
    }
    cues
}

/// Pulls each cue's end back to the next cue's start.
///
/// SubRip has no concept of concurrent cues. Segments arrive ordered by
/// `(start_ms, seq)` (the store's `load_segments`), and with the microphone
/// track on (D2) the two tracks interleave, so one cue's end routinely passes
/// the next cue's start — one speaker is still mid-sentence when the other
/// begins. Shown two overlapping cues, a player drops one, which loses a line
/// of the transcript rather than merely mistiming it. WebVTT permits overlap
/// and keeps its own timing untouched.
///
/// A cue is never pulled back past its own start: a zero-length cue is still
/// a cue a player can render, an inverted one is not.
fn clamp_overlaps(cues: &mut [(u64, u64, String)]) {
    for i in 0..cues.len().saturating_sub(1) {
        let next_start = cues[i + 1].0;
        if cues[i].1 > next_start {
            cues[i].1 = next_start.max(cues[i].0);
        }
    }
}
