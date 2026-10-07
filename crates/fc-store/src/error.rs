//! Errors the store surfaces to its callers.
//!
//! Callers need to tell "this row does not exist" apart from "the database
//! misbehaved" — the UI reacts to the first by closing a view, and to the
//! second by showing an error banner. [`StoreError::NotFound`] exists for
//! exactly that distinction; everything else bubbles up from rusqlite as-is.

use fc_core::Track;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    NotFound(String),

    /// A caller handed the store a segment still marked `provisional`.
    /// Provisional segments are a UI-only concept (architecture §3): they are
    /// never meant to reach the database, so seeing one here is a bug in the
    /// caller, not a recoverable condition.
    #[error("segment {track:?}#{seq} is provisional and must not be persisted")]
    ProvisionalSegment { track: Track, seq: u64 },

    #[error("database path has no usable data directory")]
    NoDataDir,

    /// The library was written by a newer build of fastcription. Opening it
    /// anyway would mean writing rows against a schema this build does not
    /// know, so it refuses.
    #[error("library schema is version {found}, but this build only knows {known}")]
    SchemaTooNew { found: usize, known: usize },

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;
