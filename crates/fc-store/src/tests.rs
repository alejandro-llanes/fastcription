use std::path::{Path, PathBuf};

use fc_core::{AudioSource, ConversationStatus, EngineInfo, Segment, SourceKind, Track};
use tempfile::TempDir;

use crate::{ConversationFilter, NewConversation, Store, StoreError};

fn open_temp() -> (TempDir, Store) {
    let dir = TempDir::new().expect("tempdir");
    let store = Store::open(dir.path().join("library.db")).expect("open store");
    (dir, store)
}

fn sample_source() -> AudioSource {
    AudioSource::named(
        SourceKind::SinkMonitor,
        "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
        "Built-in Audio Analog Stereo",
    )
}

fn sample_engine() -> EngineInfo {
    EngineInfo {
        engine: "whisper".into(),
        model: "base.en".into(),
        language: "en".into(),
        backend: Some("CPU (AVX2)".into()),
    }
}

fn sample_conversation(title: &str, started_at: i64) -> NewConversation {
    NewConversation {
        title: title.into(),
        group: None,
        started_at,
        source: sample_source(),
        mic_track: false,
        engine: sample_engine(),
        voxtype_meeting_id: None,
    }
}

fn sample_segment(seq: u64, start_ms: u64, end_ms: u64, text: &str) -> Segment {
    Segment {
        track: Track::Selected,
        seq,
        start_ms,
        end_ms,
        text: text.into(),
        translation: None,
        speaker: None,
        confidence: Some(0.87),
        provisional: false,
    }
}

#[test]
fn migrates_from_empty() {
    let (_dir, store) = open_temp();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    // One per migration: V1 the library, V2 the word registry.
    assert_eq!(version, 2);

    let table_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                'conversations', 'segments', 'groups', 'tags', 'conversation_tags', 'segments_fts'
            )",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 6);
}

#[test]
fn reopen_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("library.db");
    {
        let store = Store::open(&path).unwrap();
        let id = store
            .create_conversation(&sample_conversation("first open", 1000))
            .unwrap();
        store
            .finish_conversation(id, 2000, ConversationStatus::Completed)
            .unwrap();
    }
    // Reopening must not error, must not re-run migrations destructively, and
    // must still see the row written before the close.
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    // One per migration: V1 the library, V2 the word registry.
    assert_eq!(version, 2);
    let convs = store
        .list_conversations(&ConversationFilter::default())
        .unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].title, "first open");
}

#[test]
fn append_and_load_round_trip_preserves_every_field() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("round trip", 0))
        .unwrap();

    let with_translation = Segment {
        translation: Some("hola".into()),
        speaker: Some("Alex".into()),
        ..sample_segment(1, 1000, 2000, "hello")
    };
    let without_translation = sample_segment(2, 2000, 3000, "world");

    store
        .append_segments(id, &[with_translation.clone(), without_translation.clone()])
        .unwrap();

    let loaded = store.load_segments(id).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0], with_translation);
    assert_eq!(loaded[1], without_translation);
    assert_eq!(loaded[1].translation, None);
}

#[test]
fn provisional_segments_are_rejected() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("provisional", 0))
        .unwrap();

    let mut provisional = sample_segment(1, 0, 500, "partial");
    provisional.provisional = true;

    let err = store.append_segments(id, &[provisional]).unwrap_err();
    assert!(matches!(err, StoreError::ProvisionalSegment { .. }));

    // The whole batch is rejected, including the committed segment alongside it.
    assert_eq!(store.load_segments(id).unwrap().len(), 0);
}

