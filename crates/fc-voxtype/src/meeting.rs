//! Thin, typed wrappers over `voxtype meeting`.
//!
//! Control goes through the CLI (`voxtype meeting start/stop/...`), never
//! through writing `$XDG_RUNTIME_DIR/voxtype/meeting_start` and its siblings
//! directly, even though we could: the CLI only writes those trigger files
//! as its own implementation of the contract it exposes to us, and that
//! implementation is upstream's to change. See ARCHITECTURE.md §7.
//!
//! `list`/`show`/`status` parse plain text meant for a terminal, not a
//! machine, so every field is read permissively -- unknown lines are
//! ignored and known fields can arrive in any order. What is *not*
//! permissive: a block missing the one field every meeting must have (its
//! id) is a [`VoxtypeError::Parse`] carrying the raw text, never a
//! `MeetingRecord` with `id: String::new()` silently standing in for "didn't
//! find one".
//!
//! Exit codes, `stop`/`pause`/`resume`/`label`/`delete` confirmation text,
//! and `meeting show`/`export` on a nonexistent id were all captured from
//! the real voxtype 1.0.1 binary on this machine (see fixtures). The exact
//! field layout of `meeting show`/`meeting list` rows (there are currently
//! no past meetings on this machine to list) and the top-level shape of
//! `meeting export --format json` are reconstructed from the binary's
//! embedded SQL (`meetings` table columns: id, title, started_at, ended_at,
//! duration_secs, status, chunk_count, storage_path, audio_retained, model,
//! synced_at) and from its string table, not captured from a live run --
//! re-verify both against a real meeting before relying on them.

use std::path::Path;

use serde::Deserialize;

use crate::cli::run;
use crate::error::{Result, VoxtypeError};

/// `voxtype meeting export`'s format set: a strict subset of
/// [`fc_export::ExportFormat`] -- voxtype's `--help` (verified on 1.0.1)
/// only ever offered `text`, `markdown`, `json`, with no `srt`/`vtt`, despite
/// ARCHITECTURE.md §1's table listing all five. Trust the binary here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Text,
    Markdown,
    Json,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

/// Mirrors `meeting export`'s `--timestamps`/`--speakers`/`--metadata` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportOptions {
    pub timestamps: bool,
    pub speakers: bool,
    pub metadata: bool,
}

/// `--diarization` override for `meeting start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diarization {
    /// Attribute by audio source (You vs Remote). Best for 1:1 calls.
    Simple,
    /// ONNX speaker embeddings for multi-speaker meetings. Requires a build
    /// with the `ml-diarization` feature.
    Ml,
}

impl Diarization {
    fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Ml => "ml",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingStatus {
    Active,
    Paused,
    Completed,
    Cancelled,
    /// A status word this module has not seen in the binary's string table.
    /// Kept distinct from a parse error: an unrecognised status still means
    /// every other field parsed fine.
    Unknown,
}

impl MeetingStatus {
    fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "active" => Self::Active,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "cancelled" | "canceled" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }
}

/// One meeting as reported by `meeting show`, one row of `meeting list`, or
/// the single current meeting from `meeting status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingRecord {
    pub id: String,
    pub title: String,
    pub started_at: Option<String>,
    pub duration: Option<String>,
    pub segment_count: Option<u64>,
    pub speakers: Option<String>,
    pub status: Option<MeetingStatus>,
}

