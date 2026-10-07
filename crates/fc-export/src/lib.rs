//! Transcript exporters: `Conversation` + `[Segment]` -> text, markdown,
//! json, srt, vtt.
//!
//! The format set deliberately matches `voxtype meeting export`'s (text,
//! markdown, json), plus the two caption formats voxtype does not offer, so
//! a fastcription transcript and an imported voxtype transcript stay
//! interchangeable downstream (ARCHITECTURE.md §6). Every renderer skips
//! blank segments (`Segment::is_blank`) -- whisper's silence/music artifacts
//! are not meant to reach any exported transcript -- and otherwise renders
//! segments in the order it is given them; merging and chronologically
//! ordering the selected-source and microphone tracks is the session
//! pipeline's job, not this crate's.

mod cues;
mod format;
mod json;
mod markdown;
mod srt;
mod text;
mod timestamp;
mod vtt;

pub use format::{ExportFormat, ExportOptions};

use std::io::{self, Write};

use fc_core::{Conversation, Segment};

/// Renders `segments` in `format`, honoring `options` as each renderer
/// documents (see each module: not every flag means the same thing in every
/// format).
pub fn export(
    conversation: &Conversation,
    segments: &[Segment],
    format: ExportFormat,
    options: &ExportOptions,
) -> String {
    match format {
        ExportFormat::Text => text::render(conversation, segments, options),
        ExportFormat::Markdown => markdown::render(conversation, segments, options),
        ExportFormat::Json => json::render(conversation, segments, options),
        ExportFormat::Srt => srt::render(segments, options),
        ExportFormat::Vtt => vtt::render(conversation, segments, options),
    }
}

/// As [`export`], written straight to `out` instead of returned as a
/// `String`, for a caller that is about to write it to a file anyway.
pub fn write(
    conversation: &Conversation,
    segments: &[Segment],
    format: ExportFormat,
    options: &ExportOptions,
    out: &mut dyn Write,
) -> io::Result<()> {
    out.write_all(export(conversation, segments, format, options).as_bytes())
}
