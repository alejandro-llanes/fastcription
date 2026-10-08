//! Transcript segments: the unit the pipeline produces and the store keeps.

use serde::{Deserialize, Serialize};

/// Which audio track a segment came from.
///
/// With only the selected source captured there is one track. With the optional
/// microphone track on, the distinction is what lets the UI label the two sides
/// of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Track {
    /// The source the user chose: the remote side of a call, usually.
    Selected,
    /// The user's own microphone, captured only when they opt in.
    Microphone,
}

impl Track {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Microphone => "microphone",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "selected" => Some(Self::Selected),
            "microphone" => Some(Self::Microphone),
            _ => None,
        }
    }

    /// Default speaker label when no diarisation has named anyone.
    pub fn default_speaker(self) -> &'static str {
        match self {
            Self::Selected => "Remote",
            Self::Microphone => "You",
        }
    }
}

/// One stretch of transcribed speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub track: Track,
    /// Monotonic per-track sequence number, assigned by the segmenter. It
    /// orders segments that share a millisecond and survives a reordered write.
    pub seq: u64,
    /// Offset from the start of the conversation.
    pub start_ms: u64,
    pub end_ms: u64,
    /// The transcribed words, as **one line**.
    ///
    /// This is an invariant the rest of the app relies on, not a suggestion:
    /// a blank line terminates an SRT/VTT cue, and the plain-text export
    /// promises one segment per line. Producers hand over whatever the engine
    /// emitted, which for a finalised multi-sentence utterance or an imported
    /// voxtype meeting can contain newlines, so [`single_line`] is applied on
    /// the way into the store — the one gate every persisted segment passes
    /// through.
    pub text: String,
    /// Reserved for the translation pass. The column exists from the first
    /// release so turning translation on later needs no migration.
    pub translation: Option<String>,
    /// Speaker name, once anything knows it.
    pub speaker: Option<String>,
    pub confidence: Option<f32>,
    /// A first-pass guess from a short chunk, shown dimmed and replaced by the
    /// committed segment covering the same audio. Provisional segments are
    /// never written to the store.
    pub provisional: bool,
}

impl Segment {
    /// True when there is nothing worth showing. Whisper returns empty strings
    /// and lone punctuation for silence and for music, and those should not
    /// become transcript lines.
    ///
    /// Also true for [`Segment::is_artifact`]: the bracketed markers are
    /// syntactically text but semantically the same "nothing was said", and
    /// folding them in here means every existing caller — the exporters, the
    /// live view — drops them without being changed.
    pub fn is_blank(&self) -> bool {
        !self.text.chars().any(|c| c.is_alphanumeric()) || self.is_artifact()
    }

    /// True for whisper's non-speech markers: `[MUSIC]`, `[BLANK_AUDIO]`,
    /// `(music playing)`.
    ///
    /// These are the model describing the audio rather than transcribing it,
    /// and in a live caption window they are noise. The test is deliberately
    /// narrow — *all* of the alphanumeric content inside one bracket or
    /// parenthesis pair — so a real sentence that merely contains a
    /// parenthetical ("we agreed (finally) to ship") is never discarded. No
    /// marker word list: voxtype ships nine engines in many languages, and a
    /// list would only ever cover English whisper.
    pub fn is_artifact(&self) -> bool {
        let text = self.text.trim();
        let Some((open, opener)) = text.char_indices().find(|(_, c)| matches!(c, '[' | '(')) else {
            return false;
        };
        let closer = if opener == '[' { ']' } else { ')' };
        let Some(close) = text[open..].find(closer).map(|i| open + i) else {
            return false;
        };
        let has_alnum = |s: &str| s.chars().any(char::is_alphanumeric);
        !has_alnum(&text[..open])
            && !has_alnum(&text[close..])
            && has_alnum(&text[open + opener.len_utf8()..close])
    }

    /// The speaker to display, falling back to the track's default.
    pub fn speaker_label(&self) -> &str {
        self.speaker
            .as_deref()
            .unwrap_or_else(|| self.track.default_speaker())
    }
}

/// Collapses every run of whitespace — newlines included — to a single space
/// and trims the ends, which is what [`Segment::text`]'s one-line invariant
/// means in practice.
///
/// Lives here rather than in the store because the invariant is declared here
/// and two crates have to honour it: the store on the way in, the caption
/// writers defensively on the way out.
pub fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str) -> Segment {
        Segment {
            track: Track::Selected,
            seq: 0,
            start_ms: 0,
            end_ms: 1_000,
            text: text.to_string(),
            translation: None,
            speaker: None,
            confidence: None,
            provisional: false,
        }
    }

    #[test]
    fn whisper_artifacts_are_not_transcript() {
        for marker in [
            "[MUSIC]",
            "[BLANK_AUDIO]",
            "(music playing)",
            " [ Silence ] ",
            "[_BEG_]",
        ] {
            let seg = segment(marker);
            assert!(seg.is_artifact(), "{marker} should read as an artifact");
            assert!(seg.is_blank(), "{marker} should be skipped as blank");
        }
    }

    #[test]
    fn a_parenthetical_inside_a_real_sentence_is_kept() {
        for sentence in [
            "We agreed (finally) to ship it.",
            "[MUSIC] and then she said hello",
            "The plan (see below).",
        ] {
            let seg = segment(sentence);
            assert!(
                !seg.is_artifact(),
                "{sentence} is speech, not an artifact marker"
            );
            assert!(
                !seg.is_blank(),
                "{sentence} must survive into the transcript"
            );
        }
    }

    #[test]
    fn empty_brackets_are_blank_by_punctuation_not_by_artifact() {
        let seg = segment("[]");
        assert!(!seg.is_artifact());
        assert!(seg.is_blank());
    }

    #[test]
    fn single_line_collapses_every_whitespace_run() {
        assert_eq!(single_line("one\r\n\r\ntwo"), "one two");
        assert_eq!(single_line("  padded\tout \n"), "padded out");
        assert_eq!(single_line("\n\n"), "");
        assert_eq!(single_line("already one line"), "already one line");
    }
}
