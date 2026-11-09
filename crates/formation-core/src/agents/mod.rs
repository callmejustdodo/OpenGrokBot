//! Agent folders and their six local commands (TASK-012).
//!
//! `Agents` polls persisted agents every 250 ms, including agents loaded at service
//! startup. Revisions cover Formation's files, so a save also detects outside
//! edits made before the next poll. Dropping the handler stops its watcher.
//! Metadata edits preserve the stable slug and folder. File changes take effect
//! in the next harness session; this module never starts or alters a session.

mod files;
mod readiness;

pub use readiness::{ClaudeReadiness, HarnessReadiness};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use formation_proto::agent::*;
use formation_proto::common::{AgentId, ApiError, ErrorCode};
use formation_proto::events::Event;
use formation_proto::policy::Caller;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::ports::{CommandCall, CommandHandler, EventSink, Result, Store};

/// One command handler backed by the Store port and an injectable readiness check.
pub struct Agents {
    inner: Arc<Inner>,
    stop: mpsc::Sender<()>,
    watcher: Option<JoinHandle<()>>,
}

struct Inner {
    parent: PathBuf,
    store: Arc<dyn Store>,
    events: Arc<dyn EventSink>,
    readiness: Arc<dyn HarnessReadiness>,
    operations: Mutex<()>,
}

impl Agents {
    /// The parent is used for default folders and relative custom folder paths.
    /// Watcher and command filesystem work run off the async executor.
    pub fn new(
        parent: PathBuf,
        store: Arc<dyn Store>,
        events: Arc<dyn EventSink>,
        readiness: Arc<dyn HarnessReadiness>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&parent).map_err(files::io_error)?;
        let inner = Arc::new(Inner {
            parent: parent.canonicalize().map_err(files::io_error)?,
            store,
            events,
            readiness,
            operations: Mutex::new(()),
        });
        let (stop, receiver) = mpsc::channel();
        let weak = Arc::downgrade(&inner);
        let watcher = thread::Builder::new()
            .name("formation-agent-files".into())
            .spawn(move || {
                while matches!(
                    receiver.recv_timeout(Duration::from_millis(250)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let Some(inner) = weak.upgrade() else { break };
                    let Ok(_guard) = inner.operations.lock() else {
                        break;
                    };
                    if let Ok(agents) = inner.store.agents() {
                        for mut agent in agents {
                            // Unreadable folders are retried next poll; explicit reads
                            // return the error to the caller instead of hiding it.
                            let _ = inner.refresh(&mut agent);
                        }
                    }
                }
            })
            .map_err(files::io_error)?;
        Ok(Self {
            inner,
            stop,
            watcher: Some(watcher),
        })
    }
}

