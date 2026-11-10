//! Owner: A (agent runtime).
//!
//! Agents, harness status, detection, agent files, schedules, needs-attention items,
//! and the activity history.

use std::path::PathBuf;

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::{
    ActivityId, AgentId, ApprovalId, AttentionId, MemberId, QuestionId, RequestId, RunId,
    ScheduleId, ScheduleRunId, SessionId, Timestamp, commands,
};

/// The harness an agent runs on (D-002: Claude Code first, then Codex).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
    /// Started outside Formation and connected later (FR-006); see [`DetectedSource`].
    Detected,
}

/// What a detected agent runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DetectedHarness {
    ClaudeCode,
    Codex,
    Hermes,
    OpenClaw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentState {
    Working,
    Idle,
    WaitingForApproval,
    /// Working with no harness output for the stalled threshold (10 minutes, FR-007).
    Stalled,
    Error,
    Stopped,
    /// A detected agent whose state Formation cannot tell. Also what an older Mac
    /// reads for a state it does not know yet.
    #[serde(other)]
    Unknown,
}

/// An `open-bot-avatar` choice (D-042). Values are the library's own names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Avatar {
    pub shape: String,
    pub color: String,
}

/// Whether one operation works on a detected agent, and why not (FR-006).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Capability {
    pub supported: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DetectedCapabilities {
    pub state: Capability,
    pub read_conversation: Capability,
    pub answer_teammates: Capability,
    pub receive_requests: Capability,
}

/// Where a connected detected agent came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DetectedSource {
    pub harness: DetectedHarness,
    pub path: PathBuf,
    pub capabilities: DetectedCapabilities,
}

/// A teammate's work request running in its own fork of the agent's session (#35).
/// Shown over the agent's state; the main session stays free for the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunningRequest {
    pub request_id: RequestId,
    pub requester: MemberId,
    pub prompt: String,
    pub started_at: Timestamp,
    pub run_id: RunId,
    /// `None` until Claude names the fork's session.
    pub fork_session_id: Option<SessionId>,
}

/// An agent on this Mac. Its folder holds `AGENTS.md`, `CLAUDE.md`, `.claude/skills/`,
/// and `formation.toml` (FR-001).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Agent {
    pub id: AgentId,
    /// Unique per Mac; used in CLI addresses such as `dohyeon/api`.
    pub slug: String,
    pub name: String,
    /// Free text, for example "API developer" or "Marketing".
    pub role: String,
    pub harness: HarnessKind,
    pub folder_path: PathBuf,
    /// Repositories assigned to the agent; reads inside them, like reads inside the
    /// agent folder, run without asking (D-028).
    pub repositories: Vec<PathBuf>,
    pub avatar: Avatar,
    pub state: AgentState,
    /// Changes whenever the agent's files change, including edits made outside Formation.
    pub files_revision: String,
    pub created_at: Timestamp,
    pub last_activity_at: Option<Timestamp>,
    /// Set only for connected detected agents.
    pub detected: Option<DetectedSource>,
    /// Teammates' requests running in forks now; empty after a restart.
    #[serde(default)]
    pub running_requests: Vec<RunningRequest>,
}

/// Install and sign-in state of a harness on this Mac (SC-001, SC-018).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HarnessStatus {
    pub harness: HarnessKind,
    pub installed: bool,
    /// `None` when Formation cannot tell.
    pub signed_in: Option<bool>,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
    /// What the owner must do, when the harness is not ready.
    pub problem: Option<String>,
}

/// An agent found outside Formation (FR-006).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DetectedAgent {
    /// Stable key for this detection, used by `agent_connect`.
    pub key: String,
    pub harness: DetectedHarness,
    pub name: String,
    pub path: PathBuf,
    /// The session's working folder, or the installation's folder; `name` is its last part.
    pub folder: PathBuf,
    pub session_id: Option<SessionId>,
    pub last_activity_at: Option<Timestamp>,
    pub capabilities: DetectedCapabilities,
    /// Set when this detection is already connected.
    pub connected_agent: Option<AgentId>,
}

/// One editable file, with its path relative to the agent folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentFile {
    /// `AGENTS.md` (canonical instructions) or a file under `.claude/skills/`.
    pub path: PathBuf,
    pub content: String,
}

/// The editable files of an agent: instructions and skills (SC-006, FR-004).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentFiles {
    pub agent_id: AgentId,
    pub files: Vec<AgentFile>,
    /// Equals `Agent::files_revision`.
    pub revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StopMode {
    /// Let the current turn finish, then stop.
    AfterTurn,
    /// Interrupt the current turn and stop.
    Now,
}

