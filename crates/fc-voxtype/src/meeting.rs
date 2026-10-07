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
//! the real voxtype 1.0.1 binary on this machine (see fixtures).
//!
//! The exact layout of `meeting list`/`meeting show`/`meeting status` text
//! and the top-level shape of `meeting export --format json` below were
//! previously *invented* by a prior pass over this module (its own doc
//! comment said so) because this machine has no past meetings to capture
//! them from live. They have since been re-derived from upstream source at
//! the `v1.0.1` tag (`github.com/peteonrails/voxtype`), which is what this
//! machine's installed `voxtype 1.0.1` binary was built from:
//!   - `src/app/meeting.rs` -- the `println!`/`eprintln!` calls are
//!     themselves the output contract for `list`/`show`/`status`; there is
//!     no other source of truth for terminal text than the code that prints
//!     it.
//!   - `src/meeting/data.rs` -- `MeetingStatus` (`Active`/`Paused`/
//!     `Completed`/`Cancelled`, rendered with `{:?}` in list/show text) and
//!     `AudioSource` (`Microphone`/`Loopback`/`Unknown`), plus confirmation
//!     that `TranscriptSegment::start_ms`/`end_ms` are **milliseconds** from
//!     meeting start (not seconds -- the previous pass got this one right
//!     by luck, but it was never confirmed against source until now).
//!   - `src/meeting/export/json.rs` -- the exact `#[derive(Serialize)]`
//!     structs `meeting export --format json` emits. The top level is
//!     always `{"metadata": {...}, "transcript": {"segments": [...], ...}}`;
//!     the previous pass's "bare array or `{title, started_at, segments}`"
//!     guess does not match either shape upstream actually produces.
//!   - `src/daemon.rs` -- `write_meeting_state_file`, confirming the
//!     *runtime* status words (`idle`/`recording`/`paused`) the daemon
//!     writes to `$XDG_RUNTIME_DIR/voxtype/meeting_state` (and which
//!     `meeting status` then prints verbatim) are a smaller, different
//!     vocabulary from the `MeetingStatus` enum `list`/`show` render.
//!
//! Cross-checked against this machine's installed binary two ways: its
//! reported version (`voxtype --version` => `voxtype 1.0.1`, matching the
//! tag fetched from source) and its embedded strings (`strings -n2
//! /usr/lib/voxtype/voxtype-avx2`), which contain the exact literal label
//! text and column padding used below (`"ID:       "`, `"Started:  "`,
//! `"Chunks:   "`, `"  ID: "`, `"Meeting Status: "`, ...). Still worth a
//! real `voxtype meeting start && ... stop` round trip the first time this
//! machine has something to transcribe, since no fixture here was captured
//! from a live run.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::cli::run;
use crate::error::{Result, VoxtypeError};

/// `voxtype meeting export`'s format set -- a strict subset of
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


/// `src/meeting/data.rs`'s `MeetingStatus` enum, as rendered by `{:?}` in
/// `list`/`show` text (`Active`/`Paused`/`Completed`/`Cancelled`).
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
    /// Matches both vocabularies upstream uses for "is a meeting running":
    /// the `MeetingStatus` enum's `{:?}` text in `list`/`show`
    /// (`Active`/`Paused`/`Completed`/`Cancelled`) and the smaller set of
    /// raw words the daemon writes to the runtime `meeting_state` file,
    /// which `meeting status` prints verbatim (`recording`/`paused`; `idle`
    /// never reaches this parser -- see [`parse_status`]). `recording` and
    /// `active` both mean "in progress", so they alias to the same variant.
    fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "active" | "recording" => Self::Active,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "cancelled" | "canceled" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }
}

/// One meeting as reported by `meeting show`, one row of `meeting list`, or
/// the single current meeting from `meeting status`. The three commands
/// share this type but not a field layout -- `list` never carries a segment
/// or speaker count, and `status` carries neither a title nor any count at
/// all, since it reads the runtime state file rather than storage. A field
/// this command's output never has is simply `None`, not a sentinel value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingRecord {
    pub id: String,
    pub title: String,
    pub started_at: Option<String>,
    pub duration: Option<String>,
    pub status: Option<MeetingStatus>,
}

/// Extracts `key: value` pairs from `text`, tolerant of the leading
/// indentation `meeting list`'s rows use (`"  ID: ..."`) and of the
/// `Meeting ` prefix `meeting status` puts on its keys (`Meeting Status:`,
/// `Meeting ID:`, vs. plain `Status:`/`ID:` from `list`/`show`). A line with
/// no colon, or an empty value, never produces an entry -- that is how a
/// divider (`===============`, `-----------`), a blank line, or a
/// label-only line (`Transcript:`) all fail to produce a field without a
/// special case for any one of them.
fn key_values(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let key = key
            .trim()
            .trim_start_matches("Meeting")
            .trim()
            .to_lowercase();
        map.insert(key, value.to_string());
    }
    map
}


