use formation_proto::{
    agent::{Activity, Agent, Attention, AttentionTarget, Schedule, ScheduleRun},
    common::{
        ActivityId, AgentId, ApiError, ErrorCode, MemberId, RunId, ScheduleId, SessionId, Timestamp,
    },
    harness::SessionInfo,
    request::QueueItem,
    snapshot::Settings,
};
use rusqlite::{Connection, OptionalExtension, Params, params, types::Value as SqlValue};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use super::{SqliteStore, db_error, json_error};
use crate::ports::{
    ActivityRepo, AgentRepo, AttentionRepo, Result, ScheduleRepo, SessionRepo, SettingsRepo,
};

fn timestamp(at: Timestamp) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()
}

// Nested proto fields remain JSON; all other fields are native SQL scalars.
const JSON_FIELDS: &[&str] = &[
    "repositories",
    "detected",
    "running_requests",
    "rule",
    "last_run",
    "target",
    "notifications",
];

fn read<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    params: impl Params,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql).map_err(db_error)?;
    let columns: Vec<_> = statement.column_names().map(|s| s.to_string()).collect();
    let mut rows = statement.query(params).map_err(db_error)?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(db_error)? {
        let mut object = Map::new();
        for (i, column) in columns.iter().enumerate() {
            if matches!(column.as_str(), "deleted_at" | "run_id") {
                continue;
            }
            let value = match row.get::<_, SqlValue>(i).map_err(db_error)? {
                SqlValue::Null => Value::Null,
                SqlValue::Integer(n) if column == "paused" => Value::Bool(n != 0),
                SqlValue::Integer(n) => Value::from(n),
                SqlValue::Text(s) if JSON_FIELDS.contains(&column.as_str()) => {
                    serde_json::from_str(&s).map_err(json_error)?
                }
                SqlValue::Text(s) => Value::String(s),
                _ => {
                    return Err(ApiError::new(
                        ErrorCode::Internal,
                        "unexpected SQLite field type",
                    ));
                }
            };
            object.insert(column.clone(), value);
        }
        if let Some(shape) = object.remove("avatar_shape") {
            let color = object.remove("avatar_color").unwrap_or(Value::Null);
            object.insert(
                "avatar".into(),
                serde_json::json!({"shape": shape, "color": color}),
            );
        }
        values.push(serde_json::from_value(Value::Object(object)).map_err(json_error)?);
    }
    Ok(values)
}

fn fields(value: &impl Serialize) -> Result<Map<String, Value>> {
    let Value::Object(mut fields) = serde_json::to_value(value).map_err(json_error)? else {
        unreachable!("repository records are objects")
    };
    if let Some(Value::Object(avatar)) = fields.remove("avatar") {
        for (key, value) in avatar {
            fields.insert(format!("avatar_{key}"), value);
        }
    }
    Ok(fields)
}

fn sql_values(fields: &Map<String, Value>) -> Result<Vec<SqlValue>> {
    fields
        .iter()
        .map(|(key, value)| {
            Ok(match value {
                Value::Null => SqlValue::Null,
                Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
                Value::String(s) => {
                    let s = if key == "at" || key.ends_with("_at") {
                        timestamp(
                            s.parse::<Timestamp>()
                                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?,
                        )
                    } else {
                        s.clone()
                    };
                    SqlValue::Text(s)
                }
                _ => SqlValue::Text(serde_json::to_string(value).map_err(json_error)?),
            })
        })
        .collect()
}

fn put(
    connection: &Connection,
    table: &str,
    key: &str,
    fields: Map<String, Value>,
    insert_only: bool,
) -> Result<()> {
    let names: Vec<_> = fields.keys().map(String::as_str).collect();
    let marks = vec!["?"; names.len()].join(",");
    let mut sql = format!("INSERT INTO {table} ({}) VALUES ({marks})", names.join(","));
    if !insert_only {
        let updates = names
            .iter()
            .filter(|name| **name != key)
            .map(|name| format!("{name}=excluded.{name}"))
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" ON CONFLICT({key}) DO UPDATE SET {updates}"));
    }
    connection
        .execute(&sql, rusqlite::params_from_iter(sql_values(&fields)?))
        .map_err(db_error)?;
    Ok(())
}

