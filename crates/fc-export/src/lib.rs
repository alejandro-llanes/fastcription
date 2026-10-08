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
pub use json::{ExportError, ImportedExport};

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

/// Reads back a transcript rendered as [`ExportFormat::Json`] with
/// `metadata` on.
///
/// The JSON exporter is the only one of the five whose output is lossless, so
/// it is the only one that can be parsed. Having it means a transcript file
/// is a backup and not only a document: a library that was lost, or one on
/// another machine, can be rebuilt from what the user already exported.
pub fn parse_json(text: &str) -> Result<ImportedExport, ExportError> {
    json::parse(text)
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
