//! Conversation CRUD and the filtered list view.

use fc_core::{
    AudioSource, Conversation, ConversationId, ConversationStatus, EngineInfo, GroupId, Segment,
    SourceKind, Tag, TagId, UnixMillis,
};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{Result, StoreError};
use crate::model::{ConversationFilter, ConversationSummary, NewConversation};
use crate::Store;

/// The columns `row_to_conversation` expects, in its order. Shared so
/// `get_conversation` and `list_conversations_full` cannot drift apart.
const CONVERSATION_COLUMNS: &str = "c.id, c.title, c.group_id, c.started_at, c.ended_at, c.status,
     c.source_kind, c.source_name, c.source_desc, c.source_application, c.source_index,
     c.mic_track, c.engine, c.model, c.language, c.backend, c.voxtype_meeting_id";

/// The `WHERE`/`ORDER`/`LIMIT` tail both list queries share, with the same
/// five bound parameters in the same order: group, title, tag, limit, offset.
const LIST_FILTER: &str = "WHERE (?1 IS NULL OR c.group_id = ?1)
       AND (?2 IS NULL OR c.title LIKE '%' || ?2 || '%' ESCAPE '\\')
       AND (
            ?3 IS NULL OR EXISTS (
                SELECT 1 FROM conversation_tags ct
                WHERE ct.conversation_id = c.id AND ct.tag_id = ?3
            )
       )
     ORDER BY c.started_at DESC
     LIMIT ?4 OFFSET ?5";

impl Store {
    pub fn create_conversation(&self, new: &NewConversation) -> Result<ConversationId> {
        self.writable("starting a conversation")?;
        insert_conversation(&self.conn, new)?;
        Ok(ConversationId(self.conn.last_insert_rowid()))
    }

