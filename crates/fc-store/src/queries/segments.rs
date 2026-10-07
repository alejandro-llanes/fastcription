//! The hot path: appending committed segments, and reading them back in
//! display order.

use fc_core::{ConversationId, Segment, Track};
use rusqlite::{params, OptionalExtension};

use crate::error::{Result, StoreError};
use crate::Store;

impl Store {
    /// Inserts `segments` for `conversation` in one transaction. Called every
    /// few seconds while a conversation is recording, so this has to stay
    /// cheap: one prepared statement, no round trip per row beyond the
    /// execute itself.
    ///
    /// A provisional segment reaching here is a caller bug (architecture §3:
    /// provisional segments are UI-only and never meant to be persisted), so
    /// this rejects the whole batch rather than silently dropping one row.
    pub fn append_segments(&self, conversation: ConversationId, segments: &[Segment]) -> Result<()> {
        if segments.is_empty() {
            return Ok(());
        }
        if let Some(bad) = segments.iter().find(|s| s.provisional) {
            return Err(StoreError::ProvisionalSegment {
                track: bad.track,
                seq: bad.seq,
            });
        }

        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM conversations WHERE id = ?1",
                params![conversation.get()],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound(format!(
                "conversation {conversation} not found"
            )));
        }

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
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
                    s.text,
                    s.translation,
                    s.speaker,
                    s.confidence,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Ordered by `(start_ms, seq)`: the natural reading order even when a
    /// microphone track interleaves with the selected source.
    pub fn load_segments(&self, conversation: ConversationId) -> Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, track, start_ms, end_ms, text, translation, speaker, confidence
             FROM segments WHERE conversation_id = ?1
             ORDER BY start_ms, seq",
        )?;
        let rows = stmt.query_map(params![conversation.get()], |row| {
            let track_str: String = row.get(1)?;
            let track = Track::parse(&track_str).unwrap_or_else(|| {
                tracing::error!(value = %track_str, "unrecognised track, defaulting to Selected");
                Track::Selected
            });
            Ok(Segment {
                track,
                seq: row.get::<_, i64>(0)? as u64,
                start_ms: row.get::<_, i64>(2)? as u64,
                end_ms: row.get::<_, i64>(3)? as u64,
                text: row.get(4)?,
                translation: row.get(5)?,
                speaker: row.get(6)?,
                confidence: row.get(7)?,
                provisional: false,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
