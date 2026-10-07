//! Plain text: just the words, one segment per line, in the order given.
//!
//! Speaker labels and timestamps are opt-in via [`ExportOptions`] rather than
//! always-on, so the default really is "just the words" -- a single-track
//! conversation with the defaults produces a plain line-per-utterance
//! transcript with nothing else in it.

use fc_core::{Conversation, Segment};

use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(conversation: &Conversation, segments: &[Segment], options: &ExportOptions) -> String {
    let mut out = String::new();

    if options.metadata {
        out.push_str(&conversation.title);
        out.push('\n');
        out.push_str(&format!("Started: {}\n", timestamp::epoch_millis(conversation.started_at)));
        if let Some(ended) = conversation.ended_at {
            out.push_str(&format!("Ended: {}\n", timestamp::epoch_millis(ended)));
        }
        out.push_str(&format!("Source: {}\n", conversation.source.label()));
        out.push_str(&format!(
            "Engine: {} ({})\n",
            conversation.engine.engine, conversation.engine.model
        ));
        out.push('\n');
    }

    for seg in segments {
        if seg.is_blank() {
            continue;
        }
        if options.timestamps {
            out.push_str(&format!("[{}] ", timestamp::vtt(seg.start_ms)));
        }
        if options.speakers {
            out.push_str(seg.speaker_label());
            out.push_str(": ");
        }
        out.push_str(seg.text.trim());
        out.push('\n');
    }

    out
}
