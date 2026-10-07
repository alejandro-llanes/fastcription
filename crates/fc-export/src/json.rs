//! JSON: structured, serde, every segment field including `translation`.
//!
//! Unlike the human-readable formats, `timestamps`/`speakers` do not gate
//! anything here -- this format is meant to be read back (it is also what
//! `fc_voxtype::meeting::parse_export_json` deserialises on the way in), so
//! every field is always present. Only `metadata` is conditional, since
//! omitting an unwanted block is the one thing a consumer cannot easily undo
//! on their own.

use fc_core::{Conversation, Segment};
use serde::Serialize;

use crate::format::ExportOptions;

#[derive(Serialize)]
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

#[derive(Serialize)]
struct JsonMetadata {
    title: String,
    started_at: i64,
    ended_at: Option<i64>,
    status: String,
    source: String,
    engine: String,
    model: String,
    language: String,
}

#[derive(Serialize)]
struct JsonExport {
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<JsonMetadata>,
    segments: Vec<JsonSegment>,
}

pub(crate) fn render(conversation: &Conversation, segments: &[Segment], options: &ExportOptions) -> String {
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
        title: conversation.title.clone(),
        started_at: conversation.started_at,
        ended_at: conversation.ended_at,
        status: conversation.status.as_str().to_string(),
        source: conversation.source.label(),
        engine: conversation.engine.engine.clone(),
        model: conversation.engine.model.clone(),
        language: conversation.engine.language.clone(),
    });

    serde_json::to_string_pretty(&JsonExport { metadata, segments }).unwrap_or_default()
}