    /// Creates a finished conversation and all of its segments as one
    /// transaction.
    ///
    /// This exists for import, where the three-call sequence
    /// (`create_conversation`, `append_segments`, `finish_conversation`) is
    /// three separate transactions and the first one commits
    /// `voxtype_meeting_id`. A failure in the second then leaves a half-empty
    /// `Active` row that the importer's own dedupe reads as "already
    /// imported" — so the meeting can never be imported again, and what is in
    /// the library is a fragment. All of it or none of it is the only honest
    /// outcome.
    ///
    /// Deliberately not a general `Store::transaction()`: import calls the
    /// voxtype binary per meeting, and an exposed transaction would invite a
    /// caller to hold a write lock across a subprocess.
    pub fn import_conversation(
        &self,
        new: &NewConversation,
        segments: &[Segment],
        ended_at: UnixMillis,
        status: ConversationStatus,
    ) -> Result<ConversationId> {
        self.writable("importing a conversation")?;
        reject_provisional(segments)?;

        let tx = self.conn.unchecked_transaction()?;
        let id = ConversationId({
            insert_conversation(&tx, new)?;
            tx.last_insert_rowid()
        });
        insert_segments(&tx, id, segments)?;
        let changed = tx.execute(
            "UPDATE conversations SET ended_at = ?1, status = ?2 WHERE id = ?3",
            params![ended_at, status.as_str(), id.get()],
        )?;
        require_row(changed, || format!("conversation {id} not found"))?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get_conversation(&self, id: ConversationId) -> Result<Conversation> {
        // `prepare_cached`: the history view fetches one conversation per
        // click and the importer one per candidate meeting, so this is called
        // often enough that re-compiling the statement each time is pure
        // waste.
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {CONVERSATION_COLUMNS} FROM conversations c WHERE c.id = ?1"
        ))?;
        stmt.query_row(params![id.get()], row_to_conversation)
            .map_err(|e| not_found_or(e, || format!("conversation {id} not found")))
    }

    pub fn rename_conversation(&self, id: ConversationId, title: &str) -> Result<()> {
        self.writable("renaming a conversation")?;
        let changed = self.conn.execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![title, id.get()],
        )?;
        require_row(changed, || format!("conversation {id} not found"))
    }

    pub fn set_group(&self, id: ConversationId, group: Option<GroupId>) -> Result<()> {
        self.writable("regrouping a conversation")?;
        let changed = self.conn.execute(
            "UPDATE conversations SET group_id = ?1 WHERE id = ?2",
            params![group.map(GroupId::get), id.get()],
        )?;
        require_row(changed, || format!("conversation {id} not found"))
    }

    pub fn finish_conversation(
        &self,
        id: ConversationId,
        ended_at: UnixMillis,
        status: ConversationStatus,
    ) -> Result<()> {
        self.writable("finishing a conversation")?;
        let changed = self.conn.execute(
            "UPDATE conversations SET ended_at = ?1, status = ?2 WHERE id = ?3",
            params![ended_at, status.as_str(), id.get()],
        )?;
        require_row(changed, || format!("conversation {id} not found"))
    }

    /// Any conversation still `Active` after an open was left that way by a
    /// process that never called `finish_conversation` — most likely a crash.
    /// Its committed segments are untouched; only the status changes.
    ///
    /// `ended_at` is set at the same time, from the last segment that made it
    /// to disk. Leaving it NULL would make an interrupted conversation
    /// indistinguishable from a running one to anything reading the row, and
    /// the last committed segment's end is the best evidence of when the
    /// recording actually stopped.
    pub fn reap_active(&self) -> Result<usize> {
        self.writable("reaping interrupted conversations")?;
        let changed = self.conn.execute(
            "UPDATE conversations
                SET status = ?1,
                    ended_at = COALESCE(ended_at, started_at + (
                        SELECT COALESCE(MAX(end_ms), 0) FROM segments
                         WHERE segments.conversation_id = conversations.id
                    ))
              WHERE status = ?2",
            params![
                ConversationStatus::Interrupted.as_str(),
                ConversationStatus::Active.as_str(),
            ],
        )?;
        if changed > 0 {
            tracing::warn!(
                count = changed,
                "reaped conversations left active by a dead process"
            );
        }
        Ok(changed)
    }

    /// Cascades to `segments` and `conversation_tags` via `ON DELETE CASCADE`.
    ///
    /// Refuses the conversation being recorded right now. The cascade would
    /// take the live transcript with it and leave the session appending to a
    /// row that no longer exists, so every later append and the closing
    /// `finish_conversation` would fail. Enforced here rather than in the UI
    /// so no caller can route around it.
    pub fn delete_conversation(&self, id: ConversationId) -> Result<()> {
        self.writable("deleting a conversation")?;
        let changed = self.conn.execute(
            "DELETE FROM conversations WHERE id = ?1 AND status <> ?2",
            params![id.get(), ConversationStatus::Active.as_str()],
        )?;
        if changed > 0 {
            return Ok(());
        }
        // Nothing was deleted, for one of two reasons that want different
        // words: there is no such row, or there is one and it is active (the
        // only status the statement above excludes).
        let exists = self
            .conn
            .query_row(
                "SELECT 1 FROM conversations WHERE id = ?1",
                params![id.get()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if exists {
            Err(StoreError::ConversationActive { id })
        } else {
            Err(StoreError::NotFound(format!("conversation {id} not found")))
        }
    }

    /// Filtered, paginated list of conversations for the sidebar. Tags for the
    /// whole page are fetched in one follow-up query, not one per row.
    pub fn list_conversations(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT c.id, c.title, c.started_at, g.name AS group_name
             FROM conversations c
             LEFT JOIN groups g ON g.id = c.group_id
             {LIST_FILTER}"
        ))?;

        let rows = stmt.query_map(rusqlite::params_from_iter(list_params(filter)), |row| {
            let id = ConversationId(row.get(0)?);
            Ok(ConversationSummary {
                id,
                title: row.get(1)?,
                started_at: row.get(2)?,
                group_name: row.get(3)?,
                tags: Vec::new(),
            })
        })?;

        let mut summaries = rows.collect::<rusqlite::Result<Vec<ConversationSummary>>>()?;
        let ids: Vec<ConversationId> = summaries.iter().map(|s| s.id).collect();
        let mut tags = self.tags_for_conversations(&ids)?;
        for summary in &mut summaries {
            summary.tags = tags.remove(&summary.id).unwrap_or_default();
        }
        Ok(summaries)
    }

    /// The same page as [`Store::list_conversations`], but as whole
    /// [`Conversation`]s with their tags.
    ///
    /// The views want the complete record — source, engine, status, the
    /// imported voxtype id — which a summary does not carry, and the only way
    /// to get it used to be one `get_conversation` per listed row: 201
    /// queries for a 200-row page, on every reload. This is two.
    pub fn list_conversations_full(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<(Conversation, Vec<Tag>)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CONVERSATION_COLUMNS}
             FROM conversations c
             {LIST_FILTER}"
        ))?;

        let rows = stmt.query_map(
            rusqlite::params_from_iter(list_params(filter)),
            row_to_conversation,
        )?;
        let conversations = rows.collect::<rusqlite::Result<Vec<Conversation>>>()?;

        let ids: Vec<ConversationId> = conversations.iter().map(|c| c.id).collect();
        let mut tags = self.tags_for_conversations(&ids)?;
        Ok(conversations
            .into_iter()
            .map(|c| {
                let own = tags.remove(&c.id).unwrap_or_default();
                (c, own)
            })
            .collect())
    }

    pub(crate) fn tags_for_conversations(
        &self,
        ids: &[ConversationId],
    ) -> Result<std::collections::HashMap<ConversationId, Vec<Tag>>> {
        use std::collections::HashMap;

        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT ct.conversation_id, t.id, t.name, t.color
             FROM conversation_tags ct
             JOIN tags t ON t.id = ct.tag_id
             WHERE ct.conversation_id IN ({placeholders})
             ORDER BY t.name COLLATE NOCASE"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(ids.iter().map(|id| id.get()));
        let rows = stmt.query_map(params, |row| {
            let conv_id: i64 = row.get(0)?;
            Ok((
                ConversationId(conv_id),
                Tag {
                    id: TagId(row.get(1)?),
                    name: row.get(2)?,
                    color: row.get(3)?,
                },
            ))
        })?;

        let mut map: HashMap<ConversationId, Vec<Tag>> = HashMap::new();
        for row in rows {
            let (conv_id, tag) = row?;
            map.entry(conv_id).or_default().push(tag);
        }
        Ok(map)
    }
}

