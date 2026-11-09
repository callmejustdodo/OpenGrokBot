//! Owner: B. B's SQLite repositories: the [`CollabStore`](crate::ports::CollabStore)
//! port over A's [`SqliteStore`] (TASK-017, TASK-019, TASK-023, TASK-025).
//!
//! Open one store with both roles' migrations and share it:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use formation_core::{collab_store::{COLLAB_MIGRATIONS, SqliteCollabStore}, store::{MIGRATIONS, SqliteStore}};
//! let store = Arc::new(SqliteStore::open("formation.db", &[MIGRATIONS, COLLAB_MIGRATIONS])?);
//! let collab = SqliteCollabStore::new(store.clone()); // `store` is A's `Store`
//! # Ok::<(), formation_proto::common::ApiError>(())
//! ```
//!
//! [`SqliteCollabStore`] is a wrapper rather than more impls on `SqliteStore`
//! because A's `SessionRepo` and B's `QuestionRepo` both name `fork_session` and
//! `put_fork_session`; on one type those calls would be ambiguous wherever both
//! traits are in scope.
//!
//! The behavior matches the in-memory `MemoryCollabStore` (testkit): sharing is
//! private until stored; `members` and `member_by_tailscale` skip removed members;
//! `put_member` clears a removal; `insert_request` is idempotent on (requester,
//! idempotency key) in one transaction; uniqueness clashes give `conflict`, and
//! removing or counting something absent gives `not_found`. Lists come back oldest
//! first by their time column, then in insertion order.
//!
//! Top-level scalar fields are SQL columns; nested values are JSON text (see
//! `migrations/2001_collab.sql`). Timestamps have fixed nanosecond precision, as in
//! A's store, so text order is time order.
//!
//! It is also the persistent [`OutboxStore`](crate::peer::outbox::OutboxStore)
//! (`outbox.rs`), which follows `MemoryOutboxStore` instead: entries load in the
//! order they were first put, and removing an absent entry is not an error.

use std::sync::Arc;

use formation_proto::ask::Question;
use formation_proto::common::{
    AgentId, ApiError, ApprovalId, ErrorCode, MemberId, MessageId, QuestionId, RequestId, RuleId,
    SessionId, Timestamp,
};
use formation_proto::policy::{Approval, ApprovalStatus, Rule, ShareSettings};
use formation_proto::request::{QueueItem, WorkRequest};
use formation_proto::team::{Invitation, Member};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Params, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::ports::{
    ApprovalRepo, Migration, QuestionRepo, RequestRepo, Result, RuleRepo, ShareRepo, TeamRecord,
    TeamRepo,
};
use crate::store::SqliteStore;

mod outbox;
#[cfg(test)]
mod tests;

/// B's schema: the team tables (2000); sharing, questions, requests, queues,
/// approvals, and rules (2001); and the asker's outbox (2002, for
/// [`OutboxStore`](crate::peer::outbox::OutboxStore)). Pass it beside A's
/// `store::MIGRATIONS`.
pub const COLLAB_MIGRATIONS: &[Migration] = &[
    crate::team::MIGRATIONS[0],
    Migration {
        version: 2001,
        name: "collaboration",
        sql: include_str!("migrations/2001_collab.sql"),
    },
    Migration {
        version: 2002,
        name: "outbox",
        sql: include_str!("migrations/2002_outbox.sql"),
    },
];

/// B's repositories on a shared [`SqliteStore`] opened with [`COLLAB_MIGRATIONS`].
#[derive(Clone)]
pub struct SqliteCollabStore {
    store: Arc<SqliteStore>,
}

impl SqliteCollabStore {
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    /// The shared store, for A's repositories.
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }

    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.store.with_connection(f)
    }
}

/// How a table maps to its record's serde fields.
struct Table {
    name: &'static str,
    key: &'static str,
    /// Columns holding JSON text.
    json: &'static [&'static str],
    /// INTEGER 0/1 columns holding a `bool`.
    bools: &'static [&'static str],
    /// Columns that are not record fields, left out when reading.
    hidden: &'static [&'static str],
}

