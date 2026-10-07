//! The hot path: appending committed segments, and reading them back in
//! display order.

use fc_core::{ConversationId, Segment, Track};
use rusqlite::params;

use crate::error::{Result, StoreError};
use crate::Store;

/// A stored column is declared `INTEGER NOT NULL` but the domain type is
/// `u64`; nothing in this crate ever writes a negative value, but a
/// hand-edited or corrupted database file could contain one. `as u64` on a
/// negative `i64` would wrap to a value near `u64::MAX` and silently hand a
/// nonsensical timestamp/sequence number downstream (export, ordering, the
/// UI). Treat it as the corruption it is instead: clamp to 0 and say so in
/// the log, the same defensive stance already taken for an unrecognised
/// `track`/`status`/`kind` string elsewhere in this crate.
pub(crate) fn nonneg_u64(value: i64, field: &'static str) -> u64 {
    if value < 0 {
        tracing::error!(
            value,
            field,
            "negative value in database column, treating as 0"
        );
        0
    } else {
        value as u64
    }
}

impl Store {
    /// Inserts `segments` for `conversation` in one transaction. Called every
    /// few seconds while a conversation is recording, so this has to stay
    /// cheap: the insert uses `prepare_cached`, so the statement is compiled
    /// once per connection and reused on every later call rather than
    /// re-prepared each time, and existence of `conversation` is established
    /// by the `FOREIGN KEY` constraint on `segments.conversation_id` itself
    /// (checked anyway, with `foreign_keys = ON` set in `configure()`) rather
    /// than by a separate `SELECT` that would just be a second round trip to
    /// ask the database something the insert already has to verify.
    ///
    /// A provisional segment reaching here is a caller bug (architecture §3:
    /// provisional segments are UI-only and never meant to be persisted), so
    /// this rejects the whole batch rather than silently dropping one row.
    pub fn append_segments(
        &self,
        conversation: ConversationId,
        segments: &[Segment],
    ) -> Result<()> {
        if segments.is_empty() {
            return Ok(());
        }
        if let Some(bad) = segments.iter().find(|s| s.provisional) {
            return Err(StoreError::ProvisionalSegment {
                track: bad.track,
                seq: bad.seq,
            });
        }

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
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
                ])
                .map_err(|e| foreign_key_violation_as_not_found(e, conversation))?;
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
                seq: nonneg_u64(row.get(0)?, "segments.seq"),
                start_ms: nonneg_u64(row.get(2)?, "segments.start_ms"),
                end_ms: nonneg_u64(row.get(3)?, "segments.end_ms"),
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

/// Translates the `FOREIGN KEY` violation that `conversation_id` not existing
/// produces into the same [`StoreError::NotFound`] the old explicit
/// existence check used to return, so callers see no difference. Any other
/// error (including a different constraint, e.g. the `idx_segments_identity`
/// duplicate-append guard) passes through unchanged.
fn foreign_key_violation_as_not_found(
    err: rusqlite::Error,
    conversation: ConversationId,
) -> StoreError {
    if let rusqlite::Error::SqliteFailure(ref sqlite_err, _) = err {
        if sqlite_err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY {
            return StoreError::NotFound(format!("conversation {conversation} not found"));
        }
    }
    StoreError::Sqlite(err)
}