impl AgentRepo for SqliteStore {
    fn agents(&self) -> Result<Vec<Agent>> {
        self.with_connection(|c| {
            read(
                c,
                "SELECT * FROM agents WHERE deleted_at IS NULL ORDER BY rowid",
                [],
            )
        })
    }
    fn agent(&self, id: AgentId) -> Result<Option<Agent>> {
        self.with_connection(|c| {
            Ok(read(
                c,
                "SELECT * FROM agents WHERE id=? AND deleted_at IS NULL",
                [id.to_string()],
            )?
            .pop())
        })
    }
    fn agent_by_slug(&self, slug: &str) -> Result<Option<Agent>> {
        self.with_connection(|c| {
            Ok(read(
                c,
                "SELECT * FROM agents WHERE slug=? AND deleted_at IS NULL",
                [slug],
            )?
            .pop())
        })
    }
    fn insert_agent(&self, agent: &Agent) -> Result<()> {
        self.with_connection(|c| put(c, "agents", "id", fields(agent)?, true))
    }
    fn update_agent(&self, agent: &Agent) -> Result<()> {
        self.with_connection(|c| {
            let fields = fields(agent)?;
            let assignments = fields
                .keys()
                .map(|name| format!("{name}=?"))
                .collect::<Vec<_>>()
                .join(",");
            let mut values = sql_values(&fields)?;
            values.push(SqlValue::Text(agent.id.to_string()));
            if c.execute(
                &format!("UPDATE agents SET {assignments} WHERE id=? AND deleted_at IS NULL"),
                rusqlite::params_from_iter(values),
            )
            .map_err(db_error)?
                == 0
            {
                return Err(ApiError::new(ErrorCode::NotFound, "agent not found"));
            }
            Ok(())
        })
    }
    fn delete_agent(&self, id: AgentId, at: Timestamp) -> Result<()> {
        self.with_connection(|c| {
            c.execute(
                "UPDATE agents SET deleted_at=? WHERE id=? AND deleted_at IS NULL",
                (timestamp(at), id.to_string()),
            )
            .map_err(db_error)?;
            Ok(())
        })
    }
}

impl SessionRepo for SqliteStore {
    fn main_session(&self, agent_id: AgentId) -> Result<Option<SessionInfo>> {
        self.with_connection(|c| {
            Ok(read(
                c,
                "SELECT * FROM main_sessions WHERE agent_id=?",
                [agent_id.to_string()],
            )?
            .pop())
        })
    }
    fn put_main_session(&self, session: &SessionInfo) -> Result<()> {
        self.with_connection(|c| put(c, "main_sessions", "agent_id", fields(session)?, false))
    }
    fn fork_session(&self, run_id: RunId) -> Result<Option<SessionInfo>> {
        self.with_connection(|c| {
            Ok(read(
                c,
                "SELECT * FROM fork_sessions WHERE run_id=?",
                [run_id.to_string()],
            )?
            .pop())
        })
    }
    fn put_fork_session(&self, run_id: RunId, session: &SessionInfo) -> Result<()> {
        let mut fields = fields(session)?;
        fields.insert("run_id".into(), Value::String(run_id.to_string()));
        self.with_connection(|c| put(c, "fork_sessions", "run_id", fields, false))
    }
    fn is_formation_session(&self, session_id: &SessionId) -> Result<bool> {
        self.with_connection(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM main_sessions WHERE session_id=?1) OR EXISTS(SELECT 1 FROM fork_sessions WHERE session_id=?1)",
                [&session_id.0],
                |r| r.get(0),
            )
            .map_err(db_error)
        })
    }
    fn owner_queue(&self, agent_id: AgentId) -> Result<Vec<QueueItem>> {
        let items: Option<String> = self.with_connection(|c| {
            c.query_row(
                "SELECT items FROM owner_queues WHERE agent_id=?1",
                [agent_id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)
        })?;
        items.map_or(Ok(Vec::new()), |items| {
            serde_json::from_str(&items).map_err(json_error)
        })
    }
    fn put_owner_queue(&self, agent_id: AgentId, items: &[QueueItem]) -> Result<()> {
        let items = serde_json::to_string(items).map_err(json_error)?;
        self.with_connection(|c| {
            c.execute(
                "INSERT INTO owner_queues (agent_id, items) VALUES (?1, ?2) ON CONFLICT(agent_id) DO UPDATE SET items=excluded.items",
                (agent_id.to_string(), items),
            )
            .map(drop)
            .map_err(db_error)
        })
    }
}