const MEMBER: Table = Table {
    name: "member",
    key: "id",
    json: &[],
    bools: &["online"],
    hidden: &["removed_at"],
};
const INVITATION: Table = Table {
    name: "invitation",
    key: "id",
    json: &[],
    bools: &[],
    hidden: &[],
};
const SHARE: Table = Table {
    name: "agent_share",
    key: "agent_id",
    json: &[],
    bools: &["shared", "accepts_requests"],
    hidden: &[],
};
const QUESTION: Table = Table {
    name: "question",
    key: "id",
    json: &["target", "candidates", "answer", "error"],
    bools: &[],
    hidden: &[],
};
const REQUEST: Table = Table {
    name: "work_request",
    key: "id",
    json: &["chain", "error"],
    bools: &[],
    hidden: &[],
};
const QUEUE_ITEM: Table = Table {
    name: "queue_item",
    key: "id",
    json: &["source"],
    bools: &[],
    hidden: &[],
};
const APPROVAL: Table = Table {
    name: "approval",
    key: "id",
    json: &["call", "proposed_rule"],
    bools: &[],
    hidden: &[],
};
const RULE: Table = Table {
    name: "rule",
    key: "id",
    json: &["scope"],
    bools: &[],
    hidden: &[],
};

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

fn internal(message: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, message.to_string())
}

fn timestamp(at: Timestamp) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()
}

/// One column value from a serde field.
fn sql_value(table: &Table, column: &str, value: &Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        _ if table.json.contains(&column) => {
            SqlValue::Text(serde_json::to_string(value).map_err(internal)?)
        }
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => SqlValue::Integer(
            n.as_i64()
                .ok_or_else(|| internal(format!("{column} is out of range")))?,
        ),
        Value::String(s) if column.ends_with("_at") => SqlValue::Text(
            s.parse::<Timestamp>()
                .map(timestamp)
                .map_err(|e| internal(format!("{column}: {e}")))?,
        ),
        Value::String(s) => SqlValue::Text(s.clone()),
        _ => return Err(internal(format!("{column} is not a scalar column"))),
    })
}

