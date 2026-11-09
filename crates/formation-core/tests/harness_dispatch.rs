use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use formation_core::{harness::dispatch::HarnessDispatcher, ports::*, testkit::*};
use formation_proto::{
    agent::*,
    common::*,
    harness::{AgentStreamEvent, SessionInfo, StreamDelta},
    policy::Caller,
};
use serde_json::{Value, json};
use tokio::time::{Duration, timeout};

fn time() -> Timestamp {
    "2026-11-09T00:00:00Z".parse().unwrap()
}

fn agent(slug: &str, harness: HarnessKind) -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: slug.into(),
        name: slug.into(),
        role: "developer".into(),
        harness,
        folder_path: "/tmp/agent".into(),
        repositories: vec![],
        avatar: Avatar {
            shape: "circle".into(),
            color: "blue".into(),
        },
        state: AgentState::Idle,
        files_revision: "1".into(),
        created_at: time(),
        last_activity_at: None,
        detected: None,
        running_requests: vec![],
    }
}

fn status(kind: HarnessKind) -> HarnessStatus {
    HarnessStatus {
        harness: kind,
        installed: true,
        signed_in: Some(true),
        version: Some(format!("{kind:?}")),
        path: None,
        problem: None,
    }
}

fn fake(kind: HarnessKind, agent: &Agent) -> Arc<FakeHarness> {
    let session = SessionInfo {
        agent_id: agent.id,
        session_id: SessionId(agent.slug.clone()),
        transcript_path: "/tmp/session.jsonl".into(),
        cwd: "/tmp/agent".into(),
        started_at: time(),
        last_activity_at: None,
    };
    Arc::new(FakeHarness::new(
        kind,
        status(kind),
        [ScriptedAgent::new(session, [])],
    ))
}

/// Records which agents' commands reached it.
#[derive(Default)]
struct Commands(Mutex<Vec<Value>>);

#[async_trait]
impl CommandHandler for Commands {
    fn methods(&self) -> Vec<&'static str> {
        vec!["agent_turn_interrupt"]
    }
    async fn handle(&self, call: CommandCall) -> Result<Value> {
        self.0.lock().unwrap().push(call.params["agent_id"].clone());
        Ok(Value::Null)
    }
}

fn interrupt(agent: &Agent) -> CommandCall {
    CommandCall {
        caller: Caller::Owner,
        method: "agent_turn_interrupt".into(),
        params: json!({ "agent_id": agent.id }),
    }
}

#[tokio::test]
async fn routes_each_agent_to_its_harness() {
    let store = Arc::new(MemoryStore::default());
    let claude_agent = agent("claude", HarnessKind::ClaudeCode);
    let codex_agent = agent("codex", HarnessKind::Codex);
    store.insert_agent(&claude_agent).unwrap();
    store.insert_agent(&codex_agent).unwrap();
    let claude = fake(HarnessKind::ClaudeCode, &claude_agent);
    let codex = fake(HarnessKind::Codex, &codex_agent);
    let claude_commands = Arc::new(Commands::default());
    let codex_commands = Arc::new(Commands::default());
    let dispatcher = HarnessDispatcher::new(
        store.clone(),
        vec![
            (claude.clone(), claude_commands.clone()),
            (codex.clone(), codex_commands.clone()),
        ],
    );
    let mut stream = dispatcher.subscribe();

    dispatcher.interrupt(claude_agent.id).await.unwrap();
    dispatcher.interrupt(codex_agent.id).await.unwrap();
    assert_eq!(
        claude.calls(),
        [HarnessCall::Interrupt {
            agent_id: claude_agent.id
        }]
    );
    assert_eq!(
        codex.calls(),
        [HarnessCall::Interrupt {
            agent_id: codex_agent.id
        }]
    );
    let session = dispatcher.session_info(codex_agent.id).await.unwrap();
    assert_eq!(session.unwrap().session_id, SessionId("codex".into()));

    assert_eq!(dispatcher.methods(), ["agent_turn_interrupt"]);
    dispatcher.handle(interrupt(&codex_agent)).await.unwrap();
    dispatcher.handle(interrupt(&claude_agent)).await.unwrap();
    assert_eq!(*claude_commands.0.lock().unwrap(), [json!(claude_agent.id)]);
    assert_eq!(*codex_commands.0.lock().unwrap(), [json!(codex_agent.id)]);

    let statuses = dispatcher.statuses().await;
    assert_eq!(
        statuses,
        [status(HarnessKind::ClaudeCode), status(HarnessKind::Codex)]
    );

    let event = AgentStreamEvent {
        agent_id: codex_agent.id,
        session_id: SessionId("codex".into()),
        stream_seq: 1,
        delta: StreamDelta::TurnStarted {
            message_id: MessageId::generate(),
        },
    };
    codex.sender().send(event.clone()).unwrap();
    let merged = timeout(Duration::from_secs(5), stream.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(merged, event);
}

#[tokio::test]
async fn agent_without_its_harness_is_unsupported() {
    let store = Arc::new(MemoryStore::default());
    let claude_agent = agent("claude", HarnessKind::ClaudeCode);
    let codex_agent = agent("codex", HarnessKind::Codex);
    store.insert_agent(&claude_agent).unwrap();
    store.insert_agent(&codex_agent).unwrap();
    let claude = fake(HarnessKind::ClaudeCode, &claude_agent);
    let commands = Arc::new(Commands::default());
    let dispatcher =
        HarnessDispatcher::new(store.clone(), vec![(claude.clone(), commands.clone())]);

    let error = dispatcher.interrupt(codex_agent.id).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::HarnessUnsupported);
    let error = dispatcher.start(&codex_agent, None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::HarnessUnsupported);
    let error = dispatcher
        .handle(interrupt(&codex_agent))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::HarnessUnsupported);
    assert!(claude.calls().is_empty());
    assert!(commands.0.lock().unwrap().is_empty());

    let error = dispatcher.interrupt(AgentId::generate()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}