/// The five parameters [`LIST_FILTER`] binds, as owned values so both list
/// queries can share one builder.
///
/// `query` is free text a user typed into a search box, not a LIKE pattern
/// they authored on purpose: SQLite's own wildcards are escaped so searching
/// for a title that happens to contain a literal `%` or `_` matches that
/// literal text instead of matching "anything"/"any one character" (see
/// `ConversationFilter::query`'s doc comment).
fn list_params(filter: &ConversationFilter) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    vec![
        filter
            .group
            .map(GroupId::get)
            .map_or(Value::Null, Value::Integer),
        filter
            .query
            .as_deref()
            .map(escape_like_pattern)
            .map_or(Value::Null, Value::Text),
        filter
            .tag
            .map(TagId::get)
            .map_or(Value::Null, Value::Integer),
        Value::Integer(filter.limit.map(i64::from).unwrap_or(-1)),
        Value::Integer(filter.offset.map(i64::from).unwrap_or(0)),
    ]
}

fn insert_conversation(conn: &Connection, new: &NewConversation) -> Result<()> {
    conn.execute(
        "INSERT INTO conversations (
            title, group_id, started_at, ended_at, status,
            source_kind, source_name, source_desc, source_application, source_index,
            mic_track, engine, model, language, backend, voxtype_meeting_id
        ) VALUES (?1,?2,?3,NULL,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            new.title,
            new.group.map(GroupId::get),
            new.started_at,
            ConversationStatus::Active.as_str(),
            new.source.kind.as_str(),
            new.source.name,
            new.source.description,
            new.source.application,
            new.source.index.map(i64::from),
            new.mic_track as i64,
            new.engine.engine,
            new.engine.model,
            new.engine.language,
            new.engine.backend,
            new.voxtype_meeting_id,
        ],
    )?;
    Ok(())
}