/// Inserts or replaces (by the table's key) a record, plus any extra columns.
fn upsert(
    connection: &Connection,
    table: &Table,
    record: &impl Serialize,
    extra: &[(&str, SqlValue)],
) -> Result<()> {
    let Value::Object(fields) = serde_json::to_value(record).map_err(internal)? else {
        return Err(internal("records are objects"));
    };
    let mut names = Vec::new();
    let mut values = Vec::new();
    for (column, value) in &fields {
        names.push(column.as_str());
        values.push(sql_value(table, column, value)?);
    }
    for (column, value) in extra {
        names.push(column);
        values.push(value.clone());
    }
    let marks = vec!["?"; names.len()].join(",");
    let updates = names
        .iter()
        .filter(|name| **name != table.key)
        .map(|name| format!("{name}=excluded.{name}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({marks}) ON CONFLICT({}) DO UPDATE SET {updates}",
        table.name,
        names.join(","),
        table.key
    );
    connection
        .execute(&sql, rusqlite::params_from_iter(values))
        .map_err(db_error)?;
    Ok(())
}

/// Reads records: `SELECT * FROM <table> <rest>`.
fn select<T: DeserializeOwned>(
    connection: &Connection,
    table: &Table,
    rest: &str,
    params: impl Params,
) -> Result<Vec<T>> {
    let sql = format!("SELECT * FROM {} {rest}", table.name);
    let mut statement = connection.prepare(&sql).map_err(db_error)?;
    let columns: Vec<String> = statement.column_names().map(String::from).collect();
    let mut rows = statement.query(params).map_err(db_error)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(db_error)? {
        let mut object = Map::new();
        for (i, column) in columns.iter().enumerate() {
            let column = column.as_str();
            if table.hidden.contains(&column) {
                continue;
            }
            let value = match row.get::<_, SqlValue>(i).map_err(db_error)? {
                SqlValue::Null => Value::Null,
                SqlValue::Integer(n) if table.bools.contains(&column) => Value::Bool(n != 0),
                SqlValue::Integer(n) => Value::from(n),
                SqlValue::Text(s) if table.json.contains(&column) => {
                    serde_json::from_str(&s).map_err(internal)?
                }
                SqlValue::Text(s) => Value::String(s),
                _ => return Err(internal(format!("unexpected SQLite type in {column}"))),
            };
            object.insert(column.to_owned(), value);
        }
        records.push(serde_json::from_value(Value::Object(object)).map_err(internal)?);
    }
    Ok(records)
}

fn select_one<T: DeserializeOwned>(
    connection: &Connection,
    table: &Table,
    rest: &str,
    params: impl Params,
) -> Result<Option<T>> {
    Ok(select(connection, table, rest, params)?.into_iter().next())
}

fn not_found(message: String) -> ApiError {
    ApiError::new(ErrorCode::NotFound, message)
}

/// A stored ID, read the way serde writes it.
fn parse<T: DeserializeOwned>(column: &str, text: &str) -> Result<T> {
    serde_json::from_value(Value::String(text.to_owned()))
        .map_err(|e| internal(format!("stored {column}: {e}")))
}

impl TeamRepo for SqliteCollabStore {
    fn me(&self) -> Result<Option<MemberId>> {
        self.with(|c| {
            let id: Option<String> = c
                .query_row("SELECT member_id FROM team_me WHERE singleton=1", [], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(db_error)?;
            id.map(|id| parse("team_me.member_id", &id)).transpose()
        })
    }

    fn set_me(&self, id: MemberId) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO team_me (singleton, member_id) VALUES (1, ?1)
                 ON CONFLICT(singleton) DO UPDATE SET member_id=excluded.member_id",
                [id.to_string()],
            )
            .map_err(db_error)?;
            Ok(())
        })
    }

    fn team(&self) -> Result<Option<TeamRecord>> {
        self.with(|c| {
            let row = c
                .query_row(
                    "SELECT id, creator_host, creator_port, roster_version, open_invitation_code
                     FROM team WHERE singleton=1",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(db_error)?;
            row.map(|(id, creator_host, port, version, open_invitation)| {
                Ok(TeamRecord {
                    id: parse("team.id", &id)?,
                    creator_host,
                    creator_port: u16::try_from(port).map_err(internal)?,
                    roster_version: u64::try_from(version).map_err(internal)?,
                    open_invitation,
                })
            })
            .transpose()
        })
    }

    fn put_team(&self, team: Option<&TeamRecord>) -> Result<()> {
        self.with(|c| {
            match team {
                None => c.execute("DELETE FROM team", []),
                Some(team) => c.execute(
                    "INSERT INTO team (singleton, id, creator_host, creator_port, roster_version,
                         open_invitation_code)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(singleton) DO UPDATE SET id=excluded.id,
                         creator_host=excluded.creator_host, creator_port=excluded.creator_port,
                         roster_version=excluded.roster_version,
                         open_invitation_code=excluded.open_invitation_code",
                    params![
                        team.id.to_string(),
                        team.creator_host,
                        team.creator_port,
                        i64::try_from(team.roster_version).map_err(internal)?,
                        team.open_invitation,
                    ],
                ),
            }
            .map_err(db_error)?;
            Ok(())
        })
    }

    fn members(&self) -> Result<Vec<Member>> {
        self.with(|c| {
            select(
                c,
                &MEMBER,
                "WHERE removed_at IS NULL ORDER BY joined_at, rowid",
                [],
            )
        })
    }

    fn member(&self, id: MemberId) -> Result<Option<Member>> {
        self.with(|c| select_one(c, &MEMBER, "WHERE id=?1", [id.to_string()]))
    }

    fn member_by_tailscale(&self, login: &str) -> Result<Option<Member>> {
        self.with(|c| {
            select_one(
                c,
                &MEMBER,
                "WHERE tailscale_login=?1 AND removed_at IS NULL",
                [login],
            )
        })
    }

    /// `conflict` when another current member has the same GitHub login (ignoring
    /// case) or Tailscale login.
    fn put_member(&self, member: &Member) -> Result<()> {
        self.with(|c| upsert(c, &MEMBER, member, &[("removed_at", SqlValue::Null)]))
    }

    /// Keeps the first removal time when called again.
    fn remove_member(&self, id: MemberId, at: Timestamp) -> Result<()> {
        self.with(|c| {
            let changed = c
                .execute(
                    "UPDATE member SET removed_at=COALESCE(removed_at, ?1) WHERE id=?2",
                    [timestamp(at), id.to_string()],
                )
                .map_err(db_error)?;
            if changed == 0 {
                return Err(not_found(format!("no member {id}")));
            }
            Ok(())
        })
    }

    /// `conflict` when another invitation has the same code.
    fn put_invitation(&self, invitation: &Invitation) -> Result<()> {
        self.with(|c| upsert(c, &INVITATION, invitation, &[]))
    }

    fn invitation_by_code(&self, code: &str) -> Result<Option<Invitation>> {
        self.with(|c| select_one(c, &INVITATION, "WHERE code=?1", [code]))
    }
}

