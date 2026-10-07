//! `meeting.rs` parsing.
//!
//! The empty-list wording (`No meetings found.`) and the "no meeting in
//! progress" wording are real captures from voxtype 1.0.1 on this machine,
//! which has no past meetings. Multi-row `list`, a found `show`, an active
//! `status`, and both `export --format json` shapes are hand-written per the
//! task brief's own example of when that is the right call -- see the
//! module doc in `src/meeting.rs` for exactly what they are reconstructed
//! from and why they need re-verifying against a real meeting.

use fc_voxtype::meeting::{parse_export_json, parse_list, parse_status, parse_block, MeetingStatus};

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
fn list_single_hand_written() {
    let records = parse_list(&fixture("meeting_list_single.txt")).expect("parses");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "20261007-143022");
    assert_eq!(records[0].title, "Standup");
    assert_eq!(records[0].segment_count, Some(42));
    assert_eq!(records[0].status, Some(MeetingStatus::Completed));
}

#[test]
fn list_multiple_hand_written() {
    let records = parse_list(&fixture("meeting_list_multiple.txt")).expect("parses");
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].id, "20261006-091500");
    assert_eq!(records[1].id, "20261007-143022");
    assert_eq!(records[2].id, "20261007-160000");
    assert_eq!(records[2].status, Some(MeetingStatus::Cancelled));
    assert_eq!(records[2].speakers.as_deref(), Some("1"));
}

#[test]
fn show_found_hand_written() {
    let record = parse_block(&fixture("meeting_show_found.txt")).expect("parses");
    assert_eq!(record.id, "20261007-143022");
    assert_eq!(record.title, "Standup");
    assert_eq!(record.duration.as_deref(), Some("12m34s"));
    assert_eq!(record.segment_count, Some(42));
}

#[test]
fn show_missing_id_is_a_parse_error_not_a_half_filled_record() {
    let err = parse_block("Title:    Orphaned\nDuration: 1m00s\n");
    assert!(err.is_err());
}

#[test]
fn status_none_real_capture() {
    let out = fixture("meeting_status_none.txt");
    let status = parse_status(&out).expect("parses");
    assert_eq!(status, None);
}

#[test]
fn status_active_hand_written_tolerates_meeting_prefixed_keys() {
    let out = fixture("meeting_status_active.txt");
    let status = parse_status(&out).expect("parses").expect("a meeting is active");
    assert_eq!(status.id, "20261007-170000");
    assert_eq!(status.title, "Design review");
    assert_eq!(status.status, Some(MeetingStatus::Active));
    assert_eq!(status.speakers.as_deref(), Some("2"));
}

#[test]
fn export_json_bare_array() {
    let imported = parse_export_json(&fixture("export_json_bare.json")).expect("parses");
    assert_eq!(imported.segments.len(), 2);
    assert_eq!(imported.segments[0].text, "Let's get started.");
    assert_eq!(imported.segments[0].speaker_label.as_deref(), Some("Alice"));
    assert_eq!(imported.segments[1].start_ms, 3200);
    assert_eq!(imported.segments[1].end_ms, 7400);
}

#[test]
fn export_json_wrapped_with_metadata() {
    let imported = parse_export_json(&fixture("export_json_wrapped.json")).expect("parses");
    assert_eq!(imported.title.as_deref(), Some("Standup"));
    assert_eq!(imported.segments.len(), 2);
    assert_eq!(imported.segments[1].text, "");
}

#[test]
fn export_json_rejects_garbage_with_offending_text() {
    let err = parse_export_json("not json at all").unwrap_err();
    let message = err.to_string();
    assert!(message.contains("not json at all"));
}