#[test]
fn append_requires_an_existing_conversation() {
    let (_dir, store) = open_temp();
    let bogus = fc_core::ConversationId(999);
    let err = store
        .append_segments(bogus, &[sample_segment(1, 0, 100, "x")])
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[test]
fn reap_active_marks_interrupted_and_keeps_segments() {
    let (_dir, store) = open_temp();
    let active = store
        .create_conversation(&sample_conversation("left running", 0))
        .unwrap();
    let finished = store
        .create_conversation(&sample_conversation("finished normally", 0))
        .unwrap();
    store
        .finish_conversation(finished, 100, ConversationStatus::Completed)
        .unwrap();
    store
        .append_segments(active, &[sample_segment(1, 0, 100, "still here")])
        .unwrap();

    let reaped = store.reap_active().unwrap();
    assert_eq!(reaped, 1);

    let conv = store.get_conversation(active).unwrap();
    assert_eq!(conv.status, ConversationStatus::Interrupted);
    assert_eq!(store.load_segments(active).unwrap().len(), 1);

    // Finished conversation is untouched, and reaping twice finds nothing left.
    let conv = store.get_conversation(finished).unwrap();
    assert_eq!(conv.status, ConversationStatus::Completed);
    assert_eq!(store.reap_active().unwrap(), 0);
}

#[test]
fn deleting_a_conversation_cascades_segments_and_tags() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("to delete", 0))
        .unwrap();
    store
        .append_segments(id, &[sample_segment(1, 0, 100, "bye")])
        .unwrap();
    let tag = store.create_tag("ephemeral", None).unwrap();
    store.add_tag(id, tag).unwrap();
    store
        .finish_conversation(id, 100, ConversationStatus::Completed)
        .unwrap();

    store.delete_conversation(id).unwrap();

    assert!(matches!(
        store.get_conversation(id).unwrap_err(),
        StoreError::NotFound(_)
    ));
    let seg_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(seg_count, 0);
    let link_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM conversation_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(link_count, 0);
    // The tag itself survives deleting a conversation it was applied to.
    assert_eq!(store.list_tags().unwrap().len(), 1);
}

#[test]
fn deleting_a_group_leaves_its_conversations_ungrouped() {
    let (_dir, store) = open_temp();
    let group = store.create_group("Standup", 0).unwrap();
    let mut new_conv = sample_conversation("daily", 0);
    new_conv.group = Some(group);
    let id = store.create_conversation(&new_conv).unwrap();

    store.delete_group(group).unwrap();

    let conv = store.get_conversation(id).unwrap();
    assert_eq!(conv.group, None);
}

#[test]
fn tags_are_idempotent_and_unique_case_insensitively() {
    let (_dir, store) = open_temp();
    let a = store.create_tag("Work", None).unwrap();
    let b = store.create_tag("work", None).unwrap();
    let c = store.create_tag("WORK", Some("#ff0000")).unwrap();
    assert_eq!(a, b);
    assert_eq!(b, c);
    assert_eq!(store.list_tags().unwrap().len(), 1);

    let id = store
        .create_conversation(&sample_conversation("tagged", 0))
        .unwrap();
    store.add_tag(id, a).unwrap();
    store.add_tag(id, a).unwrap(); // idempotent, no error, no duplicate row
    let link_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM conversation_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(link_count, 1);

    store.remove_tag(id, a).unwrap();
    store.remove_tag(id, a).unwrap(); // idempotent on the way out too
    let link_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM conversation_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(link_count, 0);
}