impl ShareRepo for SqliteCollabStore {
    fn share(&self, agent_id: AgentId) -> Result<ShareSettings> {
        self.with(|c| {
            Ok(
                select_one(c, &SHARE, "WHERE agent_id=?1", [agent_id.to_string()])?
                    .unwrap_or_else(|| ShareSettings::private(agent_id)),
            )
        })
    }

    fn shares(&self) -> Result<Vec<ShareSettings>> {
        self.with(|c| select(c, &SHARE, "ORDER BY rowid", []))
    }

    fn put_share(&self, share: &ShareSettings) -> Result<()> {
        self.with(|c| upsert(c, &SHARE, share, &[]))
    }
}

impl QuestionRepo for SqliteCollabStore {
    fn question(&self, id: QuestionId) -> Result<Option<Question>> {
        self.with(|c| select_one(c, &QUESTION, "WHERE id=?1", [id.to_string()]))
    }

    fn questions(&self) -> Result<Vec<Question>> {
        self.with(|c| select(c, &QUESTION, "ORDER BY created_at, rowid", []))
    }

    fn put_question(&self, question: &Question) -> Result<()> {
        self.with(|c| upsert(c, &QUESTION, question, &[]))
    }

    fn delete_question(&self, id: QuestionId) -> Result<()> {
        self.with(|c| {
            let deleted = c
                .execute("DELETE FROM question WHERE id=?1", [id.to_string()])
                .map_err(db_error)?;
            if deleted == 0 {
                return Err(not_found(format!("no question {id}")));
            }
            Ok(())
        })
    }

    fn fork_session(&self, thread_id: QuestionId) -> Result<Option<SessionId>> {
        self.with(|c| {
            c.query_row(
                "SELECT session_id FROM question_fork_session WHERE thread_id=?1",
                [thread_id.to_string()],
                |r| r.get(0).map(SessionId),
            )
            .optional()
            .map_err(db_error)
        })
    }

    fn put_fork_session(&self, thread_id: QuestionId, session_id: &SessionId) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO question_fork_session (thread_id, session_id) VALUES (?1, ?2)
                 ON CONFLICT(thread_id) DO UPDATE SET session_id=excluded.session_id",
                [thread_id.to_string(), session_id.0.clone()],
            )
            .map_err(db_error)?;
            Ok(())
        })
    }
}

