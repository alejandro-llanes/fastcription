//! Versioned migrations, tracked through `PRAGMA user_version`.
//!
//! Steps are append-only: a released schema version is never edited in place,
//! only added to. A v1 database opened by a build that knows about v3 runs
//! steps 2 and 3 and ends up identical to a database created fresh at v3.

use rusqlite::Connection;

use crate::error::{Result, StoreError};

/// Each entry is the SQL that takes the schema from `index` to `index + 1`.
/// `user_version` after a fresh open equals `MIGRATIONS.len()`.
const MIGRATIONS: &[&str] = &[V1_INITIAL, V2_WORDS];

const V1_INITIAL: &str = r#"
CREATE TABLE conversations (
    id                  INTEGER PRIMARY KEY,
    title               TEXT NOT NULL,
    group_id            INTEGER REFERENCES groups(id) ON DELETE SET NULL,
    started_at          INTEGER NOT NULL,
    ended_at            INTEGER,
    status              TEXT NOT NULL,
    source_kind         TEXT NOT NULL,
    source_name         TEXT NOT NULL,
    source_desc         TEXT NOT NULL,
    source_application  TEXT,
    source_index        INTEGER,
    mic_track           INTEGER NOT NULL,
    engine              TEXT NOT NULL,
    model               TEXT NOT NULL,
    language            TEXT NOT NULL,
    backend             TEXT,
    voxtype_meeting_id  TEXT
);

CREATE TABLE groups (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL COLLATE NOCASE UNIQUE,
    created_at  INTEGER NOT NULL
);

CREATE TABLE tags (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL COLLATE NOCASE UNIQUE,
    color   TEXT
);

CREATE TABLE conversation_tags (
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    tag_id          INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (conversation_id, tag_id)
);

CREATE TABLE segments (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    track           TEXT NOT NULL,
    start_ms        INTEGER NOT NULL,
    end_ms          INTEGER NOT NULL,
    text            TEXT NOT NULL,
    translation     TEXT,
    speaker         TEXT,
    confidence      REAL
);

CREATE INDEX idx_conversations_group ON conversations(group_id);
CREATE INDEX idx_conversations_started_at ON conversations(started_at);
CREATE INDEX idx_conversation_tags_tag ON conversation_tags(tag_id);
CREATE INDEX idx_segments_conversation_order ON segments(conversation_id, start_ms, seq);

-- A chunk is appended exactly once. Without this, a retried append after an
-- ambiguous commit would silently duplicate lines in the user's transcript;
-- with it, the second attempt fails loudly and the caller can tell.
CREATE UNIQUE INDEX idx_segments_identity ON segments(conversation_id, track, seq);

-- External-content FTS5 index over segment text. "External content" means the
-- indexed text stays in `segments` only; the triggers below are what keep
-- `segments_fts` truthful as rows are written and removed.
CREATE VIRTUAL TABLE segments_fts USING fts5(
    text,
    content='segments',
    content_rowid='id',
    tokenize='unicode61'
);

CREATE TRIGGER segments_fts_ai AFTER INSERT ON segments BEGIN
    INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER segments_fts_ad AFTER DELETE ON segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;

CREATE TRIGGER segments_fts_au AFTER UPDATE ON segments BEGIN
    INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
END;
"#;

/// Applies any migration steps the database has not seen yet, inside one
/// transaction per step so a crash mid-migration cannot leave `user_version`
/// ahead of what was actually run.
/// The word registry (architecture D18).
///
/// `conversation_id` is `ON DELETE SET NULL` rather than `CASCADE`: a word the
/// reader did not know is still a word they did not know after the meeting
/// it came from is deleted. It only loses the link back.
const V2_WORDS: &str = r#"
CREATE TABLE words (
    id              INTEGER PRIMARY KEY,
    expression      TEXT NOT NULL,
    context         TEXT NOT NULL,
    conversation_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL,
    start_ms        INTEGER,
    meaning         TEXT,
    translation     TEXT,
    example         TEXT,
    created_at      INTEGER NOT NULL
);

CREATE INDEX idx_words_created_at ON words(created_at);

-- One entry per expression, however it was capitalised: "ballpark figure"
-- added from two meetings is one thing the reader did not know, not two.
CREATE UNIQUE INDEX idx_words_expression ON words(expression COLLATE NOCASE);
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let current = current as usize;

    if current > MIGRATIONS.len() {
        // Running queries against a schema this build has never seen risks
        // writing rows a newer build would read as corrupt. Refusing to open
        // costs the user a downgrade warning; proceeding could cost them a
        // transcript.
        return Err(StoreError::SchemaTooNew {
            found: current,
            known: MIGRATIONS.len(),
        });
    }

    for (i, step) in MIGRATIONS.iter().enumerate().skip(current) {
        let target = i + 1;
        tracing::info!(from = i, to = target, "applying store migration");
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(None, "user_version", target as i64)?;
        tx.commit()?;
    }

    Ok(())
}

/// Checks the schema of a database that cannot be migrated because it was
/// opened read-only (`Store::open_read_only`).
///
/// Only "exactly current" is acceptable here. A newer schema may hold rows
/// this build would misread, and an older one needs a migration — which is a
/// write, which is the thing this connection cannot do. Doubles as the probe
/// that tells a readable database from a WAL database whose wal-index cannot
/// be created, since both answers come from the same first page read.
pub fn verify(conn: &Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let current = current as usize;
    match current.cmp(&MIGRATIONS.len()) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => Err(StoreError::SchemaTooNew {
            found: current,
            known: MIGRATIONS.len(),
        }),
        std::cmp::Ordering::Less => Err(StoreError::SchemaNeedsUpgrade {
            found: current,
            known: MIGRATIONS.len(),
        }),
    }
}
