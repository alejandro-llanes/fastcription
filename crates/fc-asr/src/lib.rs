//! Chunking and transcription for fastcription's realtime path.
//!
//! voxtype has no live transcript feed (ARCHITECTURE.md §1), so this crate is
//! what turns a continuous stream of 16 kHz mono f32 PCM into committed
//! [`fc_core::Segment`]s: [`segmenter`] decides when a stretch of audio is
//! "enough" to send off, [`voxtype_cli`] runs `voxtype -q transcribe` on it
//! once per chunk, and [`dedup`] reconciles the half-second of audio that two
//! consecutive chunks share. [`transcriber`] is the seam between the two
//! halves, kept narrow enough that a future live-feed transcriber
//! (ARCHITECTURE.md §8) can replace `voxtype_cli` without the segmenter
//! noticing.
//!
//! Nothing here drops audio under load: see `Segmenter::grow_target` for how
//! backpressure is handled instead.

pub mod dedup;
pub mod segmenter;
pub mod transcriber;
pub mod voxtype_cli;

pub use dedup::dedup_overlap;
pub use segmenter::{Chunk, Segmenter, SegmenterConfig, SAMPLE_RATE_HZ};
pub use transcriber::{stamp_segment, AsrError, Transcriber};
pub use voxtype_cli::VoxtypeCli;
