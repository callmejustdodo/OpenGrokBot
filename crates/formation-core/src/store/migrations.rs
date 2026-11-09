use crate::ports::Migration;

/// A's schema. Pass this slice alongside B's migrations when opening the store.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1000,
        name: "agent runtime storage",
        sql: include_str!("schema.sql"),
    },
    Migration {
        version: 1003,
        name: "schedule run error",
        sql: "ALTER TABLE schedule_runs ADD COLUMN error TEXT;",
    },
];
