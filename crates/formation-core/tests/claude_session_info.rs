#![cfg(feature = "testkit")]

use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::{AgentRepo, CommandCall, CommandHandler, Harness, SessionRepo},
    testkit::{FakeHarness, MemoryStore, RecordingEventSink, ScriptedAgent, ScriptedPage},
};
use formation_proto::{agent::*, common::*, harness::*, policy::Caller};
use serde_json::json;
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::SystemTime};

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn real_harness_exposes_durable_session_and_conversation_like_fake() {
    let root =
        TempDir(std::env::temp_dir().join(format!("formation-session-{}", RunId::generate())));
    fs::create_dir_all(&root.0).unwrap();
    let agent = Agent {
        id: AgentId::generate(),
        slug: "backend".into(),
        name: "Backend".into(),
        role: "developer".into(),
        harness: HarnessKind::ClaudeCode,
        folder_path: root.0.clone(),
        repositories: vec![],
        avatar: Avatar {
            shape: "circle".into(),
            color: "blue".into(),
        },
        state: AgentState::Stopped,
        files_revision: "1".into(),
        created_at: SystemTime::now().into(),
        last_activity_at: None,
        detected: None,
        running_requests: vec![],
    };
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let config_dir = root.0.join("config");
    let supervisor = ClaudeSupervisor::new(
        ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: config_dir.clone(),
            env: HashMap::from([(
                "FAKE_CLAUDE_TRANSCRIPT".into(),
                format!(
                    "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                    env!("CARGO_MANIFEST_DIR")
                ),
            )]),
            ..ClaudeConfig::default()
        },
        store.clone(),
        Arc::new(RecordingEventSink::default()),
    );
    supervisor
        .handle(CommandCall {
            caller: Caller::Owner,
            method: "agent_start".into(),
            params: json!({"agent_id": agent.id}),
        })
        .await
        .unwrap();
    let started = store.main_session(agent.id).unwrap().unwrap();

    // fake-claude only emits stdout. Supply the durable record separately,
    // in a project directory discovered after start (e.g. a CLI override).
    let project = config_dir.join("projects/custom-project");
    fs::create_dir_all(&project).unwrap();
    let path = project.join(format!("{}.jsonl", started.session_id.0));
    assert_ne!(started.transcript_path, path);
    let at: Timestamp = "2026-11-09T00:00:00Z".parse().unwrap();
    let transcript = format!(
        "{}\n",
        json!({
            "type": "assistant", "uuid": "answer", "timestamp": at,
            "sessionId": started.session_id,
            "message": {"id": "answer", "content": [{"type": "text", "text": "Login cleanup is done."}]}
        })
    );
    fs::write(&path, &transcript).unwrap();
    let expected = SessionInfo {
        transcript_path: path.clone(),
        ..started
    };
    let page = ConversationPageResult {
        items: vec![ConversationItem::AssistantText {
            id: ItemId("answer:0".into()),
            at,
            text: "Login cleanup is done.".into(),
        }],
        has_more: false,
        stream_seq: 0,
    };
    let mut scripted = ScriptedAgent::new(expected.clone(), []);
    scripted.pages.push(ScriptedPage {
        session_id: Some(expected.session_id.clone()),
        before: None,
        limit: 10,
        page: page.clone(),
    });
    let fake = FakeHarness::new(
        HarnessKind::ClaudeCode,
        supervisor.status().await,
        [scripted],
    );
    for harness in [&supervisor as &dyn Harness, &fake as &dyn Harness] {
        let info = harness.session_info(agent.id).await.unwrap().unwrap();
        assert_eq!(info, expected);
        assert_eq!(
            fs::read_to_string(&info.transcript_path).unwrap(),
            transcript
        );
        assert_eq!(
            harness
                .conversation(agent.id, Some(info.session_id), None, 10)
                .await
                .unwrap(),
            page
        );
    }
    supervisor.stop(agent.id, StopMode::Now).await.unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), transcript);
}
