//! The word registry: expressions a reader did not know, and what they mean.

use fc_core::{Word, WordId};
use rusqlite::params;

use crate::error::Result;
use crate::model::NewWord;
use crate::Store;

impl Store {
    /// Adds an expression, or returns the existing entry when the same one is
    /// already there under any capitalisation.
    ///
    /// Returning the existing id rather than failing is what lets "add to my
    /// words" be pressed without first checking: the second press from
    /// another meeting lands on the one entry, which keeps the lookup that
    /// was already done.
    pub fn add_word(&self, new: &NewWord) -> Result<WordId> {
        self.writable("adding a word")?;
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO words (expression, context, conversation_id, start_ms, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                new.expression.trim(),
                new.context,
                new.conversation.map(|c| c.get()),
                // rusqlite has no u64; an offset into a conversation fits an
                // i64 for the next few hundred million years.
                new.start_ms.map(|ms| ms as i64),
                new.created_at
            ],
        )?;
        if inserted == 1 {
            return Ok(WordId(self.conn.last_insert_rowid()));
        }
        let id: i64 = self.conn.query_row(
            "SELECT id FROM words WHERE expression = ?1 COLLATE NOCASE",
            params![new.expression.trim()],
            |row| row.get(0),
        )?;
        Ok(WordId(id))
    }

    /// Newest first: the word from the meeting that just ended is the one the
    /// reader opened the registry for.
    pub fn list_words(&self) -> Result<Vec<Word>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, expression, context, conversation_id, start_ms, meaning, translation, \
             example, created_at FROM words ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], row_to_word)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Records what the meaning server answered.
    pub fn set_word_lookup(
        &self,
        id: WordId,
        meaning: Option<&str>,
        translation: Option<&str>,
        example: Option<&str>,
    ) -> Result<()> {
        self.writable("recording a lookup")?;
        let changed = self.conn.execute(
            "UPDATE words SET meaning = ?1, translation = ?2, example = ?3 WHERE id = ?4",
            params![meaning, translation, example, id.get()],
        )?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!(
                "word {id} not found"
            )));
        }
        Ok(())
    }

    /// Everything the reader can edit: the expression and the three answers.
    pub fn update_word(&self, word: &Word) -> Result<()> {
        self.writable("editing a word")?;
        let changed = self.conn.execute(
            "UPDATE words SET expression = ?1, meaning = ?2, translation = ?3, example = ?4 \
             WHERE id = ?5",
            params![
                word.expression.trim(),
                word.meaning,
                word.translation,
                word.example,
                word.id.get()
            ],
        )?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!(
                "word {} not found",
                word.id
            )));
        }
        Ok(())
    }

    pub fn delete_word(&self, id: WordId) -> Result<()> {
        self.writable("deleting a word")?;
        let changed = self
            .conn
            .execute("DELETE FROM words WHERE id = ?1", params![id.get()])?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!(
                "word {id} not found"
            )));
        }
        Ok(())
    }
}

fn row_to_word(row: &rusqlite::Row) -> rusqlite::Result<Word> {
    Ok(Word {
        id: WordId(row.get(0)?),
        expression: row.get(1)?,
        context: row.get(2)?,
        conversation: row.get::<_, Option<i64>>(3)?.map(fc_core::ConversationId),
        start_ms: row.get::<_, Option<i64>>(4)?.map(|ms| ms.max(0) as u64),
        meaning: row.get(5)?,
        translation: row.get(6)?,
        example: row.get(7)?,
        created_at: row.get(8)?,
    })
}