// ---------------------------------------------------------------------------
// Schedules (FR-005)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum ScheduleRule {
    Daily,
    Weekdays,
    Weekly { weekday: Weekday },
    EveryHours { hours: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ScheduleRunResult {
    Completed,
    Failed,
    /// Missed while the Mac slept, then run once on wake.
    MissedRan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRun {
    pub id: ScheduleRunId,
    pub schedule_id: ScheduleId,
    pub planned_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    /// `None` while running.
    pub result: Option<ScheduleRunResult>,
    /// Why a failed run failed.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Schedule {
    pub id: ScheduleId,
    pub agent_id: AgentId,
    pub prompt: String,
    pub rule: ScheduleRule,
    /// Local time of day.
    pub time: NaiveTime,
    pub paused: bool,
    pub next_run_at: Option<Timestamp>,
    pub last_run: Option<ScheduleRun>,
}

// ---------------------------------------------------------------------------
// Needs attention (FR-007): the inbox and macOS notifications
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AttentionKind {
    Approval,
    /// A teammate's question was sent to me in person.
    QuestionForYou,
    AgentError,
    AgentStalled,
    ScheduleFailed,
    /// A scheduled run was missed while the Mac slept and ran late.
    ScheduleMissed,
    /// My work request to a teammate finished, failed, or was denied.
    RequestFinished,
}

/// What a needs-attention item opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum AttentionTarget {
    Agent { agent_id: AgentId },
    Approval { approval_id: ApprovalId },
    Question { question_id: QuestionId },
    Request { request_id: RequestId },
    Schedule { schedule_id: ScheduleId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Attention {
    pub id: AttentionId,
    pub kind: AttentionKind,
    pub target: AttentionTarget,
    /// Short facts for the notification and inbox row (names, command); the UI adds
    /// localized wording by `kind`.
    pub subject: String,
    pub at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}

// ---------------------------------------------------------------------------
// History (FR-020)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ActivityKind {
    QuestionAsked,
    QuestionAnswered,
    RequestSent,
    RequestReceived,
    RequestFinished,
    ApprovalDecided,
    AgentStarted,
    AgentStopped,
    AgentFailed,
    ScheduleRan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Activity {
    pub id: ActivityId,
    pub kind: ActivityKind,
    /// `None` when the actor is this Mac's owner.
    pub actor: Option<MemberId>,
    pub agent_id: Option<AgentId>,
    pub target: Option<AttentionTarget>,
    pub at: Timestamp,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentsList {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentCreate {
    pub name: String,
    pub role: String,
    /// `claude_code` or `codex`; `detected` is rejected with `harness_unsupported`.
    pub harness: HarnessKind,
    pub avatar: Avatar,
    /// Defaults to a folder named after the slug in the agents directory.
    pub folder: Option<PathBuf>,
    pub repositories: Vec<PathBuf>,
    /// Initial `AGENTS.md`; a role template when `None`.
    pub instructions: Option<String>,
}

/// Fields left `None` keep their value. `repositories` replaces the whole list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentUpdate {
    pub agent_id: AgentId,
    pub name: Option<String>,
    pub role: Option<String>,
    pub avatar: Option<Avatar>,
    pub repositories: Option<Vec<PathBuf>>,
}

/// Detected agents (sessions started outside Formation) are read and fork only
/// (#14); returns `harness_unsupported` for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStart {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStop {
    pub agent_id: AgentId,
    pub mode: StopMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentsDetect {
    /// Lists every Claude Code and Codex session that ran in this folder or inside
    /// it, however old; without it, only sessions active in the last 24 hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentConnect {
    pub key: String,
    /// The `folder` the agent was found with in `agents_detect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentFilesRead {
    pub agent_id: AgentId,
}

/// Writes one file, creating it when new; `content: None` deletes a skill file.
/// Fails with `conflict` when `base_revision` is not the current revision (a file
/// changed on disk, SC-006), and with `denied` for a path other than `AGENTS.md` or
/// one under `.claude/skills/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentFilesWrite {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub content: Option<String>,
    pub base_revision: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HarnessCheck {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleList {
    pub agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleCreate {
    pub agent_id: AgentId,
    pub prompt: String,
    pub rule: ScheduleRule,
    pub time: NaiveTime,
}

/// Fields left `None` keep their value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleUpdate {
    pub schedule_id: ScheduleId,
    pub prompt: Option<String>,
    pub rule: Option<ScheduleRule>,
    pub time: Option<NaiveTime>,
    pub paused: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleDelete {
    pub schedule_id: ScheduleId,
}

/// Schedule run history (SC-007), newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRunList {
    pub schedule_id: ScheduleId,
    pub before: Option<Timestamp>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistoryList {
    pub agent_id: Option<AgentId>,
    pub member_id: Option<MemberId>,
    pub before: Option<Timestamp>,
    pub limit: u32,
}

/// Deletes this Mac's copy only (backend architecture, "Storage and data ownership").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistoryDelete {
    pub ids: Vec<ActivityId>,
}

commands! {
    AgentsList => "agents_list" -> Vec<Agent>;
    AgentCreate => "agent_create" -> Agent;
    AgentUpdate => "agent_update" -> Agent;
    AgentStart => "agent_start" -> Agent;
    AgentStop => "agent_stop" -> Agent;
    AgentsDetect => "agents_detect" -> Vec<DetectedAgent>;
    AgentConnect => "agent_connect" -> Agent;
    AgentFilesRead => "agent_files_read" -> AgentFiles;
    AgentFilesWrite => "agent_files_write" -> AgentFiles;
    HarnessCheck => "harness_check" -> Vec<HarnessStatus>;
    ScheduleList => "schedule_list" -> Vec<Schedule>;
    ScheduleCreate => "schedule_create" -> Schedule;
    ScheduleUpdate => "schedule_update" -> Schedule;
    ScheduleDelete => "schedule_delete" -> ();
    ScheduleRunList => "schedule_run_list" -> Vec<ScheduleRun>;
    HistoryList => "history_list" -> Vec<Activity>;
    HistoryDelete => "history_delete" -> ();
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

/// `agent.state`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStateEvent {
    pub agent_id: AgentId,
    pub state: AgentState,
    pub reason: Option<String>,
    pub exit_code: Option<i32>,
}

/// `agent.update`: an agent was created, connected, renamed, or its files changed; or it was removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "change", rename_all = "snake_case")]
#[ts(export)]
pub enum AgentUpdateEvent {
    Upsert { agent: Box<Agent> },
    Removed { agent_id: AgentId },
}

/// `schedule.update`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "change", rename_all = "snake_case")]
#[ts(export)]
pub enum ScheduleUpdateEvent {
    Upsert { schedule: Schedule },
    Removed { schedule_id: ScheduleId },
}

/// `harness.update`: install or sign-in state changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HarnessUpdateEvent {
    pub harnesses: Vec<HarnessStatus>,
}

/// `notify`: a needs-attention item was raised or resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NotifyEvent {
    pub item: Attention,
    /// Show a macOS notification (raised, and not muted in settings).
    pub alert: bool,
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::testutil::t0;

    pub fn agent() -> Agent {
        Agent {
            id: AgentId::generate(),
            slug: "api".into(),
            name: "API".into(),
            role: "API developer".into(),
            harness: HarnessKind::ClaudeCode,
            folder_path: "/Users/dohyeon/Agents/api".into(),
            repositories: vec!["/Users/dohyeon/Code/api".into()],
            avatar: Avatar {
                shape: "round".into(),
                color: "blue".into(),
            },
            state: AgentState::Idle,
            files_revision: "r1".into(),
            created_at: t0(),
            last_activity_at: Some(t0()),
            detected: None,
            running_requests: vec![],
        }
    }

    pub fn schedule(agent_id: AgentId) -> Schedule {
        let id = ScheduleId::generate();
        Schedule {
            id,
            agent_id,
            prompt: "Summarize yesterday's commits".into(),
            rule: ScheduleRule::Weekly {
                weekday: Weekday::Mon,
            },
            time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            paused: false,
            next_run_at: Some(t0()),
            last_run: Some(ScheduleRun {
                id: ScheduleRunId::generate(),
                schedule_id: id,
                planned_at: t0(),
                started_at: Some(t0()),
                finished_at: Some(t0()),
                result: Some(ScheduleRunResult::MissedRan),
                error: None,
            }),
        }
    }

    pub fn attention(agent_id: AgentId) -> Attention {
        Attention {
            id: AttentionId::generate(),
            kind: AttentionKind::AgentStalled,
            target: AttentionTarget::Agent { agent_id },
            subject: "API".into(),
            at: t0(),
            resolved_at: None,
        }
    }

    pub fn harness_status() -> HarnessStatus {
        HarnessStatus {
            harness: HarnessKind::ClaudeCode,
            installed: true,
            signed_in: Some(true),
            version: Some("2.1.0".into()),
            path: Some("/opt/homebrew/bin/claude".into()),
            problem: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;
    use crate::testutil::{roundtrip, t0};
    use serde_json::{Value, json};

    #[test]
    fn agent_round_trips_with_snake_case() {
        let mut agent = agent();
        agent.harness = HarnessKind::Detected;
        agent.state = AgentState::WaitingForApproval;
        let unsupported = Capability {
            supported: false,
            reason: Some("Hermes has no input API".into()),
        };
        agent.detected = Some(DetectedSource {
            harness: DetectedHarness::OpenClaw,
            path: "/Users/dohyeon/.openclaw".into(),
            capabilities: DetectedCapabilities {
                state: Capability {
                    supported: true,
                    reason: None,
                },
                read_conversation: unsupported.clone(),
                answer_teammates: unsupported.clone(),
                receive_requests: unsupported,
            },
        });
        agent.running_requests = vec![RunningRequest {
            request_id: RequestId::generate(),
            requester: MemberId::generate(),
            prompt: "Review the API diff".into(),
            started_at: t0(),
            run_id: RunId::generate(),
            fork_session_id: None,
        }];
        let json = roundtrip(&agent);
        assert_eq!(json["running_requests"][0]["fork_session_id"], Value::Null);
        assert_eq!(json["harness"], "detected");
        assert_eq!(json["state"], "waiting_for_approval");
        assert_eq!(json["detected"]["harness"], "open_claw");
    }

    #[test]
    fn schedules_and_attention_round_trip() {
        let agent = agent();
        let json = roundtrip(&schedule(agent.id));
        assert_eq!(json["rule"], json!({"kind": "weekly", "weekday": "mon"}));
        assert_eq!(json["time"], "09:00:00");
        assert_eq!(json["last_run"]["result"], "missed_ran");

        let json = roundtrip(&NotifyEvent {
            item: attention(agent.id),
            alert: true,
        });
        assert_eq!(json["item"]["target"]["kind"], "agent");

        roundtrip(&AgentUpdateEvent::Removed { agent_id: agent.id });
        roundtrip(&harness_status());
        roundtrip(&Activity {
            id: ActivityId::generate(),
            kind: ActivityKind::ApprovalDecided,
            actor: None,
            agent_id: Some(agent.id),
            target: None,
            at: t0(),
            summary: "Allowed npm test".into(),
        });
    }

    #[test]
    fn commands_round_trip() {
        let agent = agent();
        roundtrip(&AgentsList {});
        roundtrip(&AgentCreate {
            name: "Marketing".into(),
            role: "Marketing".into(),
            harness: HarnessKind::Codex,
            avatar: agent.avatar.clone(),
            folder: None,
            repositories: vec![],
            instructions: None,
        });
        roundtrip(&AgentUpdate {
            agent_id: agent.id,
            name: Some("API v2".into()),
            role: None,
            avatar: None,
            repositories: Some(vec![]),
        });
        let json = roundtrip(&AgentStop {
            agent_id: agent.id,
            mode: StopMode::AfterTurn,
        });
        assert_eq!(json["mode"], "after_turn");
        roundtrip(&AgentFiles {
            agent_id: agent.id,
            files: vec![AgentFile {
                path: ".claude/skills/review/SKILL.md".into(),
                content: "# Review".into(),
            }],
            revision: "r1".into(),
        });
        roundtrip(&AgentFilesWrite {
            agent_id: agent.id,
            path: "AGENTS.md".into(),
            content: Some("# API".into()),
            base_revision: "r1".into(),
        });
        let mut older = serde_json::to_value(&agent).unwrap();
        older.as_object_mut().unwrap().remove("running_requests");
        let older: Agent = serde_json::from_value(older).unwrap();
        assert!(older.running_requests.is_empty());
        let state: AgentState = serde_json::from_str(r#""added_later""#).unwrap();
        assert_eq!(state, AgentState::Unknown);
        roundtrip(&HarnessUpdateEvent {
            harnesses: vec![harness_status()],
        });
        roundtrip(&ScheduleRunList {
            schedule_id: ScheduleId::generate(),
            before: Some(t0()),
            limit: 10,
        });
        roundtrip(&ScheduleUpdate {
            schedule_id: ScheduleId::generate(),
            prompt: None,
            rule: Some(ScheduleRule::EveryHours { hours: 4 }),
            time: None,
            paused: Some(true),
        });
    }
}