impl ScheduleRepo for SqliteStore {
    fn schedules(&self, agent_id: Option<AgentId>) -> Result<Vec<Schedule>> {
        self.with_connection(|c| {
            read(
                c,
                "SELECT * FROM schedules WHERE (?1 IS NULL OR agent_id=?1) ORDER BY rowid",
                [agent_id.map(|id| id.to_string())],
            )
        })
    }
    fn schedule(&self, id: ScheduleId) -> Result<Option<Schedule>> {
        self.with_connection(|c| {
            Ok(read(c, "SELECT * FROM schedules WHERE id=?", [id.to_string()])?.pop())
        })
    }
    fn put_schedule(&self, schedule: &Schedule) -> Result<()> {
        self.with_connection(|c| put(c, "schedules", "id", fields(schedule)?, false))
    }
    fn delete_schedule(&self, id: ScheduleId) -> Result<()> {
        self.with_connection(|c| {
            c.execute("DELETE FROM schedules WHERE id=?", [id.to_string()])
                .map_err(db_error)?;
            Ok(())
        })
    }
    fn put_schedule_run(&self, run: &ScheduleRun) -> Result<()> {
        self.with_connection(|c| put(c, "schedule_runs", "id", fields(run)?, false))
    }
    fn schedule_runs(
        &self,
        schedule_id: ScheduleId,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<ScheduleRun>> {
        self.with_connection(|c| read(c, "SELECT * FROM schedule_runs WHERE schedule_id=?1 AND (?2 IS NULL OR planned_at<?2) ORDER BY planned_at DESC, id DESC LIMIT ?3", params![schedule_id.to_string(), before.map(timestamp), limit]))
    }
}

impl AttentionRepo for SqliteStore {
    fn open_attention(&self) -> Result<Vec<Attention>> {
        self.with_connection(|c| {
            read(
                c,
                "SELECT * FROM attention WHERE resolved_at IS NULL ORDER BY at, id",
                [],
            )
        })
    }
    fn put_attention(&self, item: &Attention) -> Result<()> {
        self.with_connection(|c| put(c, "attention", "id", fields(item)?, false))
    }
    fn resolve_attention(&self, target: &AttentionTarget, at: Timestamp) -> Result<Vec<Attention>> {
        let target = serde_json::to_string(target).map_err(json_error)?;
        self.with_connection(|c| {
            let tx = c.transaction().map_err(db_error)?;
            let mut items: Vec<Attention> = read(
                &tx,
                "SELECT * FROM attention WHERE target=? AND resolved_at IS NULL ORDER BY at, id",
                [&target],
            )?;
            tx.execute(
                "UPDATE attention SET resolved_at=? WHERE target=? AND resolved_at IS NULL",
                (timestamp(at), target),
            )
            .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            for item in &mut items {
                item.resolved_at = Some(at);
            }
            Ok(items)
        })
    }
}

impl ActivityRepo for SqliteStore {
    fn record_activity(&self, activity: &Activity) -> Result<()> {
        self.with_connection(|c| put(c, "activity", "id", fields(activity)?, false))
    }
    fn activity(
        &self,
        agent_id: Option<AgentId>,
        member_id: Option<MemberId>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<Activity>> {
        self.with_connection(|c| read(c, "SELECT * FROM activity WHERE (?1 IS NULL OR agent_id=?1) AND (?2 IS NULL OR actor=?2) AND (?3 IS NULL OR at<?3) ORDER BY at DESC, id DESC LIMIT ?4", params![agent_id.map(|id| id.to_string()), member_id.map(|id| id.to_string()), before.map(timestamp), limit]))
    }
    fn delete_activity(&self, ids: &[ActivityId]) -> Result<()> {
        self.with_connection(|c| {
            let tx = c.transaction().map_err(db_error)?;
            for id in ids {
                tx.execute("DELETE FROM activity WHERE id=?", [id.to_string()])
                    .map_err(db_error)?;
            }
            tx.commit().map_err(db_error)
        })
    }
}

impl SettingsRepo for SqliteStore {
    fn settings(&self) -> Result<Settings> {
        self.with_connection(|c| {
            Ok(read(c, "SELECT language, notifications FROM settings", [])?
                .pop()
                .unwrap_or_default())
        })
    }
    fn put_settings(&self, settings: &Settings) -> Result<()> {
        let mut fields = fields(settings)?;
        fields.insert("id".into(), Value::String("1".into()));
        self.with_connection(|c| put(c, "settings", "id", fields, false))
    }
}