/// Parses one `key: value`-per-line block, tolerant of a `Meeting ` prefix
/// on the key (`meeting status` prints `Meeting ID:`/`Meeting Title:`;
/// `meeting show`/`meeting list` print plain `ID:`/`Title:`), of any field
/// order, and of unrecognised extra lines.
pub fn parse_block(text: &str) -> Result<MeetingRecord> {
    let mut id = None;
    let mut title = None;
    let mut started_at = None;
    let mut duration = None;
    let mut segments = None;
    let mut speakers = None;
    let mut status = None;

    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let key = key.trim().trim_start_matches("Meeting").trim().to_lowercase();
        match key.as_str() {
            "id" => id = Some(value.to_string()),
            "title" => title = Some(value.to_string()),
            "started" | "started at" => started_at = Some(value.to_string()),
            "duration" => duration = Some(value.to_string()),
            "segments" => segments = Some(value.to_string()),
            "speakers" => speakers = Some(value.to_string()),
            "status" => status = Some(MeetingStatus::parse(value)),
            _ => {}
        }
    }

    let id = id.ok_or_else(|| VoxtypeError::Parse {
        command: "meeting show/list/status",
        reason: "no `ID:` (or `Meeting ID:`) line found".into(),
        text: text.to_string(),
    })?;
    let segment_count = segments
        .as_deref()
        .and_then(|s| s.split_whitespace().next())
        .and_then(|n| n.parse().ok());

    Ok(MeetingRecord {
        id,
        title: title.unwrap_or_default(),
        started_at,
        duration,
        segment_count,
        speakers,
        status,
    })
}

fn is_no_meetings(text: &str) -> bool {
    text.to_lowercase().contains("no meetings found")
}

fn is_no_current_meeting(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("no meeting currently in progress") || lower.contains("no meeting in progress")
}

/// `voxtype meeting list [--limit N]`. Verified empty-list wording
/// (`No meetings found.`, exit 0) against the real binary; the multi-row
/// case is a hand-written fixture -- see the module doc.
pub fn list(binary: &Path, limit: Option<u32>) -> Result<Vec<MeetingRecord>> {
    let limit_str = limit.unwrap_or(10).to_string();
    let out = run(binary, &["meeting", "list", "--limit", &limit_str])?;
    parse_list(&out)
}

