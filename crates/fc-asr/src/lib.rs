//! Realtime transcription for fastcription's live view.
//!
//! voxtype has no live transcript feed (ARCHITECTURE.md §1), so this crate
//! turns a continuous stream of 16 kHz mono f32 PCM into text while a
//! conversation is still happening: [`stream`] re-transcribes the current
//! silence-anchored utterance on every `step` and commits whatever two
//! consecutive passes agree on (LocalAgreement-2); [`voxtype_cli`] is what
//! actually runs `voxtype -q transcribe` for each pass. [`transcriber`] is
//! the seam between the two, kept narrow enough that a future live-feed
//! transcriber (ARCHITECTURE.md §8) can replace `voxtype_cli` without
//! `stream` noticing.
//!
//! Nothing here drops audio under load: a transcriber slower than `step`
//! lengthens the effective step instead (see [`stream::Update::lagging`]).

pub mod stream;
pub mod transcriber;
pub mod voxtype_cli;

pub use stream::{StreamConfig, TranscriptStream, Update, Utterance, SAMPLE_RATE_HZ};
pub use transcriber::{AsrError, Transcriber};
pub use voxtype_cli::VoxtypeCli;
