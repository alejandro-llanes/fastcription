//! The `Transcriber` trait: the seam between [`crate::stream::TranscriptStream`]
//! and whatever turns PCM into text. `voxtype_cli::VoxtypeCli` is the only
//! implementation today; the trait exists so `VoxtypeMeeting`/`VoxtypeLive`
//! (ARCHITECTURE.md §8) can slot in later without `TranscriptStream` noticing,
//! and so tests can drive the agreement logic with a scripted fake instead of
//! the real subprocess.

use std::path::PathBuf;
use std::time::Duration;

use fc_core::{EngineInfo, Segment};
use thiserror::Error;

/// Everything that can go wrong turning a chunk of PCM into text.
///
/// Every variant's message is written to be actionable on its own in a log
/// line, since that's usually the only place it's seen -- the pipeline keeps
/// running after a chunk fails (ARCHITECTURE.md: "not fatal on its own").
#[derive(Debug, Error)]
pub enum AsrError {
    #[error(
        "voxtype binary not found (looked for '{binary}'): {source}. \
         Install voxtype or point VoxtypeCli at the right path."
    )]
    BinaryNotFound {
        binary: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write temporary WAV for transcription: {0}")]
    TempFile(std::io::Error),

    #[error("voxtype exited with status {status}, stderr: {stderr}")]
    NonZeroExit { status: i32, stderr: String },

    #[error(
        "voxtype did not finish within {timeout:?}; a chunk taking this long \
         means something is wrong (model stuck, machine overloaded, binary hung)"
    )]
    Timeout { timeout: Duration },

    #[error(
        "voxtype's stdout did not look like the expected banner + blank line + \
         transcript shape ({reason}); treating this as an error rather than an \
         empty transcript, since a parse failure must never silently lose \
         meeting audio. stdout was: {stdout:?}"
    )]
    UnexpectedOutput { reason: String, stdout: String },

    #[error(
        "voxtype does not know the model '{requested}' and silently fell back to \
         its own default, so the engine recorded with this conversation would be \
         a lie. Pick an installed model (`voxtype info models`)."
    )]
    UnknownModel { requested: String },

    #[error(
        "the voxtype config file '{}' does not exist. voxtype exits 0 and uses \
         its own defaults for a missing `-c` file, which silently drops the \
         context-window optimisation and remote mode (ARCHITECTURE.md D6, D10).",
        .0.display()
    )]
    ConfigMissing(PathBuf),

    #[error("i/o error running voxtype: {0}")]
    Io(#[from] std::io::Error),
}

/// Produces transcript segments from raw PCM.
///
/// **Identity is the caller's job, not this trait's.** An implementation only
/// ever sees bare samples -- it has no idea which track they came from, what
/// utterance this is, or where it sits on the session timeline. So the
/// segments it returns are stamped with placeholders: `track:
/// Track::Selected`, `seq: 0`, and `start_ms`/`end_ms` relative to the start
/// of *this* PCM. `TranscriptStream` only ever reads `.text` off what comes
/// back -- it computes its own utterance-relative timestamps from how much
/// audio it has pushed, since every pass re-transcribes the utterance from
/// its start rather than an independent chunk.
///
/// This split keeps implementations testable against bare PCM/WAV fixtures
/// without needing a [`crate::stream::TranscriptStream`] in the loop.
pub trait Transcriber: Send {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<Vec<Segment>, AsrError>;

    /// Engine/model/language actually in use, for the UI and for the record
    /// stored with a conversation (`fc_core::Conversation::engine`).
    fn describe(&self) -> EngineInfo;
}
