//! The `Transcriber` trait: the seam between the segmenter and whatever turns
//! PCM into text. `voxtype_cli::VoxtypeCli` is the only implementation today;
//! the trait exists so `VoxtypeMeeting`/`VoxtypeLive` (ARCHITECTURE.md §8) can
//! slot in later without touching the segmenter or the ASR worker.

use std::time::Duration;

use fc_core::{EngineInfo, Segment, Track};
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

    #[error("i/o error running voxtype: {0}")]
    Io(#[from] std::io::Error),
}

/// Produces transcript segments from raw PCM.
///
/// **Chunk identity is the caller's job, not this trait's.** An implementation
/// only ever sees bare samples -- it has no idea which track they came from,
/// what sequence number the chunk has, or where it sits on the session
/// timeline. So the segments it returns are stamped with placeholders:
/// `track: Track::Selected`, `seq: 0`, and `start_ms`/`end_ms` relative to the
/// start of *this* PCM (i.e. the first segment typically starts at `0`). The
/// caller -- the ASR worker, which does have the originating [`crate::segmenter::Chunk`]
/// -- turns those into real values with [`stamp_segment`] before a segment
/// goes anywhere else (the UI, the store, dedup).
///
/// This split keeps implementations testable against bare PCM/WAV fixtures
/// without needing to construct a `Chunk` just to call `transcribe`.
pub trait Transcriber: Send {
    fn transcribe(&self, pcm: &[f32], sample_rate: u32) -> Result<Vec<Segment>, AsrError>;

    /// Engine/model/language actually in use, for the UI and for the record
    /// stored with a conversation (`fc_core::Conversation::engine`).
    fn describe(&self) -> EngineInfo;
}

/// Replaces a [`Transcriber`]'s placeholder fields with the real chunk
/// identity. `chunk_start_ms` is added to the segment's (chunk-relative)
/// `start_ms`/`end_ms` to make them session-absolute.
pub fn stamp_segment(
    mut seg: Segment,
    track: Track,
    seq: u64,
    chunk_start_ms: u64,
    provisional: bool,
) -> Segment {
    seg.track = track;
    seg.seq = seq;
    seg.start_ms += chunk_start_ms;
    seg.end_ms += chunk_start_ms;
    seg.provisional = provisional;
    seg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment {
            track: Track::Selected,
            seq: 0,
            start_ms,
            end_ms,
            text: text.to_string(),
            translation: None,
            speaker: None,
            confidence: None,
            provisional: false,
        }
    }

    #[test]
    fn stamp_segment_makes_times_absolute_and_sets_identity() {
        let s = seg(0, 6_930, "hello");
        let stamped = stamp_segment(s, Track::Microphone, 42, 21_000, true);
        assert_eq!(stamped.track, Track::Microphone);
        assert_eq!(stamped.seq, 42);
        assert_eq!(stamped.start_ms, 21_000);
        assert_eq!(stamped.end_ms, 27_930);
        assert!(stamped.provisional);
        assert_eq!(stamped.text, "hello");
    }
}
