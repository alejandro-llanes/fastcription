//! SubRip (`.srt`): numbered cues, `HH:MM:SS,mmm --> HH:MM:SS,mmm` timing.
//!
//! SRT has no comment syntax, so [`ExportOptions::metadata`] has nothing to
//! attach to and is a no-op here -- WebVTT's `NOTE` block covers that case.

use fc_core::Segment;

use crate::cues;
use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(segments: &[Segment], options: &ExportOptions) -> String {
    let mut out = String::new();
    let mut n = 1u32;
    for seg in segments {
        if seg.is_blank() {
            continue;
        }
        let cue_parts = cues::split(seg.start_ms, seg.end_ms, seg.text.trim());
        for (i, (start, end, chunk)) in cue_parts.iter().enumerate() {
            let text = if options.speakers && i == 0 {
                format!("{}: {chunk}", seg.speaker_label())
            } else {
                chunk.clone()
            };
            out.push_str(&format!(
                "{n}\n{} --> {}\n{text}\n\n",
                timestamp::srt(*start),
                timestamp::srt(*end)
            ));
            n += 1;
        }
    }
    out
}
