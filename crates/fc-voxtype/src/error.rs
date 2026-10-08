//! Errors this crate surfaces.
//!
//! voxtype's CLI output is not a versioned interface (ARCHITECTURE.md §7), so
//! every parser here either returns a fully-populated value or a
//! [`VoxtypeError::Parse`] carrying the offending text. A caller must never
//! get a half-filled struct it cannot tell apart from a correctly parsed one.

#[derive(Debug, thiserror::Error)]
pub enum VoxtypeError {
    /// Neither `$PATH` nor `/usr/bin/voxtype` had a usable binary.
    #[error("voxtype binary not found on PATH or at /usr/bin/voxtype")]
    BinaryNotFound,

    /// The binary (or `systemctl`) ran but exited non-zero.
    #[error("`{command}` exited with {status}: {stderr}")]
    CommandFailed {
        command: String,
        status: std::process::ExitStatus,
        stderr: String,
    },

    /// A command that ran but never answered. Separate from
    /// [`Self::CommandFailed`]: nothing is wrong with the arguments, and the
    /// caller's remedy ("try again later") is different. This exists because
    /// these calls run on the interface thread, where an unbounded wait is a
    /// frozen window.
    #[error("`{command}` did not answer within {after:?}")]
    Timeout {
        command: String,
        after: std::time::Duration,
    },

    /// Output came back, but it did not match what this adapter expects.
    /// Carries the raw text so a bug report can include exactly what voxtype
    /// printed, rather than a caller guessing from a generic message.
    #[error("could not parse `{command}` output: {reason}\n--- offending text ---\n{text}")]
    Parse {
        command: &'static str,
        reason: String,
        text: String,
    },

    #[error("$XDG_RUNTIME_DIR is not set; cannot locate voxtype's runtime directory")]
    NoRuntimeDir,

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Toml(#[from] toml::de::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("watching the voxtype runtime directory failed: {0}")]
    Notify(#[from] notify::Error),
}

pub type Result<T> = std::result::Result<T, VoxtypeError>;
