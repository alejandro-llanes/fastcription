//! Errors the store surfaces to its callers.
//!
//! Callers need to tell "this row does not exist" apart from "the database
//! misbehaved" — the UI reacts to the first by closing a view, and to the
//! second by showing an error banner. [`StoreError::NotFound`] exists for
//! exactly that distinction; everything else bubbles up from rusqlite as-is.

use fc_core::{ConversationId, Track};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    NotFound(String),

    /// The conversation being recorded right now. Deleting it would cascade
    /// away the live transcript and leave the session appending segments to a
    /// row that no longer exists — every later append and the closing
    /// `finish_conversation` would fail — so the store refuses instead of
    /// letting one UI click break the running session.
    #[error("conversation {id} is still recording; stop recording before deleting it")]
    ConversationActive { id: ConversationId },

    /// A caller handed the store a segment still marked `provisional`.
    /// Provisional segments are a UI-only concept (architecture §3): they are
    /// never meant to reach the database, so seeing one here is a bug in the
    /// caller, not a recoverable condition.
    #[error("segment {track:?}#{seq} is provisional and must not be persisted")]
    ProvisionalSegment { track: Track, seq: u64 },

    #[error("database path has no usable data directory")]
    NoDataDir,

    /// The library file or its directory is not writable, so it was opened
    /// read-only: past transcripts are readable, nothing can be added or
    /// changed. Worth distinguishing from a failure to open at all, which is
    /// what the user used to be told.
    #[error("the conversation library is read-only, so {operation} is not possible")]
    ReadOnly { operation: &'static str },

    /// The library was written by a newer build of fastcription. Opening it
    /// anyway would mean writing rows against a schema this build does not
    /// know, so it refuses.
    #[error("library schema is version {found}, but this build only knows {known}")]
    SchemaTooNew { found: usize, known: usize },

    /// An older schema on storage that cannot be written. The migration that
    /// would bring it forward is a write, so there is no safe way to read the
    /// rows this build expects.
    #[error(
        "library schema is version {found} and needs upgrading to {known}, but it is read-only"
    )]
    SchemaNeedsUpgrade { found: usize, known: usize },

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl StoreError {
    /// True for the errors that mean "this storage will not accept writes",
    /// which is what makes a read-only retry worth attempting rather than
    /// reporting the open as a failure (see `Store::open`).
    pub(crate) fn is_write_denied(&self) -> bool {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(err, _)) => matches!(
                err.code,
                rusqlite::ErrorCode::ReadOnly
                    | rusqlite::ErrorCode::CannotOpen
                    | rusqlite::ErrorCode::PermissionDenied
            ),
            Self::Io(err) => matches!(
                err.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
