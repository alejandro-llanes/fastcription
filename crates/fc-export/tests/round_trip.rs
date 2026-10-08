//! The JSON export is the only lossless one, which is what makes it worth
//! being able to read back: an exported transcript is then a backup, not only
//! a document.

mod common;

use fc_export::{export, parse_json, ExportError, ExportFormat, ExportOptions};

fn full() -> ExportOptions {
    ExportOptions {
        timestamps: true,
        speakers: true,
        metadata: true,
    }
}

#[test]
fn json_renders_and_parses_back_to_the_same_conversation() {
    let conversation = common::conversation();
    let segments = common::segments();

    let rendered = export(&conversation, &segments, ExportFormat::Json, &full());
    let parsed = parse_json(&rendered).expect("the renderer's own output must parse");

    assert_eq!(parsed.title, conversation.title);
    assert_eq!(parsed.started_at, conversation.started_at);
    assert_eq!(parsed.ended_at, conversation.ended_at);
    assert_eq!(parsed.source, conversation.source);
    assert_eq!(parsed.mic_track, conversation.mic_track);
    assert_eq!(parsed.engine, conversation.engine);

    // Blank segments never reach any export, so they never come back either.
    let expected: Vec<_> = segments.into_iter().filter(|s| !s.is_blank()).collect();
    assert_eq!(parsed.segments, expected);
}

/// The whole point of the structured `source` object: a display label cannot
/// be turned back into a capture target, and an application stream is the
/// most valuable source the app offers (architecture §5).
#[test]
fn a_sink_input_source_survives_the_round_trip_as_its_parts() {
    let mut conversation = common::conversation();
    conversation.source = fc_core::AudioSource::sink_input(42, "zoom", "Zoom Meeting");

    let rendered = export(
        &conversation,
        &common::segments(),
        ExportFormat::Json,
        &full(),
    );
    let parsed = parse_json(&rendered).unwrap();

    assert_eq!(parsed.source.kind, fc_core::SourceKind::SinkInput);
    assert_eq!(parsed.source.application.as_deref(), Some("zoom"));
    assert_eq!(parsed.source.description, "Zoom Meeting");
    // The index named a stream that stopped existing with the application, so
    // carrying it would only make a stale number look authoritative.
    assert_eq!(parsed.source.index, None);
}

#[test]
fn both_spellings_of_each_instant_are_present() {
    let rendered = export(
        &common::conversation(),
        &common::segments(),
        ExportFormat::Json,
        &full(),
    );
    assert!(rendered.contains("\"started_at\": 1760000000000"));
    assert!(rendered.contains("\"started_at_iso\": \"2025-10-09T08:53:20Z\""));
    assert!(rendered.contains("\"ended_at_iso\": \"2025-10-09T10:23:24Z\""));
    assert!(rendered.contains("\"mic_track\": true"));
    assert!(rendered.contains("\"backend\": \"CPU (AVX2)\""));
    assert!(rendered.contains("\"voxtype_meeting_id\": null"));
}

/// A body rendered without metadata is segments and nothing else. Inventing
/// a title and a start time for it would put a claim in the library that
/// nothing backs.
#[test]
fn a_metadata_free_export_cannot_be_reimported() {
    let rendered = export(
        &common::conversation(),
        &common::segments(),
        ExportFormat::Json,
        &ExportOptions::default(),
    );
    assert!(matches!(
        parse_json(&rendered),
        Err(ExportError::NoMetadata)
    ));
}

#[test]
fn malformed_input_and_unknown_values_are_reported_not_guessed() {
    assert!(matches!(
        parse_json("not json at all"),
        Err(ExportError::Malformed(_))
    ));

    let rendered = export(
        &common::conversation(),
        &common::segments(),
        ExportFormat::Json,
        &full(),
    )
    .replace("\"track\": \"selected\"", "\"track\": \"telepathy\"");
    assert!(
        matches!(
            parse_json(&rendered),
            Err(ExportError::BadValue { field: "track", .. })
        ),
        "an unrecognised track must not silently become Selected"
    );

    let rendered = export(
        &common::conversation(),
        &common::segments(),
        ExportFormat::Json,
        &full(),
    )
    .replace("\"kind\": \"sink-monitor\"", "\"kind\": \"telepathy\"");
    assert!(matches!(
        parse_json(&rendered),
        Err(ExportError::BadValue {
            field: "source.kind",
            ..
        })
    ));
}
