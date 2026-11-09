//! The asker's outbox on SQLite (`migrations/2002_outbox.sql`), so calls waiting
//! for a teammate's Mac survive a restart and are retried when formationd starts
//! again (backend architecture, "Recovery"). Same behavior as
//! [`MemoryOutboxStore`](crate::peer::outbox::MemoryOutboxStore).

use rusqlite::params;

use crate::peer::outbox::{OutboxEntry, OutboxKey, OutboxStore};
use crate::ports::Result;

use super::{SqliteCollabStore, db_error, internal};

/// The key columns for `key`: its kind, as serde names it, and its ID.
fn key_columns(key: OutboxKey) -> (&'static str, String) {
    match key {
        OutboxKey::Question(id) => ("question", id.to_string()),
        OutboxKey::Request(id) => ("request", id.to_string()),
    }
}

impl OutboxStore for SqliteCollabStore {
    /// Oldest first: in the order entries were first put, which is the order
    /// each teammate's calls go out in.
    fn load(&self) -> Result<Vec<OutboxEntry>> {
        self.with(|c| {
            let mut statement = c
                .prepare("SELECT entry FROM outbox_entry ORDER BY rowid")
                .map_err(db_error)?;
            let rows = statement
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(db_error)?;
            rows.map(|row| {
                let text = row.map_err(db_error)?;
                serde_json::from_str(&text)
                    .map_err(|e| internal(format!("stored outbox entry: {e}")))
            })
            .collect()
        })
    }

    /// An upsert rather than `INSERT OR REPLACE`: replacing deletes the row and
    /// gives it a new rowid, which would move a retried entry behind later ones.
    fn put(&self, entry: &OutboxEntry) -> Result<()> {
        let (kind, id) = key_columns(entry.key());
        let json = serde_json::to_string(entry).map_err(internal)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO outbox_entry (key_kind, key_id, member, entry)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(key_kind, key_id) DO UPDATE SET
                     member=excluded.member, entry=excluded.entry",
                params![kind, id, entry.member.to_string(), json],
            )
            .map_err(db_error)?;
            Ok(())
        })
    }

    /// A key that is not there is not an error, as in `MemoryOutboxStore`: an
    /// entry may be removed by a cancel and by its delivery at nearly once.
    fn remove(&self, key: OutboxKey) -> Result<()> {
        let (kind, id) = key_columns(key);
        self.with(|c| {
            c.execute(
                "DELETE FROM outbox_entry WHERE key_kind=?1 AND key_id=?2",
                params![kind, id],
            )
            .map_err(db_error)?;
            Ok(())
        })
    }
}
