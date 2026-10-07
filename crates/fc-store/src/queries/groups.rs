//! Groups: a flat, user-named bucket for conversations (architecture §6 —
//! nesting is deliberately out of scope; `group_id` is nullable so it can
//! arrive later without a migration).

use fc_core::{Group, GroupId, UnixMillis};
use rusqlite::params;

use crate::error::Result;
use crate::queries::conversations::not_found_or;
use crate::Store;

impl Store {
    pub fn create_group(&self, name: &str, created_at: UnixMillis) -> Result<GroupId> {
        self.conn.execute(
            "INSERT INTO groups (name, created_at) VALUES (?1, ?2)",
            params![name, created_at],
        )?;
        Ok(GroupId(self.conn.last_insert_rowid()))
    }

    pub fn rename_group(&self, id: GroupId, new_name: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE groups SET name = ?1 WHERE id = ?2",
            params![new_name, id.get()],
        )?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!(
                "group {id} not found"
            )));
        }
        Ok(())
    }

    /// `ON DELETE SET NULL` on `conversations.group_id` is what keeps the
    /// group's conversations alive, ungrouped, rather than cascading.
    pub fn delete_group(&self, id: GroupId) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM groups WHERE id = ?1", params![id.get()])?;
        if changed == 0 {
            return Err(crate::error::StoreError::NotFound(format!(
                "group {id} not found"
            )));
        }
        Ok(())
    }

    pub fn get_group(&self, id: GroupId) -> Result<Group> {
        self.conn
            .query_row(
                "SELECT id, name, created_at FROM groups WHERE id = ?1",
                params![id.get()],
                row_to_group,
            )
            .map_err(|e| not_found_or(e, || format!("group {id} not found")))
    }

    pub fn list_groups(&self) -> Result<Vec<Group>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM groups ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], row_to_group)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn row_to_group(row: &rusqlite::Row) -> rusqlite::Result<Group> {
    Ok(Group {
        id: GroupId(row.get(0)?),
        name: row.get(1)?,
        created_at: row.get(2)?,
    })
}
