use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use formation_proto::agent::HarnessStatus;
use formation_proto::agent::{Agent, AgentState, Avatar, HarnessKind};
use formation_proto::common::{
    AgentId, MemberId, MessageId, RequestId, RunId, SessionId, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::policy::{
    ApprovalChoice, ApprovalDecide, Caller, PermissionPrompt, PermissionSource, PermissionVerdict,
    PolicyVerdict, ToolCall,
};
use formation_proto::request::{QueueItem, QueueSource, RequestStatus};
use formation_proto::team::MemberRole;

use super::*;
use crate::notify::AttentionNotifier;
use crate::policy::approvals::SystemClock;
use crate::ports::{AgentRepo, CommandCall, RequestRepo, RunInfo};
use crate::service::EventBus;
use crate::testkit::collab::{MemoryCollabStore, PolicyCall, samples};
use crate::testkit::{FakeHarness, FakePolicy, MemoryStore};

struct World {
    gate: Arc<Gate>,
    commands: PermissionCommands,
    harness: Arc<FakeHarness>,
    collab: Arc<MemoryCollabStore>,
    policy: Arc<FakePolicy>,
    bus: Arc<EventBus>,
    agent: Agent,
}

fn agent() -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: "api".into(),
        name: "api".into(),
        role: "Backend".into(),
        harness: HarnessKind::ClaudeCode,
        folder_path: "/agents/api".into(),
        repositories: vec!["/code/api".into()],
        avatar: Avatar {
            shape: "circle".into(),
            color: "blue".into(),
        },
        state: AgentState::Working,
        files_revision: "1".into(),
        created_at: Timestamp::default(),
        last_activity_at: None,
        detected: None,
        running_requests: vec![],
    }
}

fn world() -> World {
    let store = Arc::new(MemoryStore::default());
    let agent = agent();
    store.insert_agent(&agent).unwrap();
    let collab = Arc::new(MemoryCollabStore::new());
    let harness = Arc::new(FakeHarness::new(
        HarnessKind::ClaudeCode,
        HarnessStatus {
            harness: HarnessKind::ClaudeCode,
            installed: true,
            signed_in: Some(true),
            version: None,
            path: None,
            problem: None,
        },
        [],
    ));
    let policy = Arc::new(FakePolicy::new());
    let bus = Arc::new(EventBus::new(64));
    let notifier = Arc::new(AttentionNotifier::new(store.clone(), bus.clone()));
    let approvals = Arc::new(Approvals::new(
        collab.clone(),
        bus.clone(),
        notifier,
        Arc::new(SystemClock),
    ));
    let gate = Arc::new(Gate::new(
        harness.clone(),
        store,
        collab.clone(),
        policy.clone(),
        approvals.clone(),
    ));
    World {
        commands: PermissionCommands::new(gate.clone(), approvals),
        gate,
        harness,
        collab,
        policy,
        bus,
        agent,
    }
}

impl World {
    fn run(&self, source: QueueSource) -> RunId {
        let run_id = RunId::generate();
        self.harness.set_run(RunInfo {
            run_id,
            agent_id: self.agent.id,
            session_id: SessionId("session".into()),
            item: QueueItem {
                id: MessageId::generate(),
                agent_id: self.agent.id,
                source,
                text: "work".into(),
                queued_at: Timestamp::default(),
            },
        });
        run_id
    }

    /// A teammate's request fork, with its stored request.
    fn request_run(&self) -> (RunId, RequestId, MemberId) {
        let requester = samples::member("minsu", MemberRole::Member).id;
        let mut request = samples::work_request(
            requester,
            MemberId::generate(),
            self.agent.id,
            "add tests",
            "k1",
        );
        request.status = RequestStatus::Running;
        request.requester_agent = Some("web".into());
        self.collab.insert_request(&request).unwrap();
        let run_id = self.run(QueueSource::Request {
            request_id: request.id,
            requester,
        });
        (run_id, request.id, requester)
    }

    fn prompt(&self, run_id: RunId, source: PermissionSource, call: &ToolCall) -> PermissionPrompt {
        PermissionPrompt {
            agent_id: self.agent.id,
            run_id,
            session_id: None,
            source,
            call: call.clone(),
        }
    }

    /// The hook prompt first, then the permission tool prompt, as Claude Code sends them.
    async fn both(&self, run_id: RunId, call: &ToolCall) -> (PermissionVerdict, PermissionVerdict) {
        let hook = self
            .gate
            .decide_prompt(self.prompt(run_id, PermissionSource::PreToolUseHook, call))
            .await
            .unwrap();
        let tool = self
            .gate
            .decide_prompt(self.prompt(run_id, PermissionSource::PermissionTool, call))
            .await
            .unwrap();
        (hook, tool)
    }

