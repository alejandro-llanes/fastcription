//! JSON: structured, serde, every segment field including `translation`, and
//! enough metadata to rebuild the conversation.
//!
//! Unlike the human-readable formats, `timestamps`/`speakers` do not gate
//! anything here -- this format is meant to be read back -- so every field is
//! always present. Only `metadata` is conditional, since omitting an unwanted
//! block is the one thing a consumer cannot easily undo on their own.
//!
//! This is fastcription's own shape, not voxtype's: `voxtype meeting export
//! --format json` produces something else, which `fc_voxtype::meeting` parses
//! on the way in. What makes the two interchangeable is the *set* of formats
//! (ARCHITECTURE.md §6), not the JSON schema. Since nothing downstream is
//! constrained by voxtype's layout, this one carries what it takes to round
//! trip: the source as a structured object rather than a display label, the
//! backend, the microphone track, the imported meeting id, and both epoch and
//! RFC 3339 spellings of each instant — machines want the first, anyone
//! reading the file wants the second.

use fc_core::{AudioSource, Conversation, EngineInfo, Segment, SourceKind, Track, UnixMillis};
use serde::{Deserialize, Serialize};

use crate::format::ExportOptions;

#[derive(Debug, Serialize, Deserialize)]
struct JsonSegment {
    track: String,
    seq: u64,
    start_ms: u64,
    end_ms: u64,
    text: String,
    translation: Option<String>,
    speaker: Option<String>,
    confidence: Option<f32>,
}

/// The source as its parts, not as `AudioSource::label()`.
///
/// A label is for a person to read and cannot be turned back into a capture
/// target; the parts can, which is what makes an exported transcript
/// re-importable and what tells a reader months later whether they recorded
/// an application or the whole system.
#[derive(Debug, Serialize, Deserialize)]
struct JsonSource {
    kind: String,
    name: String,
    description: String,
    application: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonMetadata {
    id: i64,
    title: String,
    started_at: UnixMillis,
    started_at_iso: String,
    ended_at: Option<UnixMillis>,
    ended_at_iso: Option<String>,
    status: String,
    source: JsonSource,
    mic_track: bool,
    engine: String,
    model: String,
    language: String,
    backend: Option<String>,
    voxtype_meeting_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonExport {
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<JsonMetadata>,
    segments: Vec<JsonSegment>,
}

/// What [`parse`] recovers from a rendered export.
///
/// Not `fc_store::NewConversation`: `fc-export` sits beside the store in the
/// dependency graph, not above it, and a round trip must not drag SQLite into
/// every consumer of a transcript file. The caller assembles whatever its own
/// store wants from these fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedExport {
    pub title: String,
    pub started_at: UnixMillis,
    pub ended_at: Option<UnixMillis>,
    pub source: AudioSource,
    pub mic_track: bool,
    pub engine: EngineInfo,
    pub segments: Vec<Segment>,
}

/// Why a rendered export could not be read back.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("the export is not valid fastcription JSON: {0}")]
    Malformed(String),

    /// A body rendered with `metadata: false` is segments and nothing else.
    /// There is no conversation in it to rebuild, and inventing a title and a
    /// start time would put a claim in the library nothing backs.
    #[error("the export has no metadata block, so there is no conversation in it")]
    NoMetadata,

    #[error("unrecognised {field} value {value:?}")]
    BadValue { field: &'static str, value: String },
}

pub(crate) fn render(
    conversation: &Conversation,
    segments: &[Segment],
    options: &ExportOptions,
) -> String {
    let segments = segments
        .iter()
        .filter(|s| !s.is_blank())
        .map(|s| JsonSegment {
            track: s.track.as_str().to_string(),
            seq: s.seq,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            text: s.text.clone(),
            translation: s.translation.clone(),
            speaker: s.speaker.clone(),
            confidence: s.confidence,
        })
        .collect();

    let metadata = options.metadata.then(|| JsonMetadata {
        id: conversation.id.get(),
        title: conversation.title.clone(),
        started_at: conversation.started_at,
        started_at_iso: fc_core::time::rfc3339(conversation.started_at),
        ended_at: conversation.ended_at,
        ended_at_iso: conversation.ended_at.map(fc_core::time::rfc3339),
        status: conversation.status.as_str().to_string(),
        source: JsonSource {
            kind: conversation.source.kind.as_str().to_string(),
            name: conversation.source.name.clone(),
            description: conversation.source.description.clone(),
            application: conversation.source.application.clone(),
        },
        mic_track: conversation.mic_track,
        engine: conversation.engine.engine.clone(),
        model: conversation.engine.model.clone(),
        language: conversation.engine.language.clone(),
        backend: conversation.engine.backend.clone(),
        voxtype_meeting_id: conversation.voxtype_meeting_id.clone(),
    });

    serde_json::to_string_pretty(&JsonExport { metadata, segments }).unwrap_or_default()
}

/// Reads back what [`render`] wrote, so a transcript exported as JSON can be
/// re-imported — into another library, or back into this one after the
/// database was lost.
pub(crate) fn parse(text: &str) -> Result<ImportedExport, ExportError> {
    let parsed: JsonExport =
        serde_json::from_str(text).map_err(|e| ExportError::Malformed(e.to_string()))?;
    let metadata = parsed.metadata.ok_or(ExportError::NoMetadata)?;

    let kind = SourceKind::parse(&metadata.source.kind).ok_or_else(|| ExportError::BadValue {
        field: "source.kind",
        value: metadata.source.kind.clone(),
    })?;

    let segments = parsed
        .segments
        .into_iter()
        .map(|s| {
            let track = Track::parse(&s.track).ok_or_else(|| ExportError::BadValue {
                field: "track",
                value: s.track.clone(),
            })?;
            Ok(Segment {
                track,
                seq: s.seq,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                text: s.text,
                translation: s.translation,
                speaker: s.speaker,
                confidence: s.confidence,
                // Provisional segments are never exported and never stored
                // (architecture §3), so anything read back is committed.
                provisional: false,
            })
        })
        .collect::<Result<Vec<Segment>, ExportError>>()?;

    Ok(ImportedExport {
        title: metadata.title,
        started_at: metadata.started_at,
        ended_at: metadata.ended_at,
        source: AudioSource {
            kind,
            name: metadata.source.name,
            description: metadata.source.description,
            application: metadata.source.application,
            // A sink-input index names a stream that stopped existing when the
            // application did. Carrying it through an export would only make
            // a stale number look authoritative (architecture §5).
            index: None,
        },
        mic_track: metadata.mic_track,
        engine: EngineInfo {
            engine: metadata.engine,
            model: metadata.model,
            language: metadata.language,
            backend: metadata.backend,
        },
        segments,
    })
}
