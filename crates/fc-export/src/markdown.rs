//! Markdown: a heading, an optional metadata block, speaker labels grouping
//! consecutive lines from the same speaker, and readable paragraphs.

use fc_core::{Conversation, Segment};

use crate::format::ExportOptions;

pub(crate) fn render(
    conversation: &Conversation,
    segments: &[Segment],
    options: &ExportOptions,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", escape_md(&conversation.title)));

    if options.metadata {
        out.push_str(&format!(
            "- **Started:** {}\n",
            fc_core::time::rfc3339(conversation.started_at)
        ));
        if let Some(ended) = conversation.ended_at {
            out.push_str(&format!("- **Ended:** {}\n", fc_core::time::rfc3339(ended)));
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
                out.push_str(&format!("**{}:**\n\n", escape_md(speaker)));
                last_speaker = Some(speaker);
            }
        }
        if options.timestamps {
            out.push_str(&format!("`[{}]` ", crate::timestamp::vtt(seg.start_ms)));
        }
        // Body text is left as the engine produced it: escaping every `.` and
        // `-` in a transcript would make the file unreadable for the sake of
        // constructs a sentence does not form. Newlines are still collapsed,
        // because a blank line inside a paragraph would split it.
        out.push_str(&fc_core::single_line(&seg.text));
        out.push_str("\n\n");
    }

    out
}

/// Escapes the Markdown constructs a user-chosen title or a diarised speaker
/// name can form by accident.
///
/// A title of `# Weekly *notes*` would otherwise become a nested heading
/// holding an italic run, and a speaker whose label is `**Bob**` would close
/// the bold run this renderer opens around it and leave stray asterisks in
/// the text. Only these two positions get escaped: they are the ones the
/// renderer wraps in syntax of its own.
fn escape_md(value: &str) -> String {
    const PUNCTUATION: &[char] = &[
        '\\', '`', '*', '_', '{', '}', '[', ']', '(', ')', '#', '+', '-', '.', '!',
    ];
    let collapsed = fc_core::single_line(value);
    let mut out = String::with_capacity(collapsed.len());
    for c in collapsed.chars() {
        if PUNCTUATION.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_md;

    #[test]
    fn a_title_cannot_become_a_heading_or_an_italic_run() {
        assert_eq!(escape_md("# Weekly *notes*"), r"\# Weekly \*notes\*");
    }

    #[test]
    fn a_speaker_label_cannot_close_the_bold_run_around_it() {
        assert_eq!(escape_md("**Bob**"), r"\*\*Bob\*\*");
    }

    #[test]
    fn newline_runs_collapse_so_a_heading_stays_one_line() {
        assert_eq!(escape_md("Sprint\r\n\r\nreview"), "Sprint review");
    }
}
