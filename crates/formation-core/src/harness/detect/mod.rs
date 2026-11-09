//! Read-only discovery of external sessions and registration with Formation.
//! Claude Code and Codex activity is recent for 24 hours; live state stays unknown.
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use formation_proto::{
    agent::*,
    common::{AgentId, ApiError, ErrorCode, SessionId, Timestamp},
    events::Event,
    policy::Caller,
};
use serde_json::Value;

use crate::{
    ask::transcript::{claude_config_dir, project_dir_name},
    ports::{CommandCall, CommandHandler, EventSink, Result, Store},
};

/// Paths are injectable so discovery never needs the owner's real installations.
#[derive(Clone)]
pub struct DetectPaths {
    pub claude_config: PathBuf,
    /// `$CODEX_HOME/sessions`, or `~/.codex/sessions` when `CODEX_HOME` is unset.
    pub codex_sessions: PathBuf,
    pub hermes: Vec<PathBuf>,
    pub openclaw: Vec<PathBuf>,
}

impl Default for DetectPaths {
    fn default() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let candidates = |name: &str| {
            let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).map(|p| p.join(name)).collect())
                .unwrap_or_default();
            if let Some(home) = &home {
                paths.push(home.join(format!(".{name}")));
            }
            paths
        };
        Self {
            claude_config: claude_config_dir().unwrap_or_default(),
            codex_sessions: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(".codex")))
                .map(|h| h.join("sessions"))
                .unwrap_or_default(),
            hermes: candidates("hermes"),
            openclaw: candidates("openclaw"),
        }
    }
}

#[derive(Clone)]
pub struct Detect {
    paths: DetectPaths,
    store: Arc<dyn Store>,
    events: Arc<dyn EventSink>,
    operations: Arc<Mutex<()>>,
}

impl Detect {
    pub fn new(paths: DetectPaths, store: Arc<dyn Store>, events: Arc<dyn EventSink>) -> Self {
        Self {
            paths,
            store,
            events,
            operations: Arc::new(Mutex::new(())),
        }
    }