impl Drop for Agents {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

#[async_trait]
impl CommandHandler for Agents {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            "agents_list",
            "agent_create",
            "agent_update",
            "agent_files_read",
            "agent_files_write",
            "harness_check",
        ]
    }

    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "agent management requires the owner",
            ));
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = inner.operations.lock().map_err(|_| {
                ApiError::new(ErrorCode::Internal, "agent operations lock poisoned")
            })?;
            inner.handle(call)
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
    }
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T> {
    serde_json::from_value(params)
        .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))
}
fn encode(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

impl Inner {
    fn handle(&self, call: CommandCall) -> Result<Value> {
        match call.method.as_str() {
            "agents_list" => {
                let _: AgentsList = decode(call.params)?;
                let mut agents = self.store.agents()?;
                for agent in &mut agents {
                    if let Ok(revision) = files::revision(&agent.folder_path) {
                        self.sync_revision(agent, revision)?;
                    }
                }
                encode(agents)
            }
            "agent_create" => encode(self.create(decode(call.params)?)?),
            "agent_update" => encode(self.update(decode(call.params)?)?),
            "agent_files_read" => {
                let args: AgentFilesRead = decode(call.params)?;
                let mut agent = self.agent(args.agent_id)?;
                let files = files::read(&agent)?;
                self.sync_revision(&mut agent, files.revision.clone())?;
                encode(files)
            }
            "agent_files_write" => encode(self.write(decode(call.params)?)?),
            "harness_check" => {
                let _: HarnessCheck = decode(call.params)?;
                let harnesses = self.readiness.check()?;
                self.events
                    .publish(Event::HarnessUpdate(HarnessUpdateEvent {
                        harnesses: harnesses.clone(),
                    }));
                encode(harnesses)
            }
            _ => Err(ApiError::new(
                ErrorCode::InvalidParams,
                "unknown agent command",
            )),
        }
    }

    fn agent(&self, id: AgentId) -> Result<Agent> {
        self.store
            .agent(id)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))
    }

    fn publish(&self, agent: &Agent) {
        self.events
            .publish(Event::AgentUpdate(AgentUpdateEvent::Upsert {
                agent: Box::new(agent.clone()),
            }));
    }

    fn refresh(&self, agent: &mut Agent) -> Result<()> {
        let revision = files::revision(&agent.folder_path)?;
        self.sync_revision(agent, revision)
    }

    fn sync_revision(&self, agent: &mut Agent, revision: String) -> Result<()> {
        if agent.files_revision != revision {
            agent.files_revision = revision;
            self.store.update_agent(agent)?;
            self.publish(agent);
        }
        Ok(())
    }

    fn check_name(&self, name: &str, exclude: Option<AgentId>) -> Result<()> {
        let candidate = slug(name)?;
        let key = name.nfkc().collect::<String>().to_lowercase();
        if self.store.agents()?.iter().any(|agent| {
            Some(agent.id) != exclude
                && (agent.slug == candidate
                    || agent.name.nfkc().collect::<String>().to_lowercase() == key)
        }) {
            // The app tells this conflict apart by this exact message
            // (`apps/desktop/src/views/new-agent/NewAgentSheet.tsx`).
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "agent name or slug is taken",
            ));
        }
        Ok(())
    }

    fn create(&self, args: AgentCreate) -> Result<Agent> {
        if args.harness == HarnessKind::Detected {
            return Err(ApiError::new(
                ErrorCode::HarnessUnsupported,
                "cannot create a detected agent",
            ));
        }
        validate_name_role(&args.name, &args.role)?;
        let slug = slug(&args.name)?;
        self.check_name(&args.name, None)?;
        let chosen = args.folder.is_some();
        let folder_path = match args.folder {
            Some(path) if path.is_absolute() => path,
            Some(path) => self.parent.join(path),
            None => self.parent.join(&slug),
        };
        // A folder the owner chose that already exists is adopted (FR-001).
        let adopted = if chosen {
            files::existing_folder(&folder_path)?
        } else {
            None
        };
        if let Some(existing) = &adopted {
            // A detected agent's folder is only its session's working folder; it does
            // not own it, so it does not block adopting the folder.
            let agents = self.store.agents()?;
            let owner = agents.iter().find(|agent| {
                agent.harness != HarnessKind::Detected
                    && agent.detected.is_none()
                    && (&agent.folder_path == existing
                        || agent.folder_path.canonicalize().ok().as_ref() == Some(existing))
            });
            if let Some(owner) = owner {
                // The app reads the owner's name from this exact wording
                // (`apps/desktop/src/views/new-agent/NewAgentSheet.tsx`).
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    format!("the folder is already {}'s folder", owner.name),
                ));
            }
        }
        let adopt = adopted.is_some();
        let folder_path = adopted.unwrap_or(folder_path);
        let repositories = absolute(&folder_path, args.repositories);
        let mut agent = Agent {
            id: AgentId::generate(),
            slug,
            name: args.name,
            role: args.role,
            harness: args.harness,
            folder_path,
            repositories,
            avatar: args.avatar,
            state: AgentState::Stopped,
            files_revision: String::new(),
            created_at: SystemTime::now().into(),
            last_activity_at: None,
            detected: None,
            running_requests: vec![],
        };
        let instructions = args
            .instructions
            .unwrap_or_else(|| format!("# {}\n\nYour role: {}\n", agent.name, agent.role));
        let folder = if adopt {
            files::adopt(&mut agent, &instructions)?
        } else {
            files::create(&mut agent, &instructions)?
        };
        if let Err(error) = self.store.insert_agent(&agent) {
            folder.rollback()?;
            return Err(error);
        }
        self.publish(&agent);
        Ok(agent)
    }

    fn update(&self, args: AgentUpdate) -> Result<Agent> {
        let mut agent = self.agent(args.agent_id)?;
        if let Some(name) = args.name {
            agent.name = name;
        }
        if let Some(role) = args.role {
            agent.role = role;
        }
        validate_name_role(&agent.name, &agent.role)?;
        self.check_name(&agent.name, Some(agent.id))?;
        if let Some(avatar) = args.avatar {
            agent.avatar = avatar;
        }
        if let Some(repositories) = args.repositories {
            agent.repositories = absolute(&agent.folder_path, repositories);
        }
        let path = files::editable_metadata(&agent.folder_path)?;
        let old = std::fs::read(&path).map_err(files::io_error)?;
        files::replace(&path, files::metadata(&agent)?.as_bytes())?;
        let result = (|| {
            agent.files_revision = files::revision(&agent.folder_path)?;
            self.store.update_agent(&agent)
        })();
        if let Err(error) = result {
            files::replace(&path, &old)?;
            return Err(error);
        }
        self.publish(&agent);
        Ok(agent)
    }

    fn write(&self, args: AgentFilesWrite) -> Result<AgentFiles> {
        let mut agent = self.agent(args.agent_id)?;
        let relative = files::validate_path(&args.path, args.content.is_none())?;
        files::safe_path(&agent.folder_path, &relative)?;
        self.refresh(&mut agent)?;
        if agent.files_revision != args.base_revision {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "agent files changed on disk; reload before saving",
            ));
        }
        let path = agent.folder_path.join(&relative);
        let old = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(files::io_error(e)),
        };
        match &args.content {
            Some(content) => {
                std::fs::create_dir_all(path.parent().expect("validated file path"))
                    .map_err(files::io_error)?;
                files::replace(&path, content.as_bytes())?;
            }
            None => std::fs::remove_file(&path).map_err(files::io_error)?,
        }
        let result = (|| {
            let result = files::read(&agent)?;
            agent.files_revision = result.revision.clone();
            self.store.update_agent(&agent)?;
            Ok(result)
        })();
        match result {
            Ok(result) => {
                self.publish(&agent);
                Ok(result)
            }
            Err(error) => {
                match old {
                    Some(bytes) => files::replace(&path, &bytes)?,
                    None => std::fs::remove_file(&path).map_err(files::io_error)?,
                }
                Err(error)
            }
        }
    }
}

/// Repository paths as stored: a relative one is taken from the agent folder,
/// as the read policy reads it.
fn absolute(folder: &Path, repositories: Vec<PathBuf>) -> Vec<PathBuf> {
    repositories
        .into_iter()
        .map(|repository| folder.join(repository))
        .collect()
}

fn validate_name_role(name: &str, role: &str) -> Result<()> {
    if name.trim().is_empty() || role.trim().is_empty() {
        Err(ApiError::new(
            ErrorCode::InvalidParams,
            "name and role are required",
        ))
    } else {
        Ok(())
    }
}

fn slug(name: &str) -> Result<String> {
    let normalized = name.nfkc().collect::<String>().to_lowercase();
    let slug = normalized
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        return Err(ApiError::new(
            ErrorCode::InvalidParams,
            "name needs a letter or number",
        ));
    }
    Ok(slug)
}
