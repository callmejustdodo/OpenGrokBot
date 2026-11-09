use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use formation_core::{
    ask::transcript::project_dir_name,
    harness::detect::{Detect, DetectPaths},
    ports::{AgentRepo, CommandCall, CommandHandler},
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{
    agent::*,
    common::{AgentId, Timestamp},
    events::Event,
    policy::Caller,
};
use serde_json::{Value, json};

struct Fixture {
    root: PathBuf,
    handler: Detect,
    store: Arc<MemoryStore>,
    events: Arc<RecordingEventSink>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("formation-detect-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = Arc::new(MemoryStore::default());
        let events = Arc::new(RecordingEventSink::default());
        let handler = Detect::new(
            DetectPaths {
                claude_config: root.join("claude"),
                codex_sessions: root.join("codex/sessions"),
                hermes: vec![root.join("hermes")],
                openclaw: vec![root.join("openclaw")],
            },
            store.clone(),
            events.clone(),
        );
        Self {
            root,
            handler,
            store,
            events,
        }
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        self.handler
            .handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            })
            .await
            .unwrap()
    }
    async fn detected(&self) -> Vec<DetectedAgent> {
        serde_json::from_value(self.call("agents_detect", json!({})).await).unwrap()
    }
    fn transcript(&self, project: &str, sid: &str, at: Timestamp) -> PathBuf {
        let cwd = self.root.join(project);
        fs::create_dir_all(&cwd).unwrap();
        let folder = self
            .root
            .join("claude/projects")
            .join(project_dir_name(&cwd.canonicalize().unwrap()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{sid}.jsonl"));
        fs::write(&path, format!("{}\n", json!({"type":"user", "sessionId":sid, "cwd":cwd, "timestamp":at, "message":{"role":"user","content":"hello"}}))).unwrap();
        path
    }
    fn rollout(&self, project: &str, sid: &str, at: Timestamp) -> PathBuf {
        let cwd = self.root.join(project);
        fs::create_dir_all(&cwd).unwrap();
        let folder = self.root.join("codex/sessions/2026/11/09");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("rollout-2026-11-09T14-12-39-{sid}.jsonl"));
        let meta = json!({"timestamp":at, "type":"session_meta", "payload":{"id":sid, "cwd":cwd, "timestamp":at}});
        let turn = json!({"timestamp":at, "type":"event_msg", "payload":{"type":"task_started"}});
        fs::write(&path, format!("{meta}\n{turn}\n")).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn detects_recent_external_sessions_skips_owned_projects_and_connects_read_only() {
    let f = Fixture::new();
    let now: Timestamp = SystemTime::now().into();
    let original = f.transcript("external", "external-session", now);
    f.transcript(
        "old",
        "old-session",
        (SystemTime::now() - Duration::from_secs(172800)).into(),
    );
    f.transcript("owned", "main-session", now);
    f.transcript("owned", "fork-session", now);
    f.store
        .insert_agent(&Agent {
            id: AgentId::generate(),
            slug: "owned".into(),
            name: "Owned".into(),
            role: String::new(),
            harness: HarnessKind::ClaudeCode,
            folder_path: f.root.join("owned"),
            repositories: vec![],
            avatar: Avatar {
                shape: "round".into(),
                color: "blue".into(),
            },
            state: AgentState::Idle,
            files_revision: String::new(),
            created_at: now,
            last_activity_at: None,
            detected: None,
            running_requests: vec![],
        })
        .unwrap();
    let before = fs::read(&original).unwrap();
    let found = f.detected().await;
    assert_eq!(found.len(), 1);
    let d = &found[0];
    assert_eq!(d.path, original);
    assert_eq!(d.session_id.as_ref().unwrap().0, "external-session");
    assert_eq!(d.last_activity_at, Some(now));
    assert!(!d.capabilities.state.supported);
    assert!(d.capabilities.read_conversation.supported);
    assert!(d.capabilities.answer_teammates.supported);
    assert!(!d.capabilities.receive_requests.supported);
    let connected: Agent =
        serde_json::from_value(f.call("agent_connect", json!({"key":d.key})).await).unwrap();
    assert_eq!(connected.harness, HarnessKind::Detected);
    assert_eq!(connected.state, AgentState::Unknown);
    assert_eq!(connected.folder_path, f.root.join("external"));
    assert_eq!(connected.detected.as_ref().unwrap().path, original);
    assert_eq!(
        f.store.agent(connected.id).unwrap(),
        Some(connected.clone())
    );
    // The reason the state is unknown is readable from the snapshot and from `agent.state`.
    let reason = connected
        .detected
        .as_ref()
        .unwrap()
        .capabilities
        .state
        .reason
        .clone();
    assert!(reason.as_ref().is_some_and(|s| !s.is_empty()));
    let events = f.events.events();
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0].1, Event::AgentUpdate(AgentUpdateEvent::Upsert { agent }) if **agent == connected)
    );
    assert!(
        matches!(&events[1].1, Event::AgentState(s) if s.agent_id == connected.id && s.state == AgentState::Unknown && s.reason == reason)
    );
    // Move the source into another project directory: exclusion must still use its session ID.
    let other = original
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("another-project");
    fs::create_dir(&other).unwrap();
    fs::copy(&original, other.join("external-session.jsonl")).unwrap();
    assert!(f.detected().await.is_empty());
    assert_eq!(fs::read(original).unwrap(), before);
    assert_eq!(fs::read_dir(f.root.join("external")).unwrap().count(), 0);
}