#[test]
fn list_conversations_filters_by_group_tag_and_query() {
    let (_dir, store) = open_temp();
    let group = store.create_group("Work", 0).unwrap();
    let tag = store.create_tag("urgent", None).unwrap();

    let mut grouped = sample_conversation("Sprint planning", 10);
    grouped.group = Some(group);
    let grouped_id = store.create_conversation(&grouped).unwrap();
    store.add_tag(grouped_id, tag).unwrap();

    let ungrouped_id = store
        .create_conversation(&sample_conversation("Casual chat", 20))
        .unwrap();

    let by_group = store
        .list_conversations(&ConversationFilter {
            group: Some(group),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_group.len(), 1);
    assert_eq!(by_group[0].id, grouped_id);
    assert_eq!(by_group[0].group_name.as_deref(), Some("Work"));
    assert_eq!(by_group[0].tags.len(), 1);

    let by_tag = store
        .list_conversations(&ConversationFilter {
            tag: Some(tag),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].id, grouped_id);

    let by_query = store
        .list_conversations(&ConversationFilter {
            query: Some("Casual".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_query.len(), 1);
    assert_eq!(by_query[0].id, ungrouped_id);

    let all = store
        .list_conversations(&ConversationFilter::default())
        .unwrap();
    assert_eq!(all.len(), 2);
}

/// `%` and `_` are SQLite `LIKE` wildcards, but `query` is free-form user
/// input, not a pattern the user authored on purpose. Searching for a title
/// containing a literal `%` must not also match titles that merely contain
/// the text around it, and a lone `_` must not match every single-character
/// gap.
#[test]
fn list_conversations_query_treats_like_wildcards_as_literal_text() {
    let (_dir, store) = open_temp();
    store
        .create_conversation(&sample_conversation("Q3 roadmap", 10))
        .unwrap();
    store
        .create_conversation(&sample_conversation("100% done", 20))
        .unwrap();
    store
        .create_conversation(&sample_conversation("100X done", 30))
        .unwrap();

    // A literal "%" must match only the title that actually contains one,
    // not every title (which `LIKE '%100%'` would do if "%" were treated as
    // a wildcard instead of literal text).
    let hits = store
        .list_conversations(&ConversationFilter {
            query: Some("100%".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "100% done");

    // A literal "_" must not match "100X done" as if "_" meant "any one
    // character".
    let hits = store
        .list_conversations(&ConversationFilter {
            query: Some("100_done".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(hits.is_empty());
}

/// Deleting a conversation cascades to `segments`, but `segments_fts` is a
/// separate external-content index kept in sync by triggers (schema.rs).
/// This proves the cascade delete actually fires `segments_fts_ad` for each
/// removed row, not just that the `segments` rows themselves are gone.
#[test]
fn deleting_a_conversation_removes_its_text_from_search_too() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("searchable", 0))
        .unwrap();
    store
        .append_segments(
            id,
            &[sample_segment(1, 0, 1000, "unforgettable giraffe fact")],
        )
        .unwrap();
    assert_eq!(store.search("giraffe", 10).unwrap().len(), 1);
    store
        .finish_conversation(id, 1_000, ConversationStatus::Completed)
        .unwrap();

    store.delete_conversation(id).unwrap();

    assert_eq!(
        store.search("giraffe", 10).unwrap().len(),
        0,
        "deleted conversation's text must not still be findable in FTS"
    );
}

/// A corrupted or hand-edited row with a negative `start_ms`/`seq` must not
/// silently wrap to a huge `u64` when read back (`i64::MIN as u64` would be
/// the largest possible value, which would sort last and misrender any
/// duration/timestamp built from it) -- it should be reported as the
/// corruption it is and clamped to 0.
#[test]
fn negative_stored_values_are_clamped_not_wrapped() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("corrupted row", 0))
        .unwrap();
    // Bypass the normal u64-typed API to simulate a row written by something
    // other than `append_segments` (or just bit-rot) with a negative value
    // in a column the domain type treats as unsigned.
    store
        .conn
        .execute(
            "INSERT INTO segments (conversation_id, seq, track, start_ms, end_ms, text)
             VALUES (?1, -1, 'selected', -5, 100, 'ok')",
            rusqlite::params![id.get()],
        )
        .unwrap();

    let loaded = store.load_segments(id).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].seq, 0, "negative seq must clamp to 0, not wrap");
    assert_eq!(
        loaded[0].start_ms, 0,
        "negative start_ms must clamp to 0, not wrap to near u64::MAX"
    );
}

#[test]
fn fts_search_handles_quotes_and_operator_words_without_erroring() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("meeting", 0))
        .unwrap();
    store
        .append_segments(
            id,
            &[
                sample_segment(1, 0, 1000, "the quick brown fox jumps"),
                sample_segment(2, 1000, 2000, "AND then it ran away"),
            ],
        )
        .unwrap();

    let hits = store.search("fox", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].conversation_id, id);

    // A literal double quote in the query must not be an FTS5 syntax error.
    let hits = store.search("foo \"bar", 10).unwrap();
    assert_eq!(hits.len(), 0);

    // "AND" typed as a plain word searches for the word, not the operator,
    // and must not error either way.
    let hits = store.search("AND", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.to_lowercase().contains("and"));

    let empty = store.search("", 10).unwrap();
    assert_eq!(empty.len(), 0);
}

#[test]
fn segments_load_ordered_by_start_then_seq() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("ordering", 0))
        .unwrap();
    // Inserted out of order on purpose.
    store
        .append_segments(
            id,
            &[
                sample_segment(5, 5000, 6000, "fifth"),
                sample_segment(1, 1000, 2000, "first"),
                sample_segment(2, 1000, 2000, "second-same-start"),
            ],
        )
        .unwrap();

    let loaded = store.load_segments(id).unwrap();
    let texts: Vec<&str> = loaded.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["first", "second-same-start", "fifth"]);
}

