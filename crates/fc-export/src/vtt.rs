//! WebVTT (`.vtt`): a `WEBVTT` header, numbered cues, `HH:MM:SS.mmm -->
//! HH:MM:SS.mmm` timing. Unlike SRT, VTT has a `NOTE` comment block, so this
//! is the one caption format that can honor [`ExportOptions::metadata`].

use fc_core::{Conversation, Segment};

use crate::cues;
use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(conversation: &Conversation, segments: &[Segment], options: &ExportOptions) -> String {
    let mut out = String::from("WEBVTT\n\n");

    if options.metadata {
        out.push_str(&format!(
            "NOTE\n{}\nstarted: {}\nengine: {} ({})\n\n",
            conversation.title,
            timestamp::epoch_millis(conversation.started_at),
            conversation.engine.engine,
            conversation.engine.model,
        ));
    }

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
                timestamp::vtt(*start),
                timestamp::vtt(*end)
            ));
            n += 1;
        }
    }

    out
}
