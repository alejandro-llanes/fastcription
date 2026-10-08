//! Titles, speaker names and segment text are user data, and every format
//! here has syntax they can collide with. These are the collisions that
//! break a file rather than merely look odd.

mod common;

use fc_export::{export, ExportFormat, ExportOptions};

fn full() -> ExportOptions {
    ExportOptions {
        timestamps: true,
        speakers: true,
        metadata: true,
    }
}

/// WebVTT gives a `NOTE` block no escape syntax: a blank line ends the
/// comment, so everything after it is read as cue syntax and the file stops
/// being a transcript.
#[test]
fn a_vtt_note_survives_a_title_with_a_blank_line() {
    let mut conversation = common::conversation();
    conversation.title = "Design sync\n\n00:00:01.000 --> 00:00:02.000\nnot a cue".to_string();

    let out = export(
        &conversation,
        &common::segments(),
        ExportFormat::Vtt,
        &full(),
    );
    let note = out
        .split("\n\n")
        .find(|block| block.starts_with("NOTE"))
        .expect("the NOTE block must still be one block");
    assert!(note.contains("Design sync"));
    assert!(
        note.contains("not a cue"),
        "the whole title belongs inside the NOTE: {note}"
    );
    assert!(
        !note.contains("-->"),
        "a NOTE line that reads as cue timing: {note}"
    );
}

/// `-->` in a title makes its line read as cue timing. There is no way to
/// escape it, so it is replaced.
#[test]
fn a_vtt_note_survives_an_arrow_in_the_title_and_the_engine() {
    let mut conversation = common::conversation();
    conversation.title = "Q3 --> Q4 handover".to_string();
    conversation.engine.model = "base --> small".to_string();

    let out = export(
        &conversation,
        &common::segments(),
        ExportFormat::Vtt,
        &full(),
    );
    let (note, body) = out.split_once("\n\n1\n").expect("a NOTE then cue 1");
    assert!(!note.contains("-->"), "{note}");
    assert!(note.contains("Q3 \u{2192} Q4 handover"), "{note}");
    assert!(note.contains("base \u{2192} small"), "{note}");
    // The cues themselves are untouched.
    assert!(body.contains("00:00:00.000 --> 00:00:05.000"));
}

/// A title starting with `#` becomes a nested heading of its own, and a
/// diarised speaker named `**Bob**` closes the bold run the renderer opens
/// around it.
#[test]
fn markdown_escapes_the_title_and_the_speaker_labels() {
    let mut conversation = common::conversation();
    conversation.title = "# Weekly *notes*".to_string();
    let mut segments = common::segments();
    segments[0].speaker = Some("**Bob**".to_string());

    let out = export(&conversation, &segments, ExportFormat::Markdown, &full());
    assert!(
        out.starts_with(r"# \# Weekly \*notes\*"),
        "{}",
        out.lines().next().unwrap()
    );
    assert!(out.contains(r"**\*\*Bob\*\*:**"), "{out}");
    // Body text stays readable: escaping every `.` in a transcript would be
    // worse than the constructs it guards against.
    assert!(out.contains("Let's get started everyone."));
}

/// `Segment::text` is one line by invariant and the store enforces it, but a
/// blank line reaching a caption writer would terminate the cue and push the
/// rest of the segment into the file as cue syntax.
#[test]
fn a_multiline_segment_still_exports_as_one_cue_and_one_line() {
    let mut segments = common::segments();
    segments.truncate(1);
    segments[0].text = "first part\r\n\r\nsecond part".to_string();

    let srt = export(
        &common::conversation(),
        &segments,
        ExportFormat::Srt,
        &ExportOptions::default(),
    );
    assert_eq!(
        srt, "1\n00:00:00,000 --> 00:00:05,000\nfirst part second part\n\n",
        "one cue, one line of text"
    );

    let text = export(
        &common::conversation(),
        &segments,
        ExportFormat::Text,
        &ExportOptions::default(),
    );
    assert_eq!(text, "first part second part\n");

    let markdown = export(
        &common::conversation(),
        &segments,
        ExportFormat::Markdown,
        &ExportOptions::default(),
    );
    assert!(
        markdown.ends_with("first part second part\n\n"),
        "{markdown}"
    );
}

/// Whisper's non-speech markers are the model describing the audio, not
/// transcribing it, and no exporter should carry them.
#[test]
fn bracketed_audio_markers_never_reach_an_export() {
    let mut segments = common::segments();
    segments[0].text = "[BLANK_AUDIO]".to_string();
    segments[1].text = "(music playing)".to_string();

    for format in [
        ExportFormat::Text,
        ExportFormat::Markdown,
        ExportFormat::Json,
        ExportFormat::Srt,
        ExportFormat::Vtt,
    ] {
        let out = export(&common::conversation(), &segments, format, &full());
        assert!(
            !out.contains("BLANK_AUDIO") && !out.contains("music playing"),
            "{} kept an audio marker:\n{out}",
            format.as_str()
        );
    }
}