/// A chunk is appended once. If an ambiguous commit ever made the caller retry,
/// duplicating the user's transcript lines would be the worst outcome, so the
/// schema makes the second attempt fail instead.
#[test]
fn appending_the_same_segment_twice_is_rejected() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("dup", 1_700_000_000_000))
        .unwrap();
    let seg = sample_segment(0, 0, 1000, "once only");

    store
        .append_segments(id, std::slice::from_ref(&seg))
        .unwrap();
    let err = store.append_segments(id, &[seg]).unwrap_err();
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "a duplicate append must surface, got {err:?}"
    );
    assert_eq!(store.load_segments(id).unwrap().len(), 1);
}

/// Opening a library written by a newer build must refuse rather than operate
/// against a schema it does not know.
#[test]
fn a_newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    Store::open(&path).expect("create at current schema");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 999i64).unwrap();
    drop(conn);

    let err = match Store::open(&path) {
        Err(e) => e,
        Ok(_) => panic!("opening a newer schema should have been refused"),
    };
    assert!(
        matches!(err, StoreError::SchemaTooNew { found: 999, .. }),
        "expected SchemaTooNew, got {err:?}"
    );
}

/// The conversation being recorded right now is reachable from the sidebar
/// like any other, and deleting it cascades the live transcript away and
/// leaves the session appending to a row that is gone — every later append
/// and the closing `finish_conversation` fail.
#[test]
fn the_conversation_being_recorded_cannot_be_deleted() {
    let (_dir, store) = open_temp();
    let active = store
        .create_conversation(&sample_conversation("recording now", 0))
        .unwrap();
    store
        .append_segments(active, &[sample_segment(0, 0, 1_000, "live")])
        .unwrap();

    let err = store.delete_conversation(active).unwrap_err();
    assert!(
        matches!(err, StoreError::ConversationActive { id } if id == active),
        "expected ConversationActive, got {err:?}"
    );
    assert!(
        err.to_string().contains("stop recording"),
        "the message has to say what to do: {err}"
    );

    // The transcript is intact and the session can keep writing to it.
    assert_eq!(store.load_segments(active).unwrap().len(), 1);
    store
        .append_segments(active, &[sample_segment(1, 1_000, 2_000, "and more")])
        .unwrap();

    // Once it is no longer recording it deletes like anything else.
    store
        .finish_conversation(active, 2_000, ConversationStatus::Completed)
        .unwrap();
    store.delete_conversation(active).unwrap();
}

/// "There is no such row" and "the row is the one you are recording into" are
/// different problems with different answers, so they cannot share an error.
#[test]
fn deleting_a_missing_conversation_is_still_not_found() {
    let (_dir, store) = open_temp();
    let err = store
        .delete_conversation(fc_core::ConversationId(999))
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn import_writes_the_conversation_and_its_segments_at_once() {
    let (_dir, store) = open_temp();
    let mut new = sample_conversation("imported meeting", 1_000);
    new.voxtype_meeting_id = Some("meeting-1".into());

    let id = store
        .import_conversation(
            &new,
            &[
                sample_segment(0, 0, 1_000, "hello"),
                sample_segment(1, 1_000, 2_000, "there"),
            ],
            3_000,
            ConversationStatus::Completed,
        )
        .unwrap();

    let conv = store.get_conversation(id).unwrap();
    assert_eq!(conv.status, ConversationStatus::Completed);
    assert_eq!(conv.ended_at, Some(3_000));
    assert_eq!(conv.voxtype_meeting_id.as_deref(), Some("meeting-1"));
    assert_eq!(store.load_segments(id).unwrap().len(), 2);
}

/// The old three-transaction sequence committed `voxtype_meeting_id` first,
/// so a failure while appending left a half-empty `Active` row that the
/// importer's dedupe reads as "already imported" — permanently blocking a
/// retry while holding a fragment of the meeting.
#[test]
fn a_failed_import_leaves_no_row_to_block_a_retry() {
    let (_dir, store) = open_temp();
    let mut new = sample_conversation("imported meeting", 1_000);
    new.voxtype_meeting_id = Some("meeting-1".into());

    let err = store
        .import_conversation(
            &new,
            &[
                sample_segment(0, 0, 1_000, "first"),
                // Same (track, seq): `idx_segments_identity` rejects it.
                sample_segment(0, 1_000, 2_000, "duplicate identity"),
            ],
            3_000,
            ConversationStatus::Completed,
        )
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "a duplicate seq must surface, got {err:?}"
    );

    assert!(store
        .list_conversations(&ConversationFilter::default())
        .unwrap()
        .is_empty());
    let segments: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(segments, 0);

    // And the meeting can now be imported again.
    store
        .import_conversation(
            &new,
            &[sample_segment(0, 0, 1_000, "first")],
            3_000,
            ConversationStatus::Completed,
        )
        .unwrap();
}