pub fn parse_list(text: &str) -> Result<Vec<MeetingRecord>> {
    if is_no_meetings(text) {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for block in text.split("\n\n") {
        if !block.contains(':') {
            continue; // a title/banner block, e.g. a "Recent Meetings" header
        }
        records.push(parse_block(block)?);
    }
    Ok(records)
}

/// `voxtype meeting show <id>` (or `"latest"`). A nonexistent id is a
/// nonzero exit on the real binary (verified: `Error loading meeting:
/// Meeting not found: No meetings found`), which `run` already turns into a
/// [`VoxtypeError::CommandFailed`] -- no special-casing needed here.
pub fn show(binary: &Path, id: &str) -> Result<MeetingRecord> {
    let out = run(binary, &["meeting", "show", id])?;
    parse_block(&out)
}

/// `voxtype meeting status`: the single in-progress meeting, or `None`.
/// Verified wording for "nothing in progress" against the real binary.
pub fn status(binary: &Path) -> Result<Option<MeetingRecord>> {
    let out = run(binary, &["meeting", "status"])?;
    parse_status(&out)
}

/// Pure parsing half of [`status`], split out so it can be tested against a
/// captured fixture without invoking the binary.
pub fn parse_status(text: &str) -> Result<Option<MeetingRecord>> {
    if is_no_current_meeting(text) {
        return Ok(None);
    }
    parse_block(text).map(Some)
}

/// `voxtype meeting start [--title T] [--diarization simple|ml]`.
pub fn start(binary: &Path, title: Option<&str>, diarization: Option<Diarization>) -> Result<()> {
    let mut args: Vec<&str> = vec!["meeting", "start"];
    if let Some(t) = title {
        args.push("--title");
        args.push(t);
    }
    if let Some(d) = diarization {
        args.push("--diarization");
        args.push(d.as_str());
    }
    run(binary, &args)?;
    Ok(())
}

/// Verified against the real binary: with no meeting in progress this exits
/// non-zero with `Error: No meeting in progress.` on stderr, which `run`
/// surfaces as a normal [`VoxtypeError::CommandFailed`].
pub fn stop(binary: &Path) -> Result<()> {
    run(binary, &["meeting", "stop"])?;
    Ok(())
}

pub fn pause(binary: &Path) -> Result<()> {
    run(binary, &["meeting", "pause"])?;
    Ok(())
}

pub fn resume(binary: &Path) -> Result<()> {
    run(binary, &["meeting", "resume"])?;
    Ok(())
}

pub fn label(binary: &Path, meeting_id: &str, speaker_id: &str, label: &str) -> Result<()> {
    run(binary, &["meeting", "label", meeting_id, speaker_id, label])?;
    Ok(())
}

/// Verified: without `--force` the real binary exits non-zero with a
/// confirmation prompt on stderr instead of deleting anything, so `force`
/// is not optional here the way it is optional on the CLI.
pub fn delete(binary: &Path, meeting_id: &str, force: bool) -> Result<()> {
    let mut args: Vec<&str> = vec!["meeting", "delete", meeting_id];
    if force {
        args.push("--force");
    }
    run(binary, &args)?;
    Ok(())
}

/// One segment of an imported meeting's transcript. Field names accept both
/// the snake_case this binary's local storage schema uses
/// (`speaker_id`/`speaker_label`) and the camelCase seen in its remote-API
/// wire types (`speaker`), since which one `meeting export --format json`
/// actually emits has not been confirmed against a real meeting on this
/// machine (there are none to export).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ImportedSegment {
    #[serde(alias = "startMs")]
    pub start_ms: u64,
    #[serde(alias = "endMs")]
    pub end_ms: u64,
    pub text: String,
    #[serde(default, alias = "speakerId")]
    pub speaker_id: Option<String>,
    #[serde(default, alias = "speaker", alias = "speakerLabel")]
    pub speaker_label: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Not expected from voxtype (its `--translate` is whole-recording, not
    /// per-segment) but kept so an import can carry one through if a future
    /// voxtype version adds it, matching fc-core's own reserved slot (D4).
    #[serde(default)]
    pub translation: Option<String>,
}

/// An imported meeting transcript: enough to build fastcription's own
/// `Conversation`/`Segment` rows (ARCHITECTURE.md §6, D5 -- past voxtype
/// meetings are imported read-only into our own store, never read live from
/// voxtype's database).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedTranscript {
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub segments: Vec<ImportedSegment>,
}

/// `meeting export --format json`'s top level has not been captured from a
/// real meeting, so this accepts either a bare segment array or an object
/// wrapping `segments` alongside metadata, and fails loudly (never a silent
/// empty transcript) if neither shape matches.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ExportJson {
    Bare(Vec<ImportedSegment>),
    Wrapped {
        #[serde(default)]
        title: Option<String>,
        #[serde(default, alias = "startedAt")]
        started_at: Option<String>,
        #[serde(default)]
        segments: Vec<ImportedSegment>,
    },
}

pub fn parse_export_json(text: &str) -> Result<ImportedTranscript> {
    let parsed: ExportJson = serde_json::from_str(text).map_err(|e| VoxtypeError::Parse {
        command: "meeting export --format json",
        reason: e.to_string(),
        text: text.to_string(),
    })?;
    Ok(match parsed {
        ExportJson::Bare(segments) => ImportedTranscript { title: None, started_at: None, segments },
        ExportJson::Wrapped { title, started_at, segments } => {
            ImportedTranscript { title, started_at, segments }
        }
    })
}

/// `voxtype meeting export <id> --format F [--timestamps] [--speakers]
/// [--metadata]`, returned as text for the caller to write out or (for
/// `ExportFormat::Json`) hand to [`parse_export_json`].
pub fn export(binary: &Path, id: &str, format: ExportFormat, options: ExportOptions) -> Result<String> {
    let mut args: Vec<&str> = vec!["meeting", "export", id, "--format", format.as_str()];
    if options.timestamps {
        args.push("--timestamps");
    }
    if options.speakers {
        args.push("--speakers");
    }
    if options.metadata {
        args.push("--metadata");
    }
    run(binary, &args)
}
