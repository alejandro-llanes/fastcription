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

use rusqlite::{Connection, OpenFlags};

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
    read_only: bool,
}

impl Store {
    /// Opens (creating if absent) the database at `path`: parent directories
    /// are created, pragmas are set, and any pending migration runs before
    /// this returns.
    ///
    /// Falls back to a read-only connection when the file or its directory
    /// will not accept writes. A library on a read-only mount, or one whose
    /// permissions were tightened, still holds every past transcript, and
    /// telling the user it "could not be opened" throws their archive away to
    /// report a problem that only affects recording. [`Store::is_read_only`]
    /// is how the UI tells the two apart.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                // Only fatal when there is nothing there to read: a directory
                // we cannot create and a file that does not exist means no
                // library at all.
                if !path.exists() {
                    return Err(err.into());
                }
                tracing::warn!(%err, "library directory is not writable");
            }
        }

        match Self::open_writable(path) {
            Ok(store) => Ok(store),
            Err(err) if err.is_write_denied() => {
                tracing::warn!(
                    %err,
                    path = %path.display(),
                    "library will not accept writes, opening it read-only"
                );
                Self::open_read_only(path)
            }
            Err(err) => Err(err),
        }
    }

    /// Opens the default library location, `dirs::data_dir()/fastcription/library.db`.
    pub fn open_default() -> Result<Self> {
        let base = dirs::data_dir().ok_or(StoreError::NoDataDir)?;
        Self::open(base.join("fastcription").join("library.db"))
    }

    /// True when this handle cannot write. Every write method returns
    /// [`StoreError::ReadOnly`] in that state, so a caller may either check
    /// first (to grey out a control) or just try.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn open_writable(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        configure(&conn)?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn,
            read_only: false,
        })
    }

    /// No pragmas and no migration: both are writes, and the point of this
    /// path is that writes are refused.
    ///
    /// It takes two attempts. SQLite needs a `-shm` wal-index file beside the
    /// database to read a WAL database at all, and fastcription's libraries
    /// are WAL (see `configure`), so a read-only *directory* defeats an
    /// ordinary read-only open. `immutable=1` is SQLite's documented escape
    /// from that: it promises the file will not change while open, which is
    /// precisely what read-only storage guarantees.
    fn open_read_only(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;
        let plain = Connection::open_with_flags(path, flags)
            .map_err(StoreError::from)
            .and_then(|conn| schema::verify(&conn).map(|()| conn));

        let conn = match plain {
            Ok(conn) => conn,
            Err(err) if err.is_write_denied() => {
                tracing::debug!(%err, "retrying the read-only open as immutable");
                let uri = format!("file:{}?immutable=1", uri_path(path));
                let conn = Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI)?;
                schema::verify(&conn)?;
                conn
            }
            Err(err) => return Err(err),
        };

        Ok(Self {
            conn,
            read_only: true,
        })
    }

    /// The gate every write method passes through. `operation` is what the
    /// user tried to do, so the message says which action was refused rather
    /// than only that something was.
    pub(crate) fn writable(&self, operation: &'static str) -> Result<()> {
        if self.read_only {
            Err(StoreError::ReadOnly { operation })
        } else {
            Ok(())
        }
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

/// Percent-encodes the three characters SQLite's URI filename parser reads as
/// syntax, so a library under a directory with a `?`, `#` or `%` in its name
/// still opens through the `immutable=1` path.
fn uri_path(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        match c {
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests;
