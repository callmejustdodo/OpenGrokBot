//! Synchronous SQLite storage. Call from blocking-safe code.
//!
//! Open with `SqliteStore::open(path, &[MIGRATIONS, b_migrations])`. B can hold an
//! `Arc<SqliteStore>` in its adapter and use `with_connection` for its repositories
//! and transactions without changing A's schema or implementing B's port here.
//!
//! The schema lives in `schema.sql` plus later steps in `migrations.rs`. JSON
//! columns are agents.repositories, agents.detected, agents.running_requests,
//! schedules.rule, schedules.last_run, attention.target,
//! activity.target, and settings.notifications. Other proto fields use scalar
//! columns, with avatar split into shape/color. UTC timestamp columns have fixed
//! nanosecond precision so SQL ordering and exclusive paging compare instants.
//! Harness transcripts remain external; only their paths are stored. Fork sessions
//! and schedule runs survive deletion of their parent runtime records for recovery.
//! In-memory SQLite uses its memory journal (SQLite cannot enable WAL there).

use std::{path::Path, sync::Mutex};

use formation_proto::common::{ApiError, ErrorCode};
use rusqlite::{Connection, OptionalExtension};

use crate::ports::{MIGRATIONS_A, MIGRATIONS_B, Migration, Result};

mod migrations;
mod repositories;
pub use migrations::MIGRATIONS;

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl SqliteStore {
    /// Creates or opens the caller's file and applies all supplied role slices.
    pub fn open(path: impl AsRef<Path>, migrations: &[&[Migration]]) -> Result<Self> {
        Self::from_connection(Connection::open(path).map_err(db_error)?, migrations)
    }

    pub fn in_memory(migrations: &[&[Migration]]) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(db_error)?, migrations)
    }

    fn from_connection(connection: Connection, migrations: &[&[Migration]]) -> Result<Self> {
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(db_error)?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.migrate(migrations)?;
        Ok(store)
    }

    /// Runs a closure with exclusive connection access. The closure may start a
    /// transaction; it must not call back into this store (the lock is not reentrant).
    pub fn with_connection<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "SQLite connection lock poisoned"))?;
        f(&mut connection)
    }

    /// Validates the complete input before any schema changes, then commits each
    /// pending version separately. Returns the number applied by this call.
    pub fn migrate(&self, roles: &[&[Migration]]) -> Result<usize> {
        let mut migrations: Vec<_> = roles.iter().flat_map(|role| role.iter()).collect();
        migrations.sort_by_key(|m| m.version);
        if migrations
            .iter()
            .any(|m| !MIGRATIONS_A.contains(&m.version) && !MIGRATIONS_B.contains(&m.version))
            || migrations
                .windows(2)
                .any(|pair| pair[0].version == pair[1].version)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidParams,
                "duplicate or out-of-range migration version",
            ));
        }
        self.with_connection(|connection| {
            connection.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL);").map_err(db_error)?;
            let mut applied = 0;
            for migration in migrations {
                let transaction = connection.transaction().map_err(db_error)?;
                let exists = transaction.query_row("SELECT version FROM schema_migrations WHERE version=?1", [migration.version], |r| r.get::<_, u32>(0)).optional().map_err(db_error)?;
                if exists.is_none() {
                    transaction.execute_batch(migration.sql).map_err(db_error)?;
                    transaction.execute("INSERT INTO schema_migrations VALUES (?1, ?2)", (migration.version, migration.name)).map_err(db_error)?;
                    applied += 1;
                }
                transaction.commit().map_err(db_error)?;
            }
            Ok(applied)
        })
    }
}

fn db_error(error: rusqlite::Error) -> ApiError {
    let code = match &error {
        rusqlite::Error::SqliteFailure(e, _)
            if matches!(
                e.extended_code,
                rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                    | rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            ) =>
        {
            ErrorCode::Conflict
        }
        _ => ErrorCode::Internal,
    };
    ApiError::new(code, error.to_string())
}

fn json_error(error: serde_json::Error) -> ApiError {
    ApiError::new(ErrorCode::Internal, error.to_string())
}

#[cfg(test)]
mod tests;
