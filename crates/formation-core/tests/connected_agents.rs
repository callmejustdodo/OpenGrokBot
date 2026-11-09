#![cfg(feature = "testkit")]

//! Connected (detected) sessions open read-only and fork without touching the original (#194).

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::SystemTime};

use formation_core::{
    ask::transcript::project_dir_name,
    harness::{
        claude::{ClaudeConfig, ClaudeSupervisor},
        codex::{CodexConfig, CodexSupervisor},
        detect::{Detect, DetectPaths},
        dispatch::HarnessDispatcher,
    },
    ports::*,
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{agent::*, common::*, harness::*, policy::Caller, request::*};
use serde_json::{Value, json};

struct Fixture {
    root: PathBuf,
    detect: Detect,
    dispatcher: HarnessDispatcher,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("formation-connected-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let store = Arc::new(MemoryStore::default());
        let sink = Arc::new(RecordingEventSink::default());
        let detect = Detect::new(
            DetectPaths {
                claude_config: root.join("claude"),
                codex_sessions: root.join("codex/sessions"),
                hermes: vec![],
                openclaw: vec![],
            },
            store.clone(),
            sink.clone(),
        );
        let claude = Arc::new(ClaudeSupervisor::new(
            ClaudeConfig {
                binary: env!("CARGO_BIN_EXE_fake-claude").into(),
                config_dir: root.join("claude"),
                env: HashMap::from([
                    (
                        "FAKE_CLAUDE_TRANSCRIPT".into(),
                        format!(
                            "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                            env!("CARGO_MANIFEST_DIR")
                        ),
                    ),
                    (
                        "FAKE_CLAUDE_LAUNCH_LOG".into(),
                        root.join("launches").display().to_string(),
                    ),
                    ("FAKE_CLAUDE_WRITE_TRANSCRIPT".into(), "1".into()),
                ]),
                ..ClaudeConfig::default()
            },
            store.clone(),
            sink.clone(),
        ));
        let codex = Arc::new(CodexSupervisor::new(
            CodexConfig::default(),
            store.clone(),
            sink,
            None,
        ));
        let dispatcher = HarnessDispatcher::new(
            store,
            vec![(claude.clone(), claude), (codex.clone(), codex)],
        );
        Self {
            root,
            detect,
            dispatcher,
        }
    }

    fn now() -> String {
        Timestamp::from(SystemTime::now()).to_string()
    }

    /// An external Claude Code transcript: one user message and one reply.
    fn claude_transcript(&self, sid: &str) -> PathBuf {
        let cwd = self.root.join("claude-project");
        fs::create_dir_all(&cwd).unwrap();
        let folder = self
            .root
            .join("claude/projects")
            .join(project_dir_name(&cwd));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{sid}.jsonl"));
        let at = Self::now();
        let user = json!({"type":"user","uuid":"u1","sessionId":sid,"cwd":cwd,"timestamp":at,
            "message":{"role":"user","content":"what changed?"}});
        let reply = json!({"type":"assistant","uuid":"a1","sessionId":sid,"cwd":cwd,"timestamp":at,
            "message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"the parser"}]}});
        fs::write(&path, format!("{user}\n{reply}\n")).unwrap();
        path
    }

    /// An external Codex rollout in the format Codex 0.164 writes.
    fn codex_rollout(&self, sid: &str) -> PathBuf {
        let cwd = self.root.join("codex-project");
        fs::create_dir_all(&cwd).unwrap();
        let folder = self.root.join("codex/sessions/2026/11/09");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("rollout-2026-11-09T15-15-45-{sid}.jsonl"));
        let at = Self::now();
        let completed = |item: Value| json!({"timestamp":at,"type":"event_msg","payload":{"type":"item_completed","thread_id":sid,"item":item}});
        let records = [
            json!({"timestamp":at,"type":"session_meta","payload":{"id":sid,"cwd":cwd,"timestamp":at}}),
            json!({"timestamp":at,"type":"response_item","payload":{"type":"message","role":"user",
                "content":[{"type":"input_text","text":"# AGENTS.md instructions"}]}}),
            completed(json!({"type":"UserMessage","id":"user-1",
                "content":[{"type":"text","text":"run the tests","text_elements":[]}]})),
            completed(json!({"type":"Reasoning","id":"rs_1","summary_text":[],"raw_content":[]})),
            completed(json!({"type":"CommandExecution","id":"exec-1",
                "command":["/bin/zsh","-lc","cargo test"],"status":"failed",
                "aggregated_output":"1 failed","exit_code":1})),
            completed(
                json!({"type":"McpToolCall","id":"exec-2","server":"codex_apps",
                "tool":"github.fetch_commit","status":"completed"}),
            ),
            completed(
                json!({"type":"FileChange","id":"exec-3","status":"completed",
                "changes":{"/src/lib.rs":{"type":"update","unified_diff":"@@ -1 +1 @@"}}}),
            ),
            completed(json!({"type":"AgentMessage","id":"msg-1",
                "content":[{"type":"Text","text":"one test fails"}],"phase":"final_answer"})),
        ];
        let text: String = records.iter().map(|r| format!("{r}\n")).collect();
        fs::write(&path, text).unwrap();
        path
    }

    async fn call(
        &self,
        handler: &dyn CommandHandler,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        handler
            .handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            })
            .await
    }

    async fn connect(&self, key: &str) -> Agent {
        serde_json::from_value(
            self.call(&self.detect, "agent_connect", json!({"key": key}))
                .await
                .unwrap(),
        )
        .unwrap()
    }

    async fn page(&self, agent: &Agent) -> ConversationPageResult {
        serde_json::from_value(
            self.call(
                &self.dispatcher,
                "conversation_page",
                json!({"agent_id": agent.id, "limit": 50}),
            )
            .await
            .unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn texts(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            ConversationItem::UserMessage { text, .. } => format!("user: {text}"),
            ConversationItem::AssistantText { text, .. } => format!("assistant: {text}"),
            ConversationItem::Step { step, .. } => {
                format!(
                    "{:?} {} {} {:?}",
                    step.kind, step.tool_name, step.target, step.status
                )
            }
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn connected_claude_session_reads_its_transcript_and_forks_without_writing_it() {
    let f = Fixture::new();
    let original = f.claude_transcript("external-session");
    let before = fs::read(&original).unwrap();
    let agent = f.connect("claude_code:external-session").await;

    let page = f.page(&agent).await;
    assert_eq!(
        texts(&page.items),
        ["user: what changed?", "assistant: the parser"]
    );
    assert!(!page.has_more);

    let main = f.dispatcher.session_info(agent.id).await.unwrap().unwrap();
    assert_eq!(main.session_id.0, "external-session");
    assert_eq!(main.transcript_path, original);
    let run_id = RunId::generate();
    let fork = f
        .dispatcher
        .run_fork(
            agent.id,
            ForkRun {
                run_id,
                from: main.session_id.clone(),
                item: QueueItem {
                    id: MessageId::generate(),
                    agent_id: agent.id,
                    source: QueueSource::Owner,
                    text: "a teammate asks".into(),
                    queued_at: SystemTime::now().into(),
                },
                tools: ForkTools::None,
            },
        )
        .await
        .unwrap();
    assert_eq!(fork.result.outcome, TurnOutcome::Completed);
    assert_ne!(fork.session.session_id, main.session_id);
    assert_ne!(fork.session.transcript_path, original);
    assert_eq!(fs::read(&original).unwrap(), before);
    let launch: Value = serde_json::from_str(
        fs::read_to_string(f.root.join("launches"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let args = launch["args"].as_array().unwrap();
    assert!(args.contains(&Value::from("--fork-session")));
    assert!(
        args.windows(2)
            .any(|w| w == [Value::from("--resume"), Value::from("external-session")])
    );
}

#[tokio::test]
async fn connected_codex_session_reads_its_rollout() {
    let f = Fixture::new();
    let original = f.codex_rollout("01a1bef2-cd3f-7831-b052-15d7defd83ee");
    let before = fs::read(&original).unwrap();
    let detected: Vec<DetectedAgent> =
        serde_json::from_value(f.call(&f.detect, "agents_detect", json!({})).await.unwrap())
            .unwrap();
    assert!(detected[0].capabilities.read_conversation.supported);
    let agent = f.connect(&detected[0].key).await;

    let page = f.page(&agent).await;
    assert_eq!(
        texts(&page.items),
        [
            "user: run the tests",
            "Command Bash cargo test Failed",
            "Tool github.fetch_commit codex_apps Done",
            "Edit Edit /src/lib.rs Done",
            "assistant: one test fails",
        ]
    );
    let main = f.dispatcher.session_info(agent.id).await.unwrap().unwrap();
    assert_eq!(main.session_id.0, "01a1bef2-cd3f-7831-b052-15d7defd83ee");
    assert_eq!(fs::read(&original).unwrap(), before);
}

#[tokio::test]
async fn sending_to_a_connected_agent_is_refused() {
    let f = Fixture::new();
    f.claude_transcript("external-session");
    f.codex_rollout("codex-session");
    for key in ["claude_code:external-session", "codex:codex-session"] {
        let agent = f.connect(key).await;
        for (method, params) in [
            (
                "agent_message_send",
                json!({"agent_id": agent.id, "text": "hello"}),
            ),
            (
                "agent_message_steer",
                json!({"agent_id": agent.id, "message_id": MessageId::generate()}),
            ),
            ("agent_start", json!({"agent_id": agent.id})),
        ] {
            let error = f
                .call(&f.dispatcher, method, params)
                .await
                .expect_err(method);
            assert_eq!(error.code, ErrorCode::HarnessUnsupported, "{key} {method}");
        }
    }
    assert!(!f.root.join("launches").exists());
}
