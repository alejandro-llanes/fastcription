use fc_core::{
    AudioSource, ConversationStatus, EngineInfo, Segment, SourceKind, Track,
};
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
    assert_eq!(version, 1);

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
        store.finish_conversation(id, 2000, ConversationStatus::Completed).unwrap();
    }
    // Reopening must not error, must not re-run migrations destructively, and
    // must still see the row written before the close.
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let convs = store.list_conversations(&ConversationFilter::default()).unwrap();
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
    store.finish_conversation(finished, 100, ConversationStatus::Completed).unwrap();
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

    let all = store.list_conversations(&ConversationFilter::default()).unwrap();
    assert_eq!(all.len(), 2);
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
    let id = store.create_conversation(&sample_conversation("dup", 1_700_000_000_000)).unwrap();
    let seg = sample_segment(0, 0, 1000, "once only");

    store.append_segments(id, std::slice::from_ref(&seg)).unwrap();
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
