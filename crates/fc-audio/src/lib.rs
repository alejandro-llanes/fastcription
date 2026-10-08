//! PipeWire/PulseAudio source enumeration and capture.
//!
//! fastcription transcribes exactly one source at a time (`CLAUDE.md`), so this
//! crate's job is narrow: list what can be captured, and capture exactly one of
//! them as 16 kHz mono f32 PCM. Both enumeration and capture shell out to the
//! `pactl`/`parec` CLIs rather than binding `libpipewire` directly — the same
//! tradeoff voxtype makes for its own loopback track (docs/ARCHITECTURE.md §3)
//! — and capture sits behind [`capture::CaptureBackend`] so a native backend
//! can replace the subprocess later without touching callers.

mod bounded;
pub mod capture;
pub mod levels;
pub mod sources;
pub mod spectrum;

pub use capture::{CaptureBackend, ParecCapture, PcmFrame};
pub use sources::{default_source, enumerate, resolve};
pub use spectrum::{Analyzer, BANDS};

/// What every source is resampled to on capture, and the rate voxtype wants.
///
/// At the crate root rather than inside [`capture`] because [`spectrum`] has
/// to agree with it: a band boundary in Hz is only a bin index if both halves
/// believe the same thing about the sample rate.
pub const SAMPLE_RATE: usize = 16_000;

/// Errors from source enumeration and capture.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("failed to run `{0}`: {1}")]
    Spawn(&'static str, #[source] std::io::Error),
    #[error("`{0}` exited reporting an error: {1}")]
    CommandFailed(&'static str, String),
    /// A command that never answered. Separate from [`Self::CommandFailed`]
    /// because the remedy is different: nothing is wrong with the arguments,
    /// the sound server is not responding.
    #[error("`{command}` did not answer within {after:?}; the sound server may be restarting")]
    Timeout {
        command: String,
        after: std::time::Duration,
    },
    /// More than one live stream fits the stored descriptor, and recording the
    /// wrong half of a meeting is worse than asking. Raised by
    /// [`sources::resolve`] and, on a reconnect, reported as
    /// [`fc_core::SessionEvent::SourceLost`] while the capture keeps retrying.
    #[error("{looked_for} matches {} streams right now ({}); fastcription will not guess which one to record", candidates.len(), candidates.join(", "))]
    Ambiguous {
        looked_for: String,
        candidates: Vec<String>,
    },
    #[error("failed to parse pactl output: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Core(#[from] fc_core::CoreError),
}

pub type Result<T> = std::result::Result<T, AudioError>;