/// Escapes `\`, `%` and `_` so `input` matches only its literal text when
/// substituted into a `LIKE ... ESCAPE '\'` pattern, regardless of which of
/// SQLite's own wildcard characters it happens to contain.
fn escape_like_pattern(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn require_row(changed: usize, msg: impl FnOnce() -> String) -> Result<()> {
    if changed == 0 {
        Err(StoreError::NotFound(msg()))
    } else {
        Ok(())
    }
}

pub(crate) fn not_found_or(err: rusqlite::Error, msg: impl FnOnce() -> String) -> StoreError {
    match err {
        rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(msg()),
        other => StoreError::Sqlite(other),
    }
}

fn row_to_conversation(row: &Row) -> rusqlite::Result<Conversation> {
    let status_str: String = row.get(5)?;
    let status = ConversationStatus::parse(&status_str).unwrap_or_else(|| {
        tracing::error!(value = %status_str, "unrecognised conversation status, defaulting to Interrupted");
        ConversationStatus::Interrupted
    });
    let kind_str: String = row.get(6)?;
    let kind = SourceKind::parse(&kind_str).unwrap_or_else(|| {
        tracing::error!(value = %kind_str, "unrecognised source kind, defaulting to Device");
        SourceKind::Device
    });
    let index: Option<i64> = row.get(10)?;
    let group: Option<i64> = row.get(2)?;
    let mic_track: i64 = row.get(11)?;

    Ok(Conversation {
        id: ConversationId(row.get(0)?),
        title: row.get(1)?,
        group: group.map(GroupId),
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        status,
        source: AudioSource {
            kind,
            name: row.get(7)?,
            description: row.get(8)?,
            application: row.get(9)?,
            index: index.and_then(source_index),
        },
        mic_track: mic_track != 0,
        engine: EngineInfo {
            engine: row.get(12)?,
            model: row.get(13)?,
            language: row.get(14)?,
            backend: row.get(15)?,
        },
        voxtype_meeting_id: row.get(16)?,
    })
}

/// A sink-input index is a `u32` in the domain and an `INTEGER` in SQLite.
/// `as u32` on a corrupted or hand-edited value truncates it into a
/// plausible-looking index, and `parec --monitor-stream=<that>` would then
/// record a different application's audio than the row names. Dropping the
/// value instead makes the source unresolvable, which is the state the UI
/// already knows how to ask about (architecture §5).
fn source_index(value: i64) -> Option<u32> {
    match u32::try_from(value) {
        Ok(index) => Some(index),
        Err(_) => {
            tracing::error!(
                value,
                "source_index out of range for a sink-input index, dropping it"
            );
            None
        }
    }
}

/// Shared by `append_segments` and `import_conversation`: a provisional
/// segment reaching the store is a caller bug (architecture §3), so the whole
/// batch is rejected rather than one row silently dropped.
pub(crate) fn reject_provisional(segments: &[Segment]) -> Result<()> {
    match segments.iter().find(|s| s.provisional) {
        Some(bad) => Err(StoreError::ProvisionalSegment {
            track: bad.track,
            seq: bad.seq,
        }),
        None => Ok(()),
    }
}

/// Shared by `append_segments` and `import_conversation`.
pub(crate) fn insert_segments(
    conn: &Connection,
    conversation: ConversationId,
    segments: &[Segment],
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO segments (
            conversation_id, seq, track, start_ms, end_ms, text, translation, speaker, confidence
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?;
    for s in segments {
        stmt.execute(params![
            conversation.get(),
            s.seq as i64,
            s.track.as_str(),
            s.start_ms as i64,
            s.end_ms as i64,
            // `Segment::text` is documented as one line; this is where that
            // stops being a convention. A newline here would terminate an
            // SRT/VTT cue and break the text export's line-per-segment
            // promise, and both the finalised-utterance path and the voxtype
            // import can carry one.
            fc_core::single_line(&s.text),
            s.translation,
            s.speaker,
            s.confidence,
        ])
        .map_err(|e| {
            crate::queries::segments::foreign_key_violation_as_not_found(e, conversation)
        })?;
    }
    Ok(())
}