#[test]
fn import_rejects_a_provisional_segment_without_writing_anything() {
    let (_dir, store) = open_temp();
    let mut provisional = sample_segment(0, 0, 500, "partial");
    provisional.provisional = true;

    let err = store
        .import_conversation(
            &sample_conversation("imported", 0),
            &[provisional],
            500,
            ConversationStatus::Completed,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::ProvisionalSegment { .. }));
    assert!(store
        .list_conversations(&ConversationFilter::default())
        .unwrap()
        .is_empty());
}

/// A reaped conversation with no `ended_at` is indistinguishable from a
/// running one to anything reading the row. The furthest committed segment is
/// the best evidence of when the recording actually stopped.
#[test]
fn reaping_dates_a_conversation_from_its_last_committed_segment() {
    let (_dir, store) = open_temp();
    let crashed = store
        .create_conversation(&sample_conversation("crashed", 1_000))
        .unwrap();
    store
        .append_segments(
            crashed,
            &[
                sample_segment(0, 0, 4_000, "first"),
                sample_segment(1, 4_000, 9_500, "last"),
            ],
        )
        .unwrap();
    let empty = store
        .create_conversation(&sample_conversation(
            "crashed before anything committed",
            2_000,
        ))
        .unwrap();

    assert_eq!(store.reap_active().unwrap(), 2);

    assert_eq!(
        store.get_conversation(crashed).unwrap().ended_at,
        Some(10_500),
        "started_at plus the furthest end_ms that reached disk"
    );
    assert_eq!(
        store.get_conversation(empty).unwrap().ended_at,
        Some(2_000),
        "nothing committed, so it ended when it started"
    );
}

#[test]
fn list_conversations_full_carries_whole_rows_and_honors_every_filter() {
    let (_dir, store) = open_temp();
    let group = store.create_group("Work", 0).unwrap();
    let tag = store.create_tag("urgent", None).unwrap();

    let mut grouped = sample_conversation("Sprint planning", 10);
    grouped.group = Some(group);
    grouped.mic_track = true;
    grouped.voxtype_meeting_id = Some("vx-7".into());
    let grouped_id = store.create_conversation(&grouped).unwrap();
    store.add_tag(grouped_id, tag).unwrap();
    let casual_id = store
        .create_conversation(&sample_conversation("Casual chat", 20))
        .unwrap();
    let standup_id = store
        .create_conversation(&sample_conversation("Standup", 30))
        .unwrap();

    // The whole record, not a summary: source, engine, mic track, imported id.
    let by_group = store
        .list_conversations_full(&ConversationFilter {
            group: Some(group),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_group.len(), 1);
    let (conversation, tags) = &by_group[0];
    assert_eq!(conversation.id, grouped_id);
    assert_eq!(conversation.source, sample_source());
    assert_eq!(conversation.engine, sample_engine());
    assert!(conversation.mic_track);
    assert_eq!(conversation.voxtype_meeting_id.as_deref(), Some("vx-7"));
    assert_eq!(conversation.status, ConversationStatus::Active);
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "urgent");

    let by_tag = store
        .list_conversations_full(&ConversationFilter {
            tag: Some(tag),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].0.id, grouped_id);

    let by_query = store
        .list_conversations_full(&ConversationFilter {
            query: Some("Casual".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_query.len(), 1);
    assert_eq!(by_query[0].0.id, casual_id);

    // Newest first, so offset 1 skips the standup.
    let paged = store
        .list_conversations_full(&ConversationFilter {
            limit: Some(1),
            offset: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].0.id, casual_id);

    let all = store
        .list_conversations_full(&ConversationFilter::default())
        .unwrap();
    let ids: Vec<_> = all.iter().map(|(c, _)| c.id).collect();
    assert_eq!(ids, vec![standup_id, casual_id, grouped_id]);
    // Only the tagged conversation has tags; the others get an empty list, not
    // a missing entry.
    assert!(all
        .iter()
        .all(|(c, tags)| (c.id == grouped_id) == !tags.is_empty()));
}

/// `Segment::text` is documented as one line. A newline survives to
/// terminate an SRT/VTT cue early and to break the text export's
/// one-line-per-segment promise, so the store is where it is normalised.
#[test]
fn a_multiline_segment_is_stored_as_a_single_line() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("multiline", 0))
        .unwrap();
    store
        .append_segments(
            id,
            &[sample_segment(
                0,
                0,
                1_000,
                "  first part\r\n\r\nsecond part\tthird\n",
            )],
        )
        .unwrap();

    let loaded = store.load_segments(id).unwrap();
    assert_eq!(loaded[0].text, "first part second part third");

    // The import path carries the same text from voxtype's own exports.
    let imported = store
        .import_conversation(
            &sample_conversation("imported multiline", 0),
            &[sample_segment(0, 0, 1_000, "line one\nline two")],
            1_000,
            ConversationStatus::Completed,
        )
        .unwrap();
    assert_eq!(
        store.load_segments(imported).unwrap()[0].text,
        "line one line two"
    );
}

