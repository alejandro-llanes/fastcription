//! The export format set and the flags that shape a render.
//!
//! Deliberately matches `voxtype meeting export`'s vocabulary (text,
//! markdown, json) plus the two caption formats voxtype does not offer
//! (srt, vtt), so a fastcription transcript and an imported voxtype
//! transcript stay interchangeable downstream (ARCHITECTURE.md §6).

/// Which renderer `export`/`write` dispatches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Text,
    Markdown,
    Json,
    Srt,
    Vtt,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }

    /// Accepts voxtype's own spellings plus the common file-extension
    /// shorthands (`txt`, `md`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "text" | "txt" => Some(Self::Text),
            "markdown" | "md" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            "srt" => Some(Self::Srt),
            "vtt" => Some(Self::Vtt),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }
}

/// Mirrors `voxtype meeting export`'s `--timestamps`/`--speakers`/`--metadata`
/// flags. Not every renderer honors every flag the same way: SRT has no
/// comment syntax, so `metadata` is a no-op there, and JSON always carries
/// every segment field regardless of `timestamps`/`speakers` since it is
/// meant to be read back, not skimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportOptions {
    pub timestamps: bool,
    pub speakers: bool,
    pub metadata: bool,
}
