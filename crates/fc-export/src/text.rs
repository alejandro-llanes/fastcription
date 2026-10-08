//! Plain text: just the words, one segment per line, in the order given.
//!
//! Speaker labels and timestamps are opt-in via [`ExportOptions`] rather than
//! always-on, so the default really is "just the words" -- a single-track
//! conversation with the defaults produces a plain line-per-utterance
//! transcript with nothing else in it.

use fc_core::{Conversation, Segment};

use crate::format::ExportOptions;
use crate::timestamp;

pub(crate) fn render(
    conversation: &Conversation,
    segments: &[Segment],
    options: &ExportOptions,
) -> String {
    let mut out = String::new();

    if options.metadata {
        out.push_str(&conversation.title);
        out.push('\n');
        out.push_str(&format!(
            "Started: {}\n",
            fc_core::time::rfc3339(conversation.started_at)
        ));
        if let Some(ended) = conversation.ended_at {
            out.push_str(&format!("Ended: {}\n", fc_core::time::rfc3339(ended)));
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
        // One segment per line is the format's whole promise, so the
        // one-line invariant is applied here too rather than assumed.
        out.push_str(&fc_core::single_line(&seg.text));
        out.push('\n');
    }

    out
}