/// `source_index` is an `INTEGER` column and a `u32` in the domain. `as u32`
/// truncates an out-of-range value into a plausible-looking index, and
/// `parec --monitor-stream=<that>` would record a different application than
/// the row names.
#[test]
fn an_out_of_range_source_index_is_dropped_not_truncated() {
    let (_dir, store) = open_temp();
    let id = store
        .create_conversation(&sample_conversation("sink input", 0))
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE conversations SET source_index = ?1 WHERE id = ?2",
            rusqlite::params![i64::from(u32::MAX) + 1, id.get()],
        )
        .unwrap();

    assert_eq!(
        store.get_conversation(id).unwrap().source.index,
        None,
        "4294967296 must not read back as index 0"
    );
}

/// A library whose file or directory lost write permission still holds every
/// past transcript. Refusing to open it reports a problem that only affects
/// recording by throwing the archive away.
struct ReadOnlyLibrary {
    dir: TempDir,
    path: PathBuf,
}

impl ReadOnlyLibrary {
    /// `None` when the test cannot be run as this user — a process that
    /// ignores permission bits (root) would see a perfectly writable library.
    fn new() -> Option<Self> {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("library.db");
        {
            let store = Store::open(&path).unwrap();
            let id = store
                .create_conversation(&sample_conversation("archived", 1_000))
                .unwrap();
            store
                .append_segments(id, &[sample_segment(0, 0, 2_000, "still readable")])
                .unwrap();
            store
                .finish_conversation(id, 3_000, ConversationStatus::Completed)
                .unwrap();
        }
        set_mode(&path, 0o444);
        set_mode(dir.path(), 0o555);

        let library = Self { dir, path };
        if std::fs::File::create(library.dir.path().join("probe")).is_ok() {
            return None;
        }
        Some(library)
    }
}

