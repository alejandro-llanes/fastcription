//! The only crate in the workspace that knows about voxtype.
//!
//! Everything here either shells out to the `voxtype` binary and parses what
//! comes back, or reads voxtype's own files read-only. Nothing in this crate
//! writes to anything voxtype owns: not its config, not its runtime trigger
//! files, not its database. See `docs/ARCHITECTURE.md` §7 for the rules this
//! crate exists to enforce.

mod bounded;
pub mod cli;
pub mod config;
mod error;
pub mod meeting;
pub mod runtime;
pub mod service;

pub use error::{Result, VoxtypeError};
