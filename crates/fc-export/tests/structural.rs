//! Structural validity checks for the caption formats: cue numbering is
//! monotonic, every cue has `end > start` (WebVTT requires it and no player
//! renders a cue that lasts no time), SubRip never emits two cues at once,
//! and skipped/zero-duration/out-of-order segments break none of it.

mod common;

use fc_export::{export, ExportFormat, ExportOptions};

struct Cue {
    number: u32,
    start: String,
    end: String,
}

/// Parses the handful of fields these tests check out of an SRT/VTT body,
/// skipping the `WEBVTT`/`NOTE` preamble if present.
fn cues(body: &str) -> Vec<Cue> {
    let mut lines = body.lines().peekable();
    let mut cues = Vec::new();
    while let Some(line) = lines.next() {
        let Ok(number) = line.trim().parse::<u32>() else {
            continue;
        };
        let Some(timing) = lines.next() else { break };
        let Some((start, end)) = timing.split_once("-->") else {
            continue;
        };
        cues.push(Cue {
            number,
            start: start.trim().to_string(),
            end: end.trim().to_string(),
        });
    }
    cues
}

fn hms_to_ms(s: &str, decimal_sep: char) -> u64 {
    let (hms, frac) = s.split_once(decimal_sep).expect("HH:MM:SS<sep>mmm");
    let mut parts = hms.split(':');
    let h: u64 = parts.next().unwrap().parse().unwrap();
    let m: u64 = parts.next().unwrap().parse().unwrap();
    let s: u64 = parts.next().unwrap().parse().unwrap();
    let ms: u64 = frac.parse().unwrap();
    ((h * 60 + m) * 60 + s) * 1000 + ms
}

fn assert_structurally_valid(body: &str, decimal_sep: char) {
    let cues = cues(body);
    assert!(!cues.is_empty(), "expected at least one cue");
    for (expected_number, cue) in (1u32..).zip(cues.iter()) {
        assert_eq!(
            cue.number, expected_number,
            "cue numbering must be monotonic"
        );
        let start = hms_to_ms(&cue.start, decimal_sep);
        let end = hms_to_ms(&cue.end, decimal_sep);
        assert!(
            end > start,
            "cue {}: end {end} is not after start {start}",
            cue.number
        );
    }
}

#[test]
fn srt_is_structurally_valid() {
    let options = ExportOptions {
        speakers: true,
        ..Default::default()
    };
    let out = export(
        &common::conversation(),
        &common::segments(),
        ExportFormat::Srt,
        &options,
    );
    assert_structurally_valid(&out, ',');
}

#[test]
fn vtt_is_structurally_valid() {
    let options = ExportOptions {
        timestamps: true,
        speakers: true,
        metadata: true,
    };
    let out = export(
        &common::conversation(),
        &common::segments(),
        ExportFormat::Vtt,
        &options,
    );
    assert!(out.starts_with("WEBVTT\n"));
    assert_structurally_valid(&out, '.');
}

#[test]
fn srt_handles_an_out_of_order_segment() {
    use fc_core::{Segment, Track};
    let backwards = Segment {
        track: Track::Selected,
        seq: 0,
        start_ms: 5_000,
        end_ms: 1_000, // end before start
        text: "glitch".to_string(),
        translation: None,
        speaker: None,
        confidence: None,
        provisional: false,
    };
    let out = export(
        &common::conversation(),
        std::slice::from_ref(&backwards),
        ExportFormat::Srt,
        &ExportOptions::default(),
    );
    assert_structurally_valid(&out, ',');
}

#[test]
fn srt_long_segment_splits_into_multiple_monotonic_cues() {
    use fc_core::{Segment, Track};
    let long_text = "one two three four five six seven eight nine ten eleven twelve thirteen \
        fourteen fifteen sixteen seventeen eighteen nineteen twenty twenty-one twenty-two";
    let long = Segment {
        track: Track::Selected,
        seq: 0,
        start_ms: 0,
        end_ms: 30_000,
        text: long_text.to_string(),
        translation: None,
        speaker: None,
        confidence: None,
        provisional: false,
    };
    let out = export(
        &common::conversation(),
        std::slice::from_ref(&long),
        ExportFormat::Srt,
        &ExportOptions::default(),
    );
    let parsed = cues(&out);
    assert!(
        parsed.len() > 1,
        "a long segment should split into more than one cue"
    );
    assert_structurally_valid(&out, ',');
}

/// SubRip has no concept of concurrent cues. With the microphone track on
/// (D2) the two tracks interleave — one speaker is still mid-sentence when
/// the other starts — and a player shown two overlapping cues drops one,
/// losing a line of transcript rather than merely mistiming it.
#[test]
fn srt_never_emits_two_cues_at_once() {
    use fc_core::{Segment, Track};

    let interleaved = |track: Track, seq: u64, start_ms: u64, end_ms: u64, text: &str| Segment {
        track,
        seq,
        start_ms,
        end_ms,
        text: text.to_string(),
        translation: None,
        speaker: None,
        confidence: None,
        provisional: false,
    };
    // Ordered by (start_ms, seq), as the store returns them. Every segment
    // here runs past the next one's start.
    let segments = vec![
        interleaved(
            Track::Selected,
            0,
            0,
            6_000,
            "So the first thing we need to settle is the schedule.",
        ),
        interleaved(
            Track::Microphone,
            0,
            2_000,
            9_000,
            "Right, and I think we should start with the schedule too.",
        ),
        interleaved(
            Track::Selected,
            1,
            4_000,
            11_000,
            "Which means the review slips by a week.",
        ),
        interleaved(Track::Microphone, 1, 8_000, 12_000, "Agreed."),
        interleaved(Track::Selected, 2, 10_000, 10_000, "Mm."),
    ];

    let options = ExportOptions {
        speakers: true,
        ..Default::default()
    };
    let out = export(
        &common::conversation(),
        &segments,
        ExportFormat::Srt,
        &options,
    );
    let parsed = cues(&out);
    assert!(parsed.len() >= segments.len());

    let mut previous: Option<(u64, u64, u32)> = None;
    for cue in &parsed {
        let start = hms_to_ms(&cue.start, ',');
        let end = hms_to_ms(&cue.end, ',');
        assert!(
            end >= start,
            "cue {}: {start}..{end} is inverted",
            cue.number
        );
        if let Some((prev_start, prev_end, prev_number)) = previous {
            assert!(
                start >= prev_start,
                "cue {} starts before cue {prev_number}",
                cue.number
            );
            assert!(
                prev_end <= start,
                "cue {prev_number} ends at {prev_end}, after cue {} begins at {start}",
                cue.number
            );
        }
        previous = Some((start, end, cue.number));
    }

    // VTT carries the same conversation with its timing intact: two people
    // talking over each other is information the format can express.
    let vtt = export(
        &common::conversation(),
        &segments,
        ExportFormat::Vtt,
        &options,
    );
    let vtt_cues = cues(&vtt);
    assert!(
        vtt_cues
            .windows(2)
            .any(|pair| { hms_to_ms(&pair[0].end, '.') > hms_to_ms(&pair[1].start, '.') }),
        "VTT should keep the overlap SRT has to clamp"
    );
}