impl Drop for ReadOnlyLibrary {
    /// Restored so `TempDir`'s own cleanup can still remove the directory.
    fn drop(&mut self) {
        set_mode(self.dir.path(), 0o755);
        for entry in std::fs::read_dir(self.dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            set_mode(&entry.path(), 0o644);
        }
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_read_only_library_still_opens_and_reads() {
    let Some(library) = ReadOnlyLibrary::new() else {
        eprintln!("skipped: this process ignores file permissions");
        return;
    };

    let store = Store::open(&library.path).expect("a read-only library must still open");
    assert!(store.is_read_only());

    let rows = store
        .list_conversations_full(&ConversationFilter::default())
        .unwrap();
    assert_eq!(rows.len(), 1, "every past transcript is still there");
    assert_eq!(rows[0].0.title, "archived");
    assert_eq!(store.load_segments(rows[0].0.id).unwrap().len(), 1);
    assert_eq!(store.search("readable", 10).unwrap().len(), 1);
}

#[test]
fn a_read_only_library_refuses_writes_by_name() {
    let Some(library) = ReadOnlyLibrary::new() else {
        eprintln!("skipped: this process ignores file permissions");
        return;
    };
    let store = Store::open(&library.path).unwrap();
    let existing = store
        .list_conversations(&ConversationFilter::default())
        .unwrap()[0]
        .id;

    for err in [
        store
            .create_conversation(&sample_conversation("new", 0))
            .unwrap_err(),
        store
            .append_segments(existing, &[sample_segment(9, 0, 1, "x")])
            .unwrap_err(),
        store.rename_conversation(existing, "renamed").unwrap_err(),
        store.delete_conversation(existing).unwrap_err(),
        store.reap_active().unwrap_err(),
        store.create_group("Work", 0).unwrap_err(),
        store.create_tag("urgent", None).unwrap_err(),
    ] {
        assert!(
            matches!(err, StoreError::ReadOnly { .. }),
            "expected ReadOnly, got {err:?}"
        );
        assert!(
            err.to_string().contains("read-only"),
            "the message has to name the cause: {err}"
        );
    }
}

mod words {
    //! The registry: one entry per expression, lookups recorded, the link to
    //! a deleted conversation dropped without dropping the word.

    use super::open_temp;
    use crate::NewWord;

    fn new_word(expression: &str) -> NewWord {
        NewWord {
            expression: expression.to_owned(),
            context: "We should table this for now.".to_owned(),
            conversation: None,
            start_ms: Some(1_500),
            created_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn adding_the_same_expression_twice_lands_on_one_entry() {
        let (_dir, store) = open_temp();
        let first = store.add_word(&new_word("table this")).unwrap();
        let again = store.add_word(&new_word("Table This")).unwrap();
        assert_eq!(first, again, "capitalisation must not make a second entry");
        assert_eq!(store.list_words().unwrap().len(), 1);
    }

    #[test]
    fn a_lookup_is_recorded_and_read_back() {
        let (_dir, store) = open_temp();
        let id = store.add_word(&new_word("ballpark figure")).unwrap();
        assert!(!store.list_words().unwrap()[0].looked_up());
        store
            .set_word_lookup(
                id,
                Some("a rough estimate"),
                Some("una cifra aproximada"),
                None,
            )
            .unwrap();
        let word = &store.list_words().unwrap()[0];
        assert!(word.looked_up());
        assert_eq!(word.meaning.as_deref(), Some("a rough estimate"));
        assert_eq!(word.translation.as_deref(), Some("una cifra aproximada"));
        assert_eq!(word.example, None);
    }

    #[test]
    fn an_expression_is_found_under_any_capitalisation_or_not_at_all() {
        let (_dir, store) = open_temp();
        let id = store.add_word(&new_word("Ballpark Figure")).unwrap();
        let found = store.find_word("  ballpark figure ").unwrap().unwrap();
        assert_eq!(found.id, id);
        assert!(store.find_word("circle back").unwrap().is_none());
    }

    #[test]
    fn newest_first() {
        let (_dir, store) = open_temp();
        let mut older = new_word("older");
        older.created_at = 1;
        let mut newer = new_word("newer");
        newer.created_at = 2;
        store.add_word(&older).unwrap();
        store.add_word(&newer).unwrap();
        let listed: Vec<String> = store
            .list_words()
            .unwrap()
            .into_iter()
            .map(|w| w.expression)
            .collect();
        assert_eq!(listed, ["newer", "older"]);
    }

    #[test]
    fn deleting_removes_it_and_a_missing_id_is_an_error() {
        let (_dir, store) = open_temp();
        let id = store.add_word(&new_word("circle back")).unwrap();
        store.delete_word(id).unwrap();
        assert!(store.list_words().unwrap().is_empty());
        assert!(store.delete_word(id).is_err());
    }

    /// The word outlives the meeting it came from; only the link goes.
    #[test]
    fn deleting_the_conversation_keeps_the_word_and_drops_the_link() {
        let (_dir, store) = open_temp();
        let conversation = store
            .create_conversation(&super::sample_conversation("meeting", 0))
            .unwrap();
        let mut word = new_word("boil the ocean");
        word.conversation = Some(conversation);
        let id = store.add_word(&word).unwrap();
        // The store refuses to delete a conversation still being recorded,
        // which is right and not what this test is about.
        store
            .finish_conversation(conversation, 100, fc_core::ConversationStatus::Completed)
            .unwrap();
        store.delete_conversation(conversation).unwrap();
        let listed = store.list_words().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].conversation, None);
    }
}