    async fn decide_next(&self, choice: ApprovalChoice) -> formation_proto::policy::Approval {
        let mut events = self.bus.subscribe();
        let approval = loop {
            let envelope = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("an approval is raised")
                .unwrap();
            if let Event::ApprovalRaised(event) = envelope.event {
                break event.approval;
            }
        };
        let result = self
            .commands
            .handle(CommandCall {
                caller: Caller::Owner,
                method: "approval_decide".into(),
                params: serde_json::to_value(ApprovalDecide {
                    approval_id: approval.id,
                    choice,
                    rule: None,
                })
                .unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(result["id"], json!(approval.id));
        approval
    }
}

fn is_deny(verdict: &PermissionVerdict) -> bool {
    matches!(verdict, PermissionVerdict::Deny { .. })
}

#[tokio::test]
async fn allowed_reads_run_without_asking_and_the_hook_defers() {
    let w = world();
    let run = w.run(QueueSource::Owner);
    let call = samples::tool_call("Read", json!({"file_path": "/agents/api/a.txt"}));
    let (hook, tool) = w.both(run, &call).await;
    assert_eq!(hook, PermissionVerdict::Defer);
    assert_eq!(
        tool,
        PermissionVerdict::Allow {
            updated_input: call.input.clone()
        }
    );
}

#[tokio::test]
async fn the_owners_own_prompt_becomes_an_approval_card_and_returns_the_answer() {
    let w = Arc::new(world());
    let run = w.run(QueueSource::Owner);
    let call = samples::bash("npm test");
    let hook = w
        .gate
        .decide_prompt(w.prompt(run, PermissionSource::PreToolUseHook, &call))
        .await
        .unwrap();
    assert_eq!(
        hook,
        PermissionVerdict::Defer,
        "the hook never skips the card"
    );
    let owner = {
        let w = w.clone();
        tokio::spawn(async move { w.decide_next(ApprovalChoice::AllowOnce).await })
    };
    tokio::task::yield_now().await;
    let verdict = w
        .gate
        .decide_prompt(w.prompt(run, PermissionSource::PermissionTool, &call))
        .await
        .unwrap();
    let approval = owner.await.unwrap();
    assert_eq!(approval.request_id, None, "the owner's own work");
    assert_eq!(approval.call, call);
    assert_eq!(
        verdict,
        PermissionVerdict::Allow {
            updated_input: call.input.clone()
        }
    );
}

#[tokio::test]
async fn a_teammates_command_waits_for_the_owner_and_a_deny_stops_it() {
    let w = Arc::new(world());
    let (run, request_id, requester) = w.request_run();
    let call = samples::bash("rm -rf build");
    let owner = {
        let w = w.clone();
        tokio::spawn(async move { w.decide_next(ApprovalChoice::Deny).await })
    };
    tokio::task::yield_now().await;
    let (hook, tool) = w.both(run, &call).await;
    let approval = owner.await.unwrap();
    assert_eq!(hook, PermissionVerdict::Defer);
    assert!(is_deny(&tool), "{tool:?}");
    assert_eq!(approval.request_id, Some(request_id));
    assert_eq!(approval.requester, Some(requester));
    // The policy saw the teammate as the caller, with the request's chain.
    let contexts: Vec<_> = w
        .policy
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            PolicyCall::Evaluate { context, .. } => Some(context),
            _ => None,
        })
        .collect();
    assert!(contexts.iter().all(|context| matches!(
        &context.caller,
        Caller::Member { member_id, via_agent, .. }
            if *member_id == requester && via_agent.as_deref() == Some("web")
    )));
    assert!(
        contexts
            .iter()
            .all(|context| context.request_id == Some(request_id)
                && context.folder == w.agent.folder_path
                && context.repositories == w.agent.repositories)
    );
    // Deny wins: the same tool call is never asked again.
    let again = w
        .gate
        .decide_prompt(w.prompt(run, PermissionSource::PermissionTool, &call))
        .await
        .unwrap();
    assert!(is_deny(&again));
}

#[tokio::test]
async fn a_hook_deny_wins_over_the_permission_tool() {
    let w = world();
    let run = w.run(QueueSource::Owner);
    let call = samples::bash("curl https://example.com");
    w.policy.push_verdict(PolicyVerdict::Deny {
        reason: "not allowed".into(),
    });
    w.policy.set_default_verdict(PolicyVerdict::Allow {
        reason: formation_proto::policy::AllowReason::AlwaysAllowedRead,
    });
    let (hook, tool) = w.both(run, &call).await;
    assert_eq!(
        hook,
        PermissionVerdict::Deny {
            message: "not allowed".into()
        }
    );
    assert!(is_deny(&tool), "the later allow does not undo the deny");
}

#[tokio::test]
async fn unknown_runs_and_mismatched_agents_are_denied() {
    let w = world();
    let call = samples::bash("npm test");
    let unknown = w
        .gate
        .decide_prompt(w.prompt(RunId::generate(), PermissionSource::PermissionTool, &call))
        .await
        .unwrap();
    assert!(is_deny(&unknown));
    let run = w.run(QueueSource::Owner);
    let mut prompt = w.prompt(run, PermissionSource::PreToolUseHook, &call);
    prompt.agent_id = AgentId::generate();
    assert!(is_deny(&w.gate.decide_prompt(prompt).await.unwrap()));
    assert!(
        w.policy.calls().is_empty(),
        "no policy call for a bad prompt"
    );
}

#[tokio::test]
async fn a_request_run_without_its_request_is_denied() {
    let w = world();
    let run = w.run(QueueSource::Request {
        request_id: RequestId::generate(),
        requester: MemberId::generate(),
    });
    let call = samples::tool_call("Write", json!({"file_path": "a.txt", "content": "x"}));
    let (hook, tool) = w.both(run, &call).await;
    assert!(is_deny(&hook) && is_deny(&tool));
}

#[tokio::test]
async fn permission_decide_is_served_on_the_socket() {
    let w = world();
    let run = w.run(QueueSource::Owner);
    let call = samples::tool_call("Edit", json!({"file_path": "a.txt"}));
    let result = w
        .commands
        .handle(CommandCall {
            caller: Caller::Owner,
            method: "permission_decide".into(),
            params: json!({"prompt": w.prompt(run, PermissionSource::PreToolUseHook, &call)}),
        })
        .await
        .unwrap();
    assert_eq!(result, json!({"behavior": "defer"}));
    let bad = w
        .commands
        .handle(CommandCall {
            caller: Caller::Owner,
            method: "permission_decide".into(),
            params: json!({"prompt": 1}),
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code, formation_proto::common::ErrorCode::InvalidParams);
}
