//! WebVTT (`.vtt`): a `WEBVTT` header, numbered cues, `HH:MM:SS.mmm -->
//! HH:MM:SS.mmm` timing. Unlike SRT, VTT has a `NOTE` comment block, so this
//! is the one caption format that can honor [`ExportOptions::metadata`].
//!
//! Cue timing is left exactly as the segments give it: WebVTT allows
//! overlapping cues, and two people talking over each other is information
//! the format can carry (SRT cannot, and `srt.rs` clamps for that reason).

use fc_core::{Conversation, Segment};

use crate::cues;
use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(
    conversation: &Conversation,
    segments: &[Segment],
    options: &ExportOptions,
) -> String {
    let mut out = String::from("WEBVTT\n\n");

    if options.metadata {
        out.push_str(&format!(
            "NOTE\n{}\nstarted: {}\nengine: {} ({})\n\n",
            note_safe(&conversation.title),
            fc_core::time::rfc3339(conversation.started_at),
            note_safe(&conversation.engine.engine),
            note_safe(&conversation.engine.model),
        ));
    }

    let mut n = 1u32;
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
            out.push_str(&format!(
                "{n}\n{} --> {}\n{text}\n\n",
                timestamp::vtt(start),
                timestamp::vtt(end)
            ));
            n += 1;
        }
    }

    out
}

/// Makes a value safe to write inside a `NOTE` block.
///
/// WebVTT gives a comment no escape syntax at all, and both of the things
/// that break one are ordinary in a conversation title: a blank line ends the
/// NOTE, so everything after it is parsed as cue syntax and the file stops
/// being a transcript; and `-->` makes the line read as cue timing. The
/// characters are replaced rather than escaped because there is nothing to
/// escape them with.
fn note_safe(value: &str) -> String {
    fc_core::single_line(value).replace("-->", "\u{2192}")
}
