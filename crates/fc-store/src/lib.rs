//! Persistence for fastcription: a SQLite-backed `Store` over the domain
//! types in `fc-core`.
//!
//! Everything else in the workspace that needs a conversation, a segment, a
//! group or a tag to survive a restart goes through [`Store`]; nothing else
//! in the app opens the database file directly.

mod error;
mod model;
mod schema;

mod queries {
    pub mod conversations;
    pub mod groups;
    pub mod search;
    pub mod segments;
    pub mod tags;
}

use std::path::Path;

use rusqlite::Connection;

pub use error::{Result, StoreError};
pub use model::{ConversationFilter, ConversationSummary, NewConversation, SearchHit};

/// Handle to the fastcription library database: one rusqlite `Connection`
/// plus the pragmas and migrations needed to make it safe to use.
///
/// Architecture §3 puts this behind a single dedicated store thread, so
/// `Store` makes no attempt to be `Sync` — cross-thread access is meant to go
/// through a channel to that thread, not through a mutex around the
/// connection here.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if absent) the database at `path`: parent directories
    /// are created, pragmas are set, and any pending migration runs before
    /// this returns.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        configure(&conn)?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Opens the default library location, `dirs::data_dir()/fastcription/library.db`.
    pub fn open_default() -> Result<Self> {
        let base = dirs::data_dir().ok_or(StoreError::NoDataDir)?;
        Self::open(base.join("fastcription").join("library.db"))
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

#[cfg(test)]
mod tests;
