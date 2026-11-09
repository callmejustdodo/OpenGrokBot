use crate::ports::Migration;

/// A's schema. Pass this slice alongside B's migrations when opening the store.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1000,
        name: "agent runtime storage",
        sql: include_str!("schema.sql"),
    },
    Migration {
        version: 1001,
        name: "agent running requests",
        sql: "ALTER TABLE agents ADD COLUMN running_requests TEXT NOT NULL DEFAULT '[]';",
    },
];
