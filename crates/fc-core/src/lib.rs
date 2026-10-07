//! Shared domain types for fastcription.
//!
//! Every other crate in the workspace speaks in these types, so this crate
//! stays free of I/O, of the UI toolkit, and of any dependency that could pull
//! one of those in. If a type is used by exactly one crate, it belongs in that
//! crate, not here.

use serde::{Deserialize, Serialize};

pub mod ids;
pub mod source;
pub mod transcript;

pub use ids::{ConversationId, GroupId, TagId};
pub use source::{AudioSource, SourceKind};
pub use transcript::{Segment, Track};

/// Milliseconds since the Unix epoch. The app stores every instant this way:
/// SQLite has no date type, and a single integer sorts, compares and exports
/// without a timezone attached.
pub type UnixMillis = i64;

/// Which transcription engine produced a transcript, recorded per conversation
/// so an old transcript can be read in the light of how it was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {
    /// voxtype engine name: `whisper`, `parakeet`, `moonshine`, ...
    pub engine: String,
    /// Model name as voxtype knows it, e.g. `base.en`.
    pub model: String,
    /// Language code passed to the engine, or `auto`.
    pub language: String,
    /// Acceleration backend as voxtype reports it, e.g. `CPU (AVX2)`.
    pub backend: Option<String>,
}

/// Lifecycle of a recorded conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationStatus {
    /// Recording right now.
    Active,
    /// Finished normally.
    Completed,
    /// The process died while recording; the transcript holds whatever was
    /// committed before that. Segments are persisted as they arrive, so this
    /// state still has usable content.
    Interrupted,
}

impl ConversationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }
}

/// A recorded conversation: the metadata row that owns a list of [`Segment`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    /// User-chosen name. Defaults to a timestamp when the user has not named it.
    pub title: String,
    pub group: Option<GroupId>,
    pub started_at: UnixMillis,
    pub ended_at: Option<UnixMillis>,
    pub status: ConversationStatus,
    /// The audio source as it was at recording time. Kept denormalised: the
    /// source may not exist any more when the transcript is read back.
    pub source: AudioSource,
    /// True when the user's own microphone was captured as a second track.
    pub mic_track: bool,
    pub engine: EngineInfo,
    /// Set only on conversations imported from `voxtype meeting`.
    pub voxtype_meeting_id: Option<String>,
}

/// A user-defined grouping of conversations. Flat for now; `Conversation::group`
/// is optional so nesting can arrive without touching stored segments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub created_at: UnixMillis,
}

/// A free-form label applied to any number of conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    /// `#rrggbb`, or `None` to let the theme pick.
    pub color: Option<String>,
}

/// How far behind live audio the transcriber is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pressure {
    /// Keeping up: chunks are transcribed faster than they arrive.
    Keeping,
    /// Falling behind; the segmenter has grown its chunk length to compensate.
    Lagging,
}

/// State of the live capture session, as the UI needs to render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Idle,
    Recording,
    Paused,
    /// Capture has stopped and the last chunks are still being transcribed.
    Finishing,
}

/// Everything the capture and transcription pipeline reports to the UI.
///
/// One channel carries all of it so the UI has a single place to drain and a
/// single ordering. Levels are frequent and lossy; segments are not.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Audio level for the meter. Dropping these is always safe.
    Level {
        peak: f32,
        rms: f32,
    },
    /// A first-pass result from a short chunk, to be replaced by the committed
    /// segment that covers the same audio. Never persisted.
    Provisional(Segment),
    /// A final result. Persisted before the UI sees it.
    Committed(Segment),
    StateChanged(SessionState),
    PressureChanged(Pressure),
    /// The capture device went away; the pipeline is attempting to reconnect.
    SourceLost {
        reason: String,
    },
    SourceRecovered,
    /// Something failed in a way the user needs to know about. Not fatal on its
    /// own: the session keeps whatever it already committed.
    Failed {
        stage: &'static str,
        message: String,
    },
}

/// Errors shared across crate boundaries.
///
/// `fc-voxtype` has its own `VoxtypeError` for everything voxtype-specific
/// (ARCHITECTURE.md §4: "the only crate that knows voxtype exists"), so this
/// type only needs to carry what a crate below it in the dependency graph can
/// actually produce.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("audio source {0} is no longer available")]
    SourceGone(String),
}