impl RequestRepo for SqliteCollabStore {
    /// `conflict` when the ID exists under another idempotency key.
    fn insert_request(&self, request: &WorkRequest) -> Result<(WorkRequest, bool)> {
        self.with(|c| {
            let transaction = c.transaction().map_err(db_error)?;
            let stored = select_one(
                &transaction,
                &REQUEST,
                "WHERE requester=?1 AND idempotency_key=?2",
                [
                    request.requester.to_string(),
                    request.idempotency_key.clone(),
                ],
            )?;
            if let Some(stored) = stored {
                return Ok((stored, false));
            }
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM work_request WHERE id=?1",
                    [request.id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if exists.is_some() {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    format!("request {} exists with another idempotency key", request.id),
                ));
            }
            upsert(&transaction, &REQUEST, request, &[])?;
            transaction.commit().map_err(db_error)?;
            Ok((request.clone(), true))
        })
    }

    fn request(&self, id: RequestId) -> Result<Option<WorkRequest>> {
        self.with(|c| select_one(c, &REQUEST, "WHERE id=?1", [id.to_string()]))
    }

    fn requests(&self) -> Result<Vec<WorkRequest>> {
        self.with(|c| select(c, &REQUEST, "ORDER BY created_at, rowid", []))
    }

    /// `conflict` when the change would give two requests the same (requester,
    /// idempotency key).
    fn put_request(&self, request: &WorkRequest) -> Result<()> {
        self.with(|c| upsert(c, &REQUEST, request, &[]))
    }

    fn queue_items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>> {
        self.with(|c| {
            select(
                c,
                &QUEUE_ITEM,
                "WHERE agent_id=?1 ORDER BY queued_at, rowid",
                [agent_id.to_string()],
            )
        })
    }

    fn put_queue_item(&self, item: &QueueItem) -> Result<()> {
        self.with(|c| upsert(c, &QUEUE_ITEM, item, &[]))
    }

    fn remove_queue_item(&self, id: MessageId) -> Result<()> {
        self.with(|c| {
            let deleted = c
                .execute("DELETE FROM queue_item WHERE id=?1", [id.to_string()])
                .map_err(db_error)?;
            if deleted == 0 {
                return Err(not_found(format!("no queued message {id}")));
            }
            Ok(())
        })
    }
}

impl ApprovalRepo for SqliteCollabStore {
    fn approval(&self, id: ApprovalId) -> Result<Option<Approval>> {
        self.with(|c| select_one(c, &APPROVAL, "WHERE id=?1", [id.to_string()]))
    }

    fn approvals(&self, status: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        self.with(|c| match status {
            None => select(c, &APPROVAL, "ORDER BY created_at, rowid", []),
            Some(status) => {
                let Value::String(status) = serde_json::to_value(status).map_err(internal)? else {
                    return Err(internal("approval status is a string"));
                };
                select(
                    c,
                    &APPROVAL,
                    "WHERE status=?1 ORDER BY created_at, rowid",
                    [status],
                )
            }
        })
    }

    fn put_approval(&self, approval: &Approval) -> Result<()> {
        self.with(|c| upsert(c, &APPROVAL, approval, &[]))
    }
}

impl RuleRepo for SqliteCollabStore {
    fn rules(&self, agent_id: Option<AgentId>) -> Result<Vec<Rule>> {
        self.with(|c| match agent_id {
            None => select(c, &RULE, "ORDER BY created_at, rowid", []),
            Some(agent_id) => select(
                c,
                &RULE,
                "WHERE agent_id=?1 ORDER BY created_at, rowid",
                [agent_id.to_string()],
            ),
        })
    }

    fn rule(&self, id: RuleId) -> Result<Option<Rule>> {
        self.with(|c| select_one(c, &RULE, "WHERE id=?1", [id.to_string()]))
    }

    fn put_rule(&self, rule: &Rule) -> Result<()> {
        self.with(|c| upsert(c, &RULE, rule, &[]))
    }

    /// Counts the use whatever the limits say; the policy checks them first.
    fn record_rule_use(&self, id: RuleId) -> Result<Rule> {
        self.with(|c| {
            let transaction = c.transaction().map_err(db_error)?;
            let changed = transaction
                .execute(
                    "UPDATE rule SET uses=MIN(uses + 1, ?1) WHERE id=?2",
                    params![u32::MAX, id.to_string()],
                )
                .map_err(db_error)?;
            if changed == 0 {
                return Err(not_found(format!("no rule {id}")));
            }
            let rule = select_one(&transaction, &RULE, "WHERE id=?1", [id.to_string()])?
                .ok_or_else(|| not_found(format!("no rule {id}")))?;
            transaction.commit().map_err(db_error)?;
            Ok(rule)
        })
    }
}
