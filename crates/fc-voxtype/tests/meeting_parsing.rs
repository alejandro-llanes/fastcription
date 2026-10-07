//! `meeting.rs` parsing.
//!
//! The empty-list wording (`No meetings found.`) and the "no meeting in
//! progress" wording are real captures from voxtype 1.0.1 on this machine,
//! which has no past meetings.
//!
//! Every other fixture here (`meeting_list_single/multiple.txt`,
//! `export_json_with_speakers.json`, `export_json_no_speakers.json`) is
//! *derived*, not captured live -- there is still nothing on this machine to
//! list, show, or export. They replace an earlier set of fixtures that were
//! hand-invented and never checked against anything. These are instead
//! built line-for-line from upstream voxtype source at the `v1.0.1` tag
//! (`github.com/peteonrails/voxtype`, matching this machine's installed
//! `voxtype --version` => `1.0.1`):
//!   - `meeting_list_*.txt` from `src/app/meeting.rs`'s
//!     `MeetingAction::List` arm (title line, then `  ID:`/`  Date:`/
//!     `  Duration:`/`  Status:`, blank-line separated).
//!   - `export_json_*.json` from `src/meeting/export/json.rs`'s
//!     `ExportedMeeting`/`ExportedMetadata`/`ExportedTranscript`/
//!     `ExportedSegment` structs: a `{"metadata": ..., "transcript": {...}}`
//!     object, camelCase keys, millisecond timestamps, and a single
//!     optional `speaker` field per segment (no separate `speaker_id` and
//!     no `confidence` -- neither exists on the wire).
//!
//! Column widths and literal label text (`"ID:       "`, `"Meeting Status: "`,
//! etc.) were spot-checked against `strings -n2 /usr/lib/voxtype/voxtype-avx2`.
//! See the module doc in `src/meeting.rs` for the full list of source files
//! read to build these.

use fc_voxtype::meeting::{parse_export_json, parse_list, MeetingStatus};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"))
}

#[test]
fn list_empty_real_capture() {
    let records = parse_list(&fixture("meeting_list_empty.txt")).expect("parses");
    assert!(records.is_empty());
}

#[test]
fn list_single_derived_from_source() {
    let records = parse_list(&fixture("meeting_list_single.txt")).expect("parses");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "3f29c5e1-8b77-4b1a-9c3b-1a2b3c4d5e6f");
    assert_eq!(records[0].title, "Standup");
    assert_eq!(records[0].started_at.as_deref(), Some("2026-10-07 14:30"));
    assert_eq!(records[0].duration.as_deref(), Some("12m 34s"));
    assert_eq!(records[0].status, Some(MeetingStatus::Completed));
    // `list` rows never carry a segment or speaker count -- only `show` does.
}

#[test]
fn list_multiple_derived_from_source() {
    let records = parse_list(&fixture("meeting_list_multiple.txt")).expect("parses");
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].title, "1:1 with Sam");
    assert_eq!(records[1].id, "3f29c5e1-8b77-4b1a-9c3b-1a2b3c4d5e6f");
    assert_eq!(records[2].title, "Design review");
    assert_eq!(records[2].status, Some(MeetingStatus::Cancelled));
    assert_eq!(records[2].duration.as_deref(), Some("5m 2s"));
}

#[test]
fn export_json_with_speakers() {
    let imported = parse_export_json(&fixture("export_json_with_speakers.json")).expect("parses");
    assert_eq!(imported.title.as_deref(), Some("Standup"));
    assert_eq!(
        imported.started_at.as_deref(),
        Some("2026-10-07T14:30:22+00:00")
    );
    assert_eq!(imported.segments.len(), 2);
    assert_eq!(imported.segments[0].text, "Let's get started.");
    assert_eq!(imported.segments[0].speaker.as_deref(), Some("Alice"));
    assert_eq!(imported.segments[0].start_ms, 0);
    assert_eq!(imported.segments[0].end_ms, 3200);
    assert_eq!(imported.segments[1].start_ms, 3200);
    assert_eq!(imported.segments[1].end_ms, 7400);
    assert_eq!(imported.segments[1].speaker.as_deref(), Some("Bob"));
}

#[test]
fn export_json_no_speakers_still_imports() {
    let imported = parse_export_json(&fixture("export_json_no_speakers.json")).expect("parses");
    assert_eq!(imported.title, None);
    assert_eq!(imported.segments.len(), 2);
    assert_eq!(imported.segments[0].speaker, None);
    assert_eq!(imported.segments[1].speaker, None);
    assert_eq!(imported.segments[1].text, "");
    assert_eq!(imported.segments[1].start_ms, 1800);
    assert_eq!(imported.segments[1].end_ms, 4200);
}

#[test]
fn export_json_rejects_garbage_with_offending_text() {
    let err = parse_export_json("not json at all").unwrap_err();
    let message = err.to_string();
    assert!(message.contains("not json at all"));
}

#[test]
fn export_json_rejects_the_old_bare_array_shape() {
    // The top level is always `{"metadata": ..., "transcript": ...}`; a bare
    // segment array (an earlier guess at the shape) must be rejected loudly,
    // not silently accepted as an empty or partial transcript.
    let err = parse_export_json(r#"[{"startMs": 0, "endMs": 100, "text": "hi"}]"#).unwrap_err();
    assert!(err.to_string().contains("startMs"));
}
