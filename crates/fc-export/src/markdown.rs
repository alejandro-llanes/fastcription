//! Markdown: a heading, an optional metadata block, speaker labels grouping
//! consecutive lines from the same speaker, and readable paragraphs.

use fc_core::{Conversation, Segment};

use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(conversation: &Conversation, segments: &[Segment], options: &ExportOptions) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", conversation.title));

    if options.metadata {
        out.push_str(&format!(
            "- **Started:** {}\n",
            timestamp::epoch_millis(conversation.started_at)
        ));
        if let Some(ended) = conversation.ended_at {
            out.push_str(&format!("- **Ended:** {}\n", timestamp::epoch_millis(ended)));
        }
        out.push_str(&format!("- **Source:** {}\n", conversation.source.label()));
        out.push_str(&format!(
            "- **Engine:** {} ({})\n",
            conversation.engine.engine, conversation.engine.model
        ));
        out.push('\n');
    }

    let mut last_speaker: Option<&str> = None;
    for seg in segments {
        if seg.is_blank() {
            continue;
        }
        if options.speakers {
            let speaker = seg.speaker_label();
            if last_speaker != Some(speaker) {
                out.push_str(&format!("**{speaker}:**\n\n"));
                last_speaker = Some(speaker);
            }
        }
        if options.timestamps {
            out.push_str(&format!("`[{}]` ", timestamp::vtt(seg.start_ms)));
        }
        out.push_str(seg.text.trim());
        out.push_str("\n\n");
    }

    out
}
