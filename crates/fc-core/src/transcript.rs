//! Transcript segments: the unit the pipeline produces and the store keeps.

use serde::{Deserialize, Serialize};

/// Which audio track a segment came from.
///
/// With only the selected source captured there is one track. With the optional
/// microphone track on, the distinction is what lets the UI label the two sides
/// of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Track {
    /// The source the user chose: the remote side of a call, usually.
    Selected,
    /// The user's own microphone, captured only when they opt in.
    Microphone,
}

impl Track {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Microphone => "microphone",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "selected" => Some(Self::Selected),
            "microphone" => Some(Self::Microphone),
            _ => None,
        }
    }

    /// Default speaker label when no diarisation has named anyone.
    pub fn default_speaker(self) -> &'static str {
        match self {
            Self::Selected => "Remote",
            Self::Microphone => "You",
        }
    }
}

/// One stretch of transcribed speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub track: Track,
    /// Monotonic per-track sequence number, assigned by the segmenter. It
    /// orders segments that share a millisecond and survives a reordered write.
    pub seq: u64,
    /// Offset from the start of the conversation.
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Reserved for the translation pass. The column exists from the first
    /// release so turning translation on later needs no migration.
    pub translation: Option<String>,
    /// Speaker name, once anything knows it.
    pub speaker: Option<String>,
    pub confidence: Option<f32>,
    /// A first-pass guess from a short chunk, shown dimmed and replaced by the
    /// committed segment covering the same audio. Provisional segments are
    /// never written to the store.
    pub provisional: bool,
}

impl Segment {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// True when there is nothing worth showing. Whisper returns empty strings
    /// and lone punctuation for silence and for music, and those should not
    /// become transcript lines.
    pub fn is_blank(&self) -> bool {
        !self.text.chars().any(|c| c.is_alphanumeric())
    }

    /// The speaker to display, falling back to the track's default.
    pub fn speaker_label(&self) -> &str {
        self.speaker
            .as_deref()
            .unwrap_or_else(|| self.track.default_speaker())
    }
}
