//! Conversation CRUD and the filtered list view.

use fc_core::{
    AudioSource, Conversation, ConversationId, ConversationStatus, EngineInfo, GroupId, SourceKind,
    Tag, TagId, UnixMillis,
};
use rusqlite::{params, Row};

use crate::error::{Result, StoreError};
use crate::model::{ConversationFilter, ConversationSummary, NewConversation};
use crate::Store;

impl Store {
    pub fn create_conversation(&self, new: &NewConversation) -> Result<ConversationId> {
        self.conn.execute(
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
        Ok(ConversationId(self.conn.last_insert_rowid()))
    }

    pub fn get_conversation(&self, id: ConversationId) -> Result<Conversation> {
        self.conn
            .query_row(
                "SELECT id, title, group_id, started_at, ended_at, status,
                        source_kind, source_name, source_desc, source_application, source_index,
                        mic_track, engine, model, language, backend, voxtype_meeting_id
                 FROM conversations WHERE id = ?1",
                params![id.get()],
                row_to_conversation,
            )
            .map_err(|e| not_found_or(e, || format!("conversation {id} not found")))
    }

    pub fn rename_conversation(&self, id: ConversationId, title: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![title, id.get()],
        )?;
        require_row(changed, || format!("conversation {id} not found"))
    }

    pub fn set_group(&self, id: ConversationId, group: Option<GroupId>) -> Result<()> {
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
        let changed = self.conn.execute(
            "UPDATE conversations SET ended_at = ?1, status = ?2 WHERE id = ?3",
            params![ended_at, status.as_str(), id.get()],
        )?;
        require_row(changed, || format!("conversation {id} not found"))
    }

    /// Any conversation still `Active` after an open was left that way by a
    /// process that never called `finish_conversation` — most likely a crash.
    /// Its committed segments are untouched; only the status changes.
    pub fn reap_active(&self) -> Result<usize> {
        let changed = self.conn.execute(
            "UPDATE conversations SET status = ?1 WHERE status = ?2",
            params![
                ConversationStatus::Interrupted.as_str(),
                ConversationStatus::Active.as_str(),
            ],
        )?;
        if changed > 0 {
            tracing::warn!(count = changed, "reaped conversations left active by a dead process");
        }
        Ok(changed)
    }

    /// Cascades to `segments` and `conversation_tags` via `ON DELETE CASCADE`.
    pub fn delete_conversation(&self, id: ConversationId) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM conversations WHERE id = ?1", params![id.get()])?;
        require_row(changed, || format!("conversation {id} not found"))
    }

    /// Filtered, paginated list of conversations for the sidebar. Tags for the
    /// whole page are fetched in one follow-up query, not one per row.
    pub fn list_conversations(&self, filter: &ConversationFilter) -> Result<Vec<ConversationSummary>> {
        let limit: i64 = filter.limit.map(i64::from).unwrap_or(-1);
        let offset: i64 = filter.offset.map(i64::from).unwrap_or(0);

        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.title, c.started_at, c.ended_at,
                    c.source_kind, c.source_name, c.source_desc, c.source_application,
                    g.name AS group_name,
                    (SELECT COUNT(*) FROM segments s WHERE s.conversation_id = c.id) AS segment_count
             FROM conversations c
             LEFT JOIN groups g ON g.id = c.group_id
             WHERE (?1 IS NULL OR c.group_id = ?1)
               AND (?2 IS NULL OR c.title LIKE '%' || ?2 || '%')
               AND (
                    ?3 IS NULL OR EXISTS (
                        SELECT 1 FROM conversation_tags ct
                        WHERE ct.conversation_id = c.id AND ct.tag_id = ?3
                    )
               )
             ORDER BY c.started_at DESC
             LIMIT ?4 OFFSET ?5",
        )?;

        let rows = stmt.query_map(
            params![
                filter.group.map(GroupId::get),
                filter.query,
                filter.tag.map(TagId::get),
                limit,
                offset,
            ],
            |row| {
                let id: i64 = row.get(0)?;
                let started_at: UnixMillis = row.get(2)?;
                let ended_at: Option<UnixMillis> = row.get(3)?;
                let kind_str: String = row.get(4)?;
                let source = AudioSource {
                    kind: SourceKind::parse(&kind_str).unwrap_or(SourceKind::Device),
                    name: row.get(5)?,
                    description: row.get(6)?,
                    application: row.get(7)?,
                    index: None,
                };
                Ok((
                    ConversationId(id),
                    ConversationSummary {
                        id: ConversationId(id),
                        title: row.get(1)?,
                        started_at,
                        duration_ms: ended_at.map(|e| e.saturating_sub(started_at) as u64),
                        segment_count: row.get::<_, i64>(9)? as u64,
                        source_label: source.label(),
                        group_name: row.get(8)?,
                        tags: Vec::new(),
                    },
                ))
            },
        )?;

        let mut summaries: Vec<(ConversationId, ConversationSummary)> =
            rows.collect::<rusqlite::Result<Vec<_>>>()?;

        if !summaries.is_empty() {
            let ids: Vec<ConversationId> = summaries.iter().map(|(id, _)| *id).collect();
            let tags_by_conversation = self.tags_for_conversations(&ids)?;
            for (id, summary) in &mut summaries {
                if let Some(tags) = tags_by_conversation.get(id) {
                    summary.tags = tags.clone();
                }
            }
        }

        Ok(summaries.into_iter().map(|(_, s)| s).collect())
    }

    pub(crate) fn tags_for_conversations(
        &self,
        ids: &[ConversationId],
    ) -> Result<std::collections::HashMap<ConversationId, Vec<Tag>>> {
        use std::collections::HashMap;

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
            index: index.map(|i| i as u32),
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
