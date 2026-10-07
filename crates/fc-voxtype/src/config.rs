//! Read-only parse of `~/.config/voxtype/config.toml`.
//!
//! fastcription never writes this file: D6 in ARCHITECTURE.md reserves it for
//! the user's own dictation setup. The engine, model and language a
//! fastcription recording actually uses go through per-invocation CLI flags
//! (`voxtype --engine ... transcribe`), never through editing this file. All
//! this module does is read a handful of fields to pre-fill the UI with the
//! same defaults the user already chose for dictation.
//!
//! The file is large, user-edited, and covers dozens of settings this crate
//! has no use for, so every field below is optional and unknown keys or
//! absent sections (including a config with no `[meeting]` table at all,
//! which is the common case -- meeting mode is opt-in) are expected, not
//! errors. Verified against the annotated default at `/etc/voxtype/config.toml`
//! and this machine's own `~/.config/voxtype/config.toml`, both of which parse
//! with every field below coming back `None` except `whisper.model`,
//! `whisper.language` and `audio.device`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;

/// `whisper.language` is `"en"` in every config seen so far, but voxtype's
/// own schema documents a comma-separated list (`"en,fr,de"`) and its
/// resolved-config dump prints `language = Single("en")` for the scalar
/// form, confirming this is an untagged string-or-array field upstream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
enum LanguageField {
    Single(String),
    Multiple(Vec<String>),
}

impl LanguageField {
    fn into_display(self) -> String {
        match self {
            Self::Single(s) => s,
            Self::Multiple(v) => v.join(","),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawConfig {
    engine: Option<String>,
    whisper: Option<RawWhisper>,
    audio: Option<RawAudio>,
    meeting: Option<RawMeeting>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawWhisper {
    model: Option<String>,
    language: Option<LanguageField>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawAudio {
    device: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawMeeting {
    enabled: Option<bool>,
    chunk_duration_secs: Option<u32>,
    storage_path: Option<String>,
    audio: Option<RawMeetingAudio>,
    diarization: Option<RawMeetingDiarization>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawMeetingAudio {
    mic_device: Option<String>,
    loopback_device: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawMeetingDiarization {
    backend: Option<String>,
}

/// `[meeting]` and `[meeting.*]` defaults, present only when the user has
/// opted into meeting mode at least once (the annotated default config ships
/// with no `[meeting]` table at all).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeetingDefaults {
    pub enabled: Option<bool>,
    pub chunk_duration_secs: Option<u32>,
    pub storage_path: Option<String>,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub diarization_backend: Option<String>,
}

/// The subset of `config.toml` fastcription pre-fills its UI from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Defaults {
    pub engine: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    pub audio_device: Option<String>,
    pub meeting: Option<MeetingDefaults>,
}

/// `~/.config/voxtype/config.toml`, or `None` if `$HOME`/`$XDG_CONFIG_HOME`
/// cannot be resolved.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("voxtype").join("config.toml"))
}

/// Reads and parses `path`. A missing file is reported through the regular
/// I/O error, not folded into empty defaults, so a caller can tell "nothing
/// configured yet" apart from "the path is wrong".
pub fn read_defaults(path: impl AsRef<Path>) -> Result<Defaults> {
    let text = std::fs::read_to_string(path)?;
    parse_defaults(&text)
}

/// Parses already-read config text. Split out from [`read_defaults`] so
/// fixtures can be tested without touching the filesystem.
pub fn parse_defaults(text: &str) -> Result<Defaults> {
    let raw: RawConfig = toml::from_str(text)?;
    Ok(Defaults {
        engine: raw.engine,
        model: raw.whisper.as_ref().and_then(|w| w.model.clone()),
        language: raw
            .whisper
            .and_then(|w| w.language)
            .map(LanguageField::into_display),
        audio_device: raw.audio.and_then(|a| a.device),
        meeting: raw.meeting.map(|m| MeetingDefaults {
            enabled: m.enabled,
            chunk_duration_secs: m.chunk_duration_secs,
            storage_path: m.storage_path,
            mic_device: m.audio.as_ref().and_then(|a| a.mic_device.clone()),
            loopback_device: m.audio.and_then(|a| a.loopback_device),
            diarization_backend: m.diarization.and_then(|d| d.backend),
        }),
    })
}