    fn scan(&self) -> Result<Vec<(DetectedAgent, PathBuf)>> {
        let agents = self.store.agents()?;
        let excluded: HashSet<_> = agents
            .iter()
            .map(|a| {
                project_dir_name(
                    &a.folder_path
                        .canonicalize()
                        .unwrap_or(a.folder_path.clone()),
                )
            })
            .collect();
        let folders: HashSet<_> = agents
            .iter()
            .map(|a| {
                a.folder_path
                    .canonicalize()
                    .unwrap_or(a.folder_path.clone())
            })
            .collect();
        let mut sessions = HashSet::new();
        let mut codex_sources = Vec::new();
        for agent in &agents {
            if let Some(session) = self.store.main_session(agent.id)? {
                sessions.insert(session.session_id);
            }
            if let Some(source) = &agent.detected
                && source.harness == DetectedHarness::ClaudeCode
                && let Some(stem) = source.path.file_stem().and_then(|s| s.to_str())
            {
                sessions.insert(SessionId(stem.into()));
            }
            if let Some(source) = &agent.detected
                && source.harness == DetectedHarness::Codex
                && let Some(stem) = source.path.file_stem().and_then(|s| s.to_str())
            {
                codex_sources.push(stem.to_owned());
            }
        }
        let mut found = Vec::new();
        let cutoff: Timestamp = (SystemTime::now() - Duration::from_secs(24 * 60 * 60)).into();
        for project in entries(&self.paths.claude_config.join("projects"))? {
            if !project.is_dir()
                || project
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| excluded.contains(s))
            {
                continue;
            }
            for path in entries(&project)? {
                if path.extension().and_then(|s| s.to_str()) != Some("jsonl") || !path.is_file() {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let mut cwd = None;
                let mut session = None;
                let mut latest = None;
                for line in text.lines() {
                    let Ok(record) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    if let Some(dir) = record["cwd"].as_str() {
                        cwd = Some(PathBuf::from(dir));
                    }
                    if let Some(id) = record["sessionId"].as_str() {
                        session = Some(SessionId(id.into()));
                    }
                    if let Some(at) = record["timestamp"]
                        .as_str()
                        .and_then(|s| s.parse::<Timestamp>().ok())
                    {
                        latest = Some(latest.map_or(at, |old: Timestamp| old.max(at)));
                    }
                }
                let (Some(cwd), Some(session_id), Some(at)) = (cwd, session, latest) else {
                    continue;
                };
                if at < cutoff || sessions.contains(&session_id) {
                    continue;
                }
                // The transcript path preserves the session identity in DetectedSource.
                if path.file_stem().and_then(|s| s.to_str()) != Some(session_id.0.as_str()) {
                    continue;
                }
                found.push((
                    DetectedAgent {
                        key: format!("claude_code:{}", session_id.0),
                        harness: DetectedHarness::ClaudeCode,
                        name: cwd
                            .file_name()
                            .unwrap_or(cwd.as_os_str())
                            .to_string_lossy()
                            .into_owned(),
                        path,
                        session_id: Some(session_id),
                        last_activity_at: Some(at),
                        capabilities: capabilities(None),
                        connected_agent: None,
                    },
                    cwd,
                ));
            }
        }
        let mut rollouts = Vec::new();
        if !self.paths.codex_sessions.as_os_str().is_empty() {
            rollout_files(&self.paths.codex_sessions, &mut rollouts)?;
        }
        for path in rollouts {
            if fs::metadata(&path)
                .and_then(|m| m.modified())
                .is_ok_and(|at| Timestamp::from(at) < cutoff)
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Some(meta) = text
                .lines()
                .next()
                .and_then(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|record| record["type"] == "session_meta")
            else {
                continue;
            };
            let (Some(id), Some(cwd)) = (
                meta["payload"]["id"].as_str(),
                meta["payload"]["cwd"].as_str(),
            ) else {
                continue;
            };
            let cwd = PathBuf::from(cwd);
            // The newest record carries the latest activity; scan from the end.
            let latest = text.lines().rev().find_map(|line| {
                serde_json::from_str::<Value>(line).ok()?["timestamp"]
                    .as_str()?
                    .parse::<Timestamp>()
                    .ok()
            });
            let suffix = format!("-{id}");
            // The rollout path preserves the session identity in DetectedSource.
            let identified = path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with(&suffix));
            if !identified
                || latest.is_none_or(|at| at < cutoff)
                || sessions.contains(&SessionId(id.into()))
                || codex_sources.iter().any(|s| s.ends_with(&suffix))
                || folders.contains(&cwd.canonicalize().unwrap_or(cwd.clone()))
            {
                continue;
            }
            found.push((
                DetectedAgent {
                    key: format!("codex:{id}"),
                    harness: DetectedHarness::Codex,
                    name: cwd
                        .file_name()
                        .unwrap_or(cwd.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                    path,
                    session_id: Some(SessionId(id.into())),
                    last_activity_at: latest,
                    capabilities: capabilities(Some("Codex support arrives with TASK-041")),
                    connected_agent: None,
                },
                cwd,
            ));
        }
        for (harness, name, paths) in [
            (DetectedHarness::Hermes, "Hermes", &self.paths.hermes),
            (DetectedHarness::OpenClaw, "OpenClaw", &self.paths.openclaw),
        ] {
            if let Some(path) = paths.iter().find(|p| is_installation(p)) {
                let connected_agent = agents
                    .iter()
                    .find(|a| a.detected.as_ref().is_some_and(|s| s.harness == harness))
                    .map(|a| a.id);
                let folder = if path.is_dir() {
                    path.clone()
                } else {
                    path.parent().unwrap_or(path).to_path_buf()
                };
                found.push((
                    DetectedAgent {
                        key: name.to_ascii_lowercase(),
                        harness,
                        name: name.into(),
                        path: path.clone(),
                        session_id: None,
                        last_activity_at: None,
                        capabilities: capabilities(Some(
                            "No conversation or fork adapter is available for this harness",
                        )),
                        connected_agent,
                    },
                    folder,
                ));
            }
        }
        found.sort_by(|a, b| a.0.key.cmp(&b.0.key));
        Ok(found)
    }