#[tokio::test]
async fn reports_only_present_installations_with_capability_reasons() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    assert!(f.detected().await.is_empty());
    fs::create_dir(f.root.join("hermes")).unwrap();
    fs::write(f.root.join("openclaw"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(f.root.join("openclaw"), fs::Permissions::from_mode(0o755)).unwrap();
    let found = f.detected().await;
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].harness, DetectedHarness::Hermes);
    assert_eq!(found[1].harness, DetectedHarness::OpenClaw);
    for d in found {
        for capability in [
            &d.capabilities.state,
            &d.capabilities.read_conversation,
            &d.capabilities.answer_teammates,
            &d.capabilities.receive_requests,
        ] {
            assert!(!capability.supported);
            assert!(capability.reason.as_ref().is_some_and(|s| !s.is_empty()));
        }
        let agent: Agent =
            serde_json::from_value(f.call("agent_connect", json!({"key":d.key})).await).unwrap();
        assert_eq!(agent.detected.as_ref().unwrap().harness, d.harness);
        let again: Agent =
            serde_json::from_value(f.call("agent_connect", json!({"key":d.key})).await).unwrap();
        assert_eq!(again.id, agent.id);
    }
    assert_eq!(f.events.events().len(), 4);
    assert!(
        f.detected()
            .await
            .iter()
            .all(|d| d.connected_agent.is_some())
    );
    assert_eq!(f.handler.methods(), vec!["agents_detect", "agent_connect"]);
}

#[tokio::test]
async fn detects_recent_codex_sessions_skips_known_ones_and_connects_read_only() {
    let f = Fixture::new();
    let now: Timestamp = SystemTime::now().into();
    let original = f.rollout("codex-external", "codex-session", now);
    f.rollout(
        "codex-old",
        "old-session",
        (SystemTime::now() - Duration::from_secs(172800)).into(),
    );
    f.rollout("owned", "owned-session", now);
    f.store
        .insert_agent(&Agent {
            id: AgentId::generate(),
            slug: "owned".into(),
            name: "Owned".into(),
            role: String::new(),
            harness: HarnessKind::ClaudeCode,
            folder_path: f.root.join("owned"),
            repositories: vec![],
            avatar: Avatar {
                shape: "round".into(),
                color: "blue".into(),
            },
            state: AgentState::Idle,
            files_revision: String::new(),
            created_at: now,
            last_activity_at: None,
            detected: None,
            running_requests: vec![],
        })
        .unwrap();
    let before = fs::read(&original).unwrap();
    let found = f.detected().await;
    assert_eq!(found.len(), 1);
    let d = &found[0];
    assert_eq!(d.key, "codex:codex-session");
    assert_eq!(d.harness, DetectedHarness::Codex);
    assert_eq!(d.name, "codex-external");
    assert_eq!(d.path, original);
    assert_eq!(d.session_id.as_ref().unwrap().0, "codex-session");
    assert_eq!(d.last_activity_at, Some(now));
    assert!(d.capabilities.read_conversation.supported);
    assert!(!d.capabilities.answer_teammates.supported);
    assert!(!d.capabilities.state.supported);
    let connected: Agent =
        serde_json::from_value(f.call("agent_connect", json!({"key":d.key})).await).unwrap();
    assert_eq!(connected.harness, HarnessKind::Detected);
    assert_eq!(connected.state, AgentState::Unknown);
    assert_eq!(connected.folder_path, f.root.join("codex-external"));
    let source = connected.detected.as_ref().unwrap();
    assert_eq!(source.harness, DetectedHarness::Codex);
    assert_eq!(source.path, original);
    assert_eq!(source.capabilities, d.capabilities);
    assert_eq!(
        f.store.agent(connected.id).unwrap(),
        Some(connected.clone())
    );
    // A copy of the connected session elsewhere is still recognised by its session ID.
    let other = f.root.join("codex/sessions/2026/11/08");
    fs::create_dir_all(&other).unwrap();
    fs::copy(
        &original,
        other.join("rollout-2026-11-08T10-00-00-codex-session.jsonl"),
    )
    .unwrap();
    assert!(f.detected().await.is_empty());
    assert_eq!(fs::read(original).unwrap(), before);
    assert_eq!(
        fs::read_dir(f.root.join("codex-external")).unwrap().count(),
        0
    );
}