/// The one field every one of `list`/`show`/`status`'s outputs must carry.
/// Its absence is the only thing that turns a permissive parse into an
/// error.
fn require_id(
    fields: &HashMap<String, String>,
    command: &'static str,
    text: &str,
) -> Result<String> {
    fields
        .get("id")
        .cloned()
        .ok_or_else(|| VoxtypeError::Parse {
            command,
            reason: "no `ID:` (or `Meeting ID:`) line found".into(),
            text: text.to_string(),
        })
}

fn is_no_meetings(text: &str) -> bool {
    text.to_lowercase().contains("no meetings found")
}


/// `voxtype meeting list [--limit N]`. Verified empty-list wording
/// (`No meetings found.`, exit 0) against the real binary; the populated
/// case is derived from `src/app/meeting.rs`'s `MeetingAction::List` arm,
/// which prints, per meeting:
///
/// ```text
/// <display title>
///   ID: <uuid>
///   Date: <started_at, "%Y-%m-%d %H:%M", no seconds>
///   Duration: <"{m}m {s}s", no zero-padding, or "in progress">
///   Status: <MeetingStatus Debug: Active/Paused/Completed/Cancelled>
/// ```
/// separated by a blank line, after a `Recent Meetings\n===============`
/// banner. Unlike `show`, there is no `Title:` line at all -- the title is
/// the block's bare first line -- and no `Segments:`/`Speakers:` row.
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
        let block = block.trim_end();
        if block.is_empty() || !block.contains(':') {
            continue; // the "Recent Meetings\n===============" banner, or a trailing blank block
        }
        records.push(parse_list_block(block)?);
    }
    Ok(records)
}

fn parse_list_block(block: &str) -> Result<MeetingRecord> {
    let mut lines = block.lines();
    let title = lines.next().unwrap_or_default().trim().to_string();
    let rest = lines.collect::<Vec<_>>().join("\n");
    let fields = key_values(&rest);
    let id = require_id(&fields, "meeting list", block)?;
    Ok(MeetingRecord {
        id,
        title,
        started_at: fields.get("date").cloned(),
        duration: fields.get("duration").cloned(),
        status: fields.get("status").map(|s| MeetingStatus::parse(s)),
    })
}

/// One segment of an imported meeting's transcript, as `meeting export
/// --format json` actually emits it (`ExportedSegment` in
/// `src/meeting/export/json.rs`, verified at the `v1.0.1` tag):
/// `startMs`/`endMs` are **milliseconds** from meeting start (confirmed
/// against `TranscriptSegment::start_ms`'s doc comment in
/// `src/meeting/data.rs`, which is also `fc_core::Segment`'s own unit, so
/// import needs no conversion). `speaker` is the one merged field upstream
/// exports (`speaker_label.or(speaker_id)`, written only
/// `if Some`) -- there is no separate `speaker_id`/`speaker_label` pair on
/// the wire, and no `confidence` field at all; diarization confidence never
/// leaves the daemon. `speaker` is absent, not `null`, on a segment with no
/// diarization, which is the common case this machine will actually
/// import -- `#[serde(default)]` is what makes that import succeed instead
/// of failing the whole transcript.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ImportedSegment {
    #[serde(rename = "startMs")]
    pub start_ms: u64,
    #[serde(rename = "endMs")]
    pub end_ms: u64,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
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

/// `meeting export --format json`'s actual top level (`ExportedMeeting` in
/// `src/meeting/export/json.rs`): always `{"metadata": {...}, "transcript":
/// {"segments": [...], ...}}` -- never a bare segment array and never a
/// `{title, started_at, segments}` wrapper. Both of those were a prior
/// pass's guess, not something observed; this is what the serializer
/// actually writes. A `summary` key may also be present (Phase 5 AI
/// summaries); this import path has no use for it, so it is simply never
/// named here and serde drops it on the floor rather than erroring on an
/// unrecognised field.
#[derive(Debug, Clone, Deserialize)]
struct ExportJson {
    metadata: ExportJsonMetadata,
    transcript: ExportJsonTranscript,
}

#[derive(Debug, Clone, Deserialize)]
struct ExportJsonMetadata {
    #[serde(default)]
    title: Option<String>,
    #[serde(rename = "startedAt", default)]
    started_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExportJsonTranscript {
    #[serde(default)]
    segments: Vec<ImportedSegment>,
}

pub fn parse_export_json(text: &str) -> Result<ImportedTranscript> {
    let parsed: ExportJson = serde_json::from_str(text).map_err(|e| VoxtypeError::Parse {
        command: "meeting export --format json",
        reason: e.to_string(),
        text: text.to_string(),
    })?;
    Ok(ImportedTranscript {
        title: parsed.metadata.title,
        started_at: parsed.metadata.started_at,
        segments: parsed.transcript.segments,
    })
}

/// `voxtype meeting export <id> --format F [--timestamps] [--speakers]
/// [--metadata]`, returned as text for the caller to write out or (for
/// `ExportFormat::Json`) hand to [`parse_export_json`].
pub fn export(
    binary: &Path,
    id: &str,
    format: ExportFormat,
    options: ExportOptions,
) -> Result<String> {
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