    fn connect(&self, key: &str) -> Result<Agent> {
        let (detected, folder_path) = self
            .scan()?
            .into_iter()
            .find(|(d, _)| d.key == key)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "detected agent not found"))?;
        if let Some(id) = detected.connected_agent {
            return self
                .store
                .agent(id)?
                .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "connected agent not found"));
        }
        let id = AgentId::generate();
        let agent = Agent {
            id,
            slug: format!("detected-{id}"),
            name: detected.name,
            role: String::new(),
            harness: HarnessKind::Detected,
            folder_path,
            repositories: Vec::new(),
            avatar: Avatar {
                shape: "round".into(),
                color: "blue".into(),
            },
            state: AgentState::Unknown,
            files_revision: String::new(),
            created_at: SystemTime::now().into(),
            last_activity_at: detected.last_activity_at,
            detected: Some(DetectedSource {
                harness: detected.harness,
                path: detected.path,
                capabilities: detected.capabilities,
            }),
        };
        self.store.insert_agent(&agent)?;
        self.events
            .publish(Event::AgentUpdate(AgentUpdateEvent::Upsert {
                agent: Box::new(agent.clone()),
            }));
        // The snapshot carries the reason in `detected.capabilities.state`; say it on `agent.state` too.
        self.events.publish(Event::AgentState(AgentStateEvent {
            agent_id: id,
            state: AgentState::Unknown,
            reason: agent
                .detected
                .as_ref()
                .and_then(|d| d.capabilities.state.reason.clone()),
            exit_code: None,
        }));
        Ok(agent)
    }
}

#[async_trait]
impl CommandHandler for Detect {
    fn methods(&self) -> Vec<&'static str> {
        vec!["agents_detect", "agent_connect"]
    }

    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "detection requires the owner",
            ));
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = this
                .operations
                .lock()
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            let params = if call.params.is_null() {
                serde_json::json!({})
            } else {
                call.params
            };
            match call.method.as_str() {
                "agents_detect" => {
                    serde_json::from_value::<AgentsDetect>(params).map_err(invalid_params)?;
                    serde_json::to_value(
                        this.scan()?.into_iter().map(|(d, _)| d).collect::<Vec<_>>(),
                    )
                    .map_err(internal)
                }
                "agent_connect" => {
                    let args: AgentConnect =
                        serde_json::from_value(params).map_err(invalid_params)?;
                    serde_json::to_value(this.connect(&args.key)?).map_err(internal)
                }
                _ => Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "unknown detection command",
                )),
            }
        })
        .await
        .map_err(internal)?
    }
}

fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(path) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.path()).map_err(internal))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(internal(e)),
    }
}

/// Collects `rollout-*.jsonl` files under the `YYYY/MM/DD` folders Codex writes.
fn rollout_files(dir: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for path in entries(dir)? {
        if path.is_dir() {
            rollout_files(&path, found)?;
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("rollout-") && s.ends_with(".jsonl"))
        {
            found.push(path);
        }
    }
    Ok(())
}

fn is_installation(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_dir()
        || (path.is_file() && fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
}

/// `unreadable` is why the conversation cannot be read, or `None` when it can.
fn capabilities(unreadable: Option<&str>) -> DetectedCapabilities {
    let unsupported = |reason: &str| Capability {
        supported: false,
        reason: Some(reason.into()),
    };
    let readable = match unreadable {
        Some(reason) => unsupported(reason),
        None => Capability {
            supported: true,
            reason: None,
        },
    };
    DetectedCapabilities {
        state: unsupported("Live session state cannot be determined from discovery"),
        read_conversation: readable.clone(),
        answer_teammates: readable,
        receive_requests: unsupported("Work requests for detected agents are not implemented"),
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}
fn invalid_params(e: serde_json::Error) -> ApiError {
    ApiError::new(ErrorCode::InvalidParams, e.to_string())
}
