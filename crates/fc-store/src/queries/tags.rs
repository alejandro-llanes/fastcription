//! Tags: free-form labels, unique by name regardless of case (`COLLATE
//! NOCASE` on the column does the comparison work; the queries here only need
//! to stay consistent with it).

use fc_core::{ConversationId, Tag, TagId};
use rusqlite::params;

use crate::error::Result;
use crate::Store;

impl Store {
    /// Creates the tag if no tag with this name (case-insensitively) exists
    /// yet, otherwise returns the existing one. Safe to call every time a user
    /// types a tag name into a picker.
    pub fn create_tag(&self, name: &str, color: Option<&str>) -> Result<TagId> {
        self.conn.execute(
            "INSERT INTO tags (name, color) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
            params![name, color],
        )?;
        self.conn
            .query_row("SELECT id FROM tags WHERE name = ?1", params![name], |row| {
                row.get(0)
            })
            .map(TagId)
            .map_err(Into::into)
    }

    pub fn rename_tag(&self, id: TagId, new_name: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE tags SET name = ?1 WHERE id = ?2",
            params![new_name, id.get()],
        )?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!("tag {id} not found")));
        }
        Ok(())
    }

    /// Cascades to `conversation_tags` via `ON DELETE CASCADE`; conversations
    /// themselves are untouched.
    pub fn delete_tag(&self, id: TagId) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM tags WHERE id = ?1", params![id.get()])?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!("tag {id} not found")));
        }
        Ok(())
    }

    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, color FROM tags ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |row| {
            Ok(Tag {
                id: TagId(row.get(0)?),
                name: row.get(1)?,
                color: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Idempotent: tagging an already-tagged conversation is a no-op, not an
    /// error.
    pub fn add_tag(&self, conversation: ConversationId, tag: TagId) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO conversation_tags (conversation_id, tag_id) VALUES (?1, ?2)",
            params![conversation.get(), tag.get()],
        )?;
        Ok(())
    }

    /// Idempotent: removing a tag that was not applied is a no-op, not an
    /// error.
    pub fn remove_tag(&self, conversation: ConversationId, tag: TagId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM conversation_tags WHERE conversation_id = ?1 AND tag_id = ?2",
            params![conversation.get(), tag.get()],
        )?;
        Ok(())
    }
}
