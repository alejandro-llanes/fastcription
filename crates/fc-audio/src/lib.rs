//! PipeWire/PulseAudio source enumeration and capture.
//!
//! fastcription transcribes exactly one source at a time (`CLAUDE.md`), so this
//! crate's job is narrow: list what can be captured, and capture exactly one of
//! them as 16 kHz mono f32 PCM. Both enumeration and capture shell out to the
//! `pactl`/`parec` CLIs rather than binding `libpipewire` directly — the same
//! tradeoff voxtype makes for its own loopback track (docs/ARCHITECTURE.md §3)
//! — and capture sits behind [`capture::CaptureBackend`] so a native backend
//! can replace the subprocess later without touching callers.

pub mod capture;
pub mod levels;
pub mod sources;

pub use capture::{CaptureBackend, ParecCapture, PcmFrame};
pub use sources::{default_source, enumerate, resolve};

/// Errors from source enumeration and capture.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("failed to run `{0}`: {1}")]
    Spawn(&'static str, #[source] std::io::Error),
    #[error("`{0}` exited reporting an error: {1}")]
    CommandFailed(&'static str, String),
    #[error("failed to parse pactl output: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Core(#[from] fc_core::CoreError),
}

pub type Result<T> = std::result::Result<T, AudioError>;
