//! Shared golden-test fixture.
//!
//! One conversation, six segments, covering every case the task calls out:
//! two tracks, a segment with `translation: Some`, a segment with no
//! speaker, an empty-text segment (must be skipped by every renderer), a
//! 90-minute timestamp (exercises `HH` rollover), and a zero-duration
//! segment.

use fc_core::{
    AudioSource, Conversation, ConversationId, ConversationStatus, EngineInfo, Segment, SourceKind,
    Track,
};

pub fn conversation() -> Conversation {
    Conversation {
        id: ConversationId(1),
        title: "Design sync".to_string(),
        group: None,
        started_at: 1_760_000_000_000,
        ended_at: Some(1_760_005_404_000),
        status: ConversationStatus::Completed,
        source: AudioSource::named(SourceKind::SinkMonitor, "alsa_output.monitor", "System audio"),
        mic_track: true,
        engine: EngineInfo {
            engine: "whisper".to_string(),
            model: "base.en".to_string(),
            language: "en".to_string(),
            backend: Some("CPU (AVX2)".to_string()),
        },
        voxtype_meeting_id: None,
    }
}

pub fn segments() -> Vec<Segment> {
    vec![
        // 0: baseline, two tracks established here and on #1.
        Segment {
            track: Track::Selected,
            seq: 0,
            start_ms: 0,
            end_ms: 5_000,
            text: "Let's get started everyone.".to_string(),
            translation: None,
            speaker: Some("Alice".to_string()),
            confidence: Some(0.95),
            provisional: false,
        },
        // 1: translation: Some, second track.
        Segment {
            track: Track::Microphone,
            seq: 0,
            start_ms: 5_000,
            end_ms: 9_000,
            text: "Sounds good to me.".to_string(),
            translation: Some("Suena bien.".to_string()),
            speaker: Some("Bob".to_string()),
            confidence: Some(0.9),
            provisional: false,
        },
        // 2: no speaker set -> falls back to the track's default label.
        Segment {
            track: Track::Selected,
            seq: 1,
            start_ms: 9_000,
            end_ms: 12_000,
            text: "Great, no objections then.".to_string(),
            translation: None,
            speaker: None,
            confidence: Some(0.6),
            provisional: false,
        },
        // 3: empty text -- every renderer must skip this one.
        Segment {
            track: Track::Selected,
            seq: 2,
            start_ms: 12_000,
            end_ms: 12_500,
            text: String::new(),
            translation: None,
            speaker: Some("Alice".to_string()),
            confidence: Some(0.2),
            provisional: false,
        },
        // 4: zero-duration.
        Segment {
            track: Track::Selected,
            seq: 3,
            start_ms: 12_500,
            end_ms: 12_500,
            text: "Mm-hm.".to_string(),
            translation: None,
            speaker: Some("Alice".to_string()),
            confidence: Some(0.5),
            provisional: false,
        },
        // 5: 90-minute timestamp -- exercises HH rollover past 59 minutes.
        Segment {
            track: Track::Microphone,
            seq: 1,
            start_ms: 5_400_000,
            end_ms: 5_404_000,
            text: "An hour and a half in, still going.".to_string(),
            translation: None,
            speaker: Some("Bob".to_string()),
            confidence: Some(0.85),
            provisional: false,
        },
    ]
}
