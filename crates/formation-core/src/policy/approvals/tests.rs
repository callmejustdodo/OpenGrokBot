use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use formation_proto::agent::{Attention, AttentionKind, AttentionTarget};
use formation_proto::common::{
    AgentId, ApiError, ApprovalId, AttentionId, ChainId, ErrorCode, MemberId, MessageId, RequestId,
    RunId, Seq, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::policy::{
    AllowReason, Approval, ApprovalChoice, ApprovalDecide, ApprovalStatus, Caller, Chain,
    PermissionPrompt, PermissionSource, PermissionVerdict, PolicyVerdict, ToolCall,
};
use formation_proto::request::{QueueItem, RequestStatus, WorkRequest};

use super::*;
use crate::ports::{ApprovalRepo, EventSink, Notifier, PermissionRequest, RequestRepo, Result};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Approvals and work requests in memory.
#[derive(Default)]
struct MemoryStore {
    approvals: Mutex<HashMap<ApprovalId, Approval>>,
    requests: Mutex<HashMap<RequestId, WorkRequest>>,
}

impl MemoryStore {
    fn stored(&self, id: ApprovalId) -> Approval {
        self.approvals.lock().unwrap()[&id].clone()
    }

    fn stored_request(&self, id: RequestId) -> WorkRequest {
        self.requests.lock().unwrap()[&id].clone()
    }

    fn count(&self) -> usize {
        self.approvals.lock().unwrap().len()
    }
}

impl ApprovalRepo for MemoryStore {
    fn approval(&self, id: ApprovalId) -> Result<Option<Approval>> {
        Ok(self.approvals.lock().unwrap().get(&id).cloned())
    }

    fn approvals(&self, status: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        let mut approvals: Vec<Approval> = self
            .approvals
            .lock()
            .unwrap()
            .values()
            .filter(|a| status.is_none_or(|s| a.status == s))
            .cloned()
            .collect();
        approvals.sort_by_key(|a| a.id);
        Ok(approvals)
    }

    fn put_approval(&self, approval: &Approval) -> Result<()> {
        self.approvals
            .lock()
            .unwrap()
            .insert(approval.id, approval.clone());
        Ok(())
    }
}

impl RequestRepo for MemoryStore {
    fn insert_request(&self, request: &WorkRequest) -> Result<(WorkRequest, bool)> {
        self.put_request(request)?;
        Ok((request.clone(), true))
    }

    fn request(&self, id: RequestId) -> Result<Option<WorkRequest>> {
        Ok(self.requests.lock().unwrap().get(&id).cloned())
    }

    fn requests(&self) -> Result<Vec<WorkRequest>> {
        Ok(self.requests.lock().unwrap().values().cloned().collect())
    }

    fn put_request(&self, request: &WorkRequest) -> Result<()> {
        self.requests
            .lock()
            .unwrap()
            .insert(request.id, request.clone());
        Ok(())
    }

    fn queue_items(&self, _agent_id: AgentId) -> Result<Vec<QueueItem>> {
        Ok(Vec::new())
    }

    fn put_queue_item(&self, _item: &QueueItem) -> Result<()> {
        Ok(())
    }

    fn remove_queue_item(&self, _id: MessageId) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingEvents(Mutex<Vec<Event>>);

impl RecordingEvents {
    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

impl EventSink for RecordingEvents {
    fn publish(&self, event: Event) -> Seq {
        let mut events = self.0.lock().unwrap();
        events.push(event);
        Seq(events.len() as u64)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Notice {
    Raised(AttentionKind, AttentionTarget, String),
    Resolved(AttentionTarget),
}

#[derive(Default)]
struct RecordingNotifier(Mutex<Vec<Notice>>);

impl RecordingNotifier {
    fn take(&self) -> Vec<Notice> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn raise(
        &self,
        kind: AttentionKind,
        target: AttentionTarget,
        subject: String,
    ) -> Result<Attention> {
        self.0
            .lock()
            .unwrap()
            .push(Notice::Raised(kind, target, subject.clone()));
        Ok(Attention {
            id: AttentionId::generate(),
            kind,
            target,
            subject,
            at: start(),
            resolved_at: None,
        })
    }

    async fn resolve(&self, target: AttentionTarget) -> Result<()> {
        self.0.lock().unwrap().push(Notice::Resolved(target));
        Ok(())
    }

    async fn inbox(&self) -> Result<Vec<Attention>> {
        Ok(Vec::new())
    }
}

struct ManualClock(Mutex<Timestamp>);

impl ManualClock {
    fn set(&self, at: Timestamp) {
        *self.0.lock().unwrap() = at;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap()
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn start() -> Timestamp {
    "2026-11-09T09:00:00Z".parse().unwrap()
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

struct World {
    approvals: Arc<Approvals>,
    store: Arc<MemoryStore>,
    events: Arc<RecordingEvents>,
    notifier: Arc<RecordingNotifier>,
    clock: Arc<ManualClock>,
    agent_id: AgentId,
    member: MemberId,
}

impl World {
    fn new() -> Self {
        let store = Arc::new(MemoryStore::default());
        let events = Arc::new(RecordingEvents::default());
        let notifier = Arc::new(RecordingNotifier::default());
        let clock = Arc::new(ManualClock(Mutex::new(start())));
        let approvals = Arc::new(Approvals::new(
            store.clone(),
            events.clone(),
            notifier.clone(),
            clock.clone(),
        ));
        Self {
            approvals,
            store,
            events,
            notifier,
            clock,
            agent_id: AgentId::generate(),
            member: MemberId::generate(),
        }
    }

    fn teammate(&self) -> Caller {
        Caller::Member {
            member_id: self.member,
            via_agent: Some("web".into()),
            chain: Chain {
                id: ChainId::generate(),
                depth: 0,
            },
        }
    }

    /// A running work request from the teammate.
    fn request(&self, status: RequestStatus) -> RequestId {
        let request = WorkRequest {
            id: RequestId::generate(),
            requester: self.member,
            requester_agent: Some("web".into()),
            chain: Chain {
                id: ChainId::generate(),
                depth: 0,
            },
            owner: MemberId::generate(),
            agent_id: self.agent_id,
            prompt: "\n  Add token expiry tests  \nThen run them.".into(),
            idempotency_key: "key-1".into(),
            status,
            result: None,
            error: None,
            created_at: start(),
            updated_at: start(),
        };
        self.store.put_request(&request).unwrap();
        request.id
    }

    fn teammate_ticket(
        &self,
        run_id: RunId,
        request_id: RequestId,
        call: ToolCall,
    ) -> ApprovalTicket {
        ApprovalTicket {
            run_id,
            agent_id: self.agent_id,
            caller: self.teammate(),
            request_id: Some(request_id),
            call,
        }
    }

    fn owner_ticket(&self, run_id: RunId, call: ToolCall) -> ApprovalTicket {
        ApprovalTicket {
            run_id,
            agent_id: self.agent_id,
            caller: Caller::Owner,
            request_id: None,
            call,
        }
    }

    fn decide(&self, id: ApprovalId, choice: ApprovalChoice) -> Result<Approval> {
        block_on(self.approvals.decide(
            &Caller::Owner,
            &ApprovalDecide {
                approval_id: id,
                choice,
                rule: None,
            },
        ))
    }

    fn raise(&self, ticket: ApprovalTicket) -> Approval {
        block_on(self.approvals.raise(ticket)).unwrap()
    }
}

fn bash(tool_use_id: Option<&str>, command: &str) -> ToolCall {
    ToolCall {
        tool_use_id: tool_use_id.map(str::to_owned),
        tool_name: "Bash".into(),
        input: json!({"command": command, "description": "Run the tests"}),
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn request_statuses(events: &[Event]) -> Vec<RequestStatus> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::RequestUpdate(update) => Some(update.request.status),
            _ => None,
        })
        .collect()
}

fn approval_events(events: &[Event]) -> Vec<(&'static str, ApprovalStatus)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::ApprovalRaised(a) | Event::ApprovalUpdate(a) => {
                Some((e.name(), a.approval.status))
            }
            _ => None,
        })
        .collect()
}

fn code(result: Result<Approval>) -> ErrorCode {
    result.expect_err("expected an error").code
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn raising_shows_who_what_and_until_when_and_tells_everyone() {
    let world = World::new();
    let request_id = world.request(RequestStatus::Running);
    let run = RunId::generate();
    let approval =
        world.raise(world.teammate_ticket(run, request_id, bash(Some("toolu_1"), "npm test")));

    assert_eq!(approval.status, ApprovalStatus::Pending);
    assert_eq!(approval.agent_id, world.agent_id);
    assert_eq!(approval.request_id, Some(request_id));
    assert_eq!(approval.requester, Some(world.member));
    assert_eq!(approval.requester_agent.as_deref(), Some("web"));
    assert_eq!(approval.purpose.as_deref(), Some("Add token expiry tests"));
    assert_eq!(approval.summary, "npm test");
    assert_eq!(approval.proposed_rule, None);
    assert_eq!(approval.created_at, start());
    assert_eq!(approval.expires_at, start() + DAY);
    assert_eq!(world.store.stored(approval.id), approval);
    assert_eq!(world.approvals.pending_for_run(run), vec![approval.id]);

    let events = world.events.take();
    assert_eq!(
        approval_events(&events),
        vec![("approval.raised", ApprovalStatus::Pending)]
    );
    assert_eq!(
        request_statuses(&events),
        vec![RequestStatus::AwaitingApproval]
    );
    assert_eq!(
        world.store.stored_request(request_id).status,
        RequestStatus::AwaitingApproval
    );
    assert_eq!(
        world.notifier.take(),
        vec![Notice::Raised(
            AttentionKind::Approval,
            AttentionTarget::Approval {
                approval_id: approval.id
            },
            "npm test".into()
        )]
    );
}

#[test]
fn allow_once_answers_one_call_and_is_used_up() {
    let world = World::new();
    let request_id = world.request(RequestStatus::Running);
    let run = RunId::generate();
    let call = bash(Some("toolu_1"), "npm test");
    let approval = world.raise(world.teammate_ticket(run, request_id, call.clone()));
    world.events.take();
    world.notifier.take();

    world.clock.set(start() + Duration::from_secs(60));
    let decided = world
        .decide(approval.id, ApprovalChoice::AllowOnce)
        .unwrap();
    assert_eq!(decided.status, ApprovalStatus::Allowed);
    assert_eq!(decided.choice, Some(ApprovalChoice::AllowOnce));
    assert_eq!(decided.decided_at, Some(start() + Duration::from_secs(60)));
    assert_eq!(world.store.stored(approval.id), decided);

    let waited = block_on(world.approvals.wait(approval.id)).unwrap();
    assert_eq!(
        verdict(&waited),
        PermissionVerdict::Allow {
            updated_input: call.input.clone()
        }
    );
    let events = world.events.take();
    assert_eq!(
        approval_events(&events),
        vec![("approval.update", ApprovalStatus::Allowed)]
    );
    assert_eq!(request_statuses(&events), vec![RequestStatus::Running]);
    assert_eq!(
        world.notifier.take(),
        vec![Notice::Resolved(AttentionTarget::Approval {
            approval_id: approval.id
        })]
    );

    // The decision cannot be taken again.
    assert_eq!(
        code(world.decide(approval.id, ApprovalChoice::AllowOnce)),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(world.decide(approval.id, ApprovalChoice::Deny)),
        ErrorCode::Conflict
    );

    // The same call asked again, and an identical new call, each need a new approval.
    for tool_use_id in ["toolu_1", "toolu_2"] {
        let again = world.raise(world.teammate_ticket(
            run,
            request_id,
            bash(Some(tool_use_id), "npm test"),
        ));
        assert_ne!(again.id, approval.id);
        assert_eq!(again.status, ApprovalStatus::Pending);
    }
    assert_eq!(world.approvals.pending_for_run(run).len(), 2);
    assert_eq!(
        world.store.stored(approval.id).status,
        ApprovalStatus::Allowed
    );
}

#[test]
fn deny_blocks_the_call_and_ends_the_request() {
    let world = World::new();
    let request_id = world.request(RequestStatus::Running);
    let run = RunId::generate();
    let approval =
        world.raise(world.teammate_ticket(run, request_id, bash(Some("toolu_1"), "rm -rf build")));
    world.events.take();

    let decided = world.decide(approval.id, ApprovalChoice::Deny).unwrap();
    assert_eq!(decided.status, ApprovalStatus::Denied);
    assert_eq!(decided.choice, Some(ApprovalChoice::Deny));
    assert_eq!(
        verdict(&block_on(world.approvals.wait(approval.id)).unwrap()),
        PermissionVerdict::Deny {
            message: DENIED_MESSAGE.into()
        }
    );
    let request = world.store.stored_request(request_id);
    assert_eq!(request.status, RequestStatus::Denied);
    assert_eq!(request.error.map(|e| e.code), Some(ErrorCode::Denied));
    assert_eq!(
        request_statuses(&world.events.take()),
        vec![RequestStatus::Denied]
    );

    // Later calls of the ended request are denied without asking.
    let count = world.store.count();
    let answer = block_on(world.approvals.ask(world.teammate_ticket(
        run,
        request_id,
        bash(Some("toolu_2"), "ls"),
    )));
    assert!(
        matches!(answer, Ok(PermissionVerdict::Deny { .. })),
        "{answer:?}"
    );
    assert_eq!(world.store.count(), count);
    assert!(world.events.take().is_empty());
}

#[test]
fn an_approval_expires_at_exactly_24_hours() {
    // (time of the decision after raising, expired)
    let rows = [
        (DAY - Duration::from_secs(1), false),
        (DAY - Duration::from_nanos(1), false),
        (DAY, true),
        (DAY + Duration::from_secs(1), true),
    ];
    for (after, expired) in rows {
        let world = World::new();
        let request_id = world.request(RequestStatus::Running);
        let approval = world.raise(world.teammate_ticket(
            RunId::generate(),
            request_id,
            bash(Some("toolu_1"), "npm test"),
        ));
        world.events.take();
        world.clock.set(start() + after);

        let result = world.decide(approval.id, ApprovalChoice::AllowOnce);
        let stored = world.store.stored(approval.id);
        let request = world.store.stored_request(request_id);
        let events = world.events.take();
        if expired {
            assert_eq!(code(result), ErrorCode::ApprovalExpired, "{after:?}");
            assert_eq!(stored.status, ApprovalStatus::Expired);
            assert_eq!((stored.choice, stored.decided_at), (None, None));
            assert_eq!(request.status, RequestStatus::Expired);
            assert_eq!(
                request.error.map(|e| e.code),
                Some(ErrorCode::ApprovalExpired)
            );
            assert_eq!(
                approval_events(&events),
                vec![("approval.update", ApprovalStatus::Expired)]
            );
            assert_eq!(request_statuses(&events), vec![RequestStatus::Expired]);
            assert_eq!(
                verdict(&block_on(world.approvals.wait(approval.id)).unwrap()),
                PermissionVerdict::Deny {
                    message: EXPIRED_MESSAGE.into()
                }
            );
            assert_eq!(
                code(world.decide(approval.id, ApprovalChoice::Deny)),
                ErrorCode::ApprovalExpired
            );
        } else {
            assert_eq!(result.unwrap().status, ApprovalStatus::Allowed, "{after:?}");
            assert_eq!(request.status, RequestStatus::Running);
        }
    }
}

#[test]
fn a_restart_closes_approvals_left_by_the_previous_process() {
    let before = World::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let request_id = before.request(RequestStatus::Running);
    let left = before.raise(before.teammate_ticket(
        RunId::generate(),
        request_id,
        bash(Some("toolu_1"), "npm test"),
    ));
    // A new process over the same store knows none of the old runs.
    let after = Approvals::new(
        before.store.clone(),
        before.events.clone(),
        before.notifier.clone(),
        before.clock.clone(),
    );
    let closed = runtime.block_on(after.close_interrupted()).unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].id, left.id);
    assert_eq!(before.store.stored(left.id).status, ApprovalStatus::Denied);
    assert_eq!(after.next_expiry().unwrap(), None);
    // Approvals this process raised are left alone.
    let own = block_on(
        after.raise(before.owner_ticket(RunId::generate(), bash(Some("toolu_2"), "make"))),
    )
    .unwrap()
    .id;
    assert!(
        runtime
            .block_on(after.close_interrupted())
            .unwrap()
            .is_empty()
    );
    assert_eq!(before.store.stored(own).status, ApprovalStatus::Pending);
}

#[test]
fn expire_due_expires_what_is_due_and_wakes_the_waiting_call() {
    let world = World::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let request_id = world.request(RequestStatus::Running);
    let first = world.raise(world.teammate_ticket(
        RunId::generate(),
        request_id,
        bash(Some("toolu_1"), "npm test"),
    ));
    world.clock.set(start() + Duration::from_secs(3600));
    let second = world.raise(world.owner_ticket(RunId::generate(), bash(Some("toolu_9"), "make")));
    assert_eq!(world.approvals.next_expiry().unwrap(), Some(start() + DAY));

    let approvals = world.approvals.clone();
    let id = first.id;
    let waiting = runtime.spawn(async move { approvals.wait(id).await });
    runtime.block_on(tokio::task::yield_now());
    world.events.take();
    world.notifier.take();

    world.clock.set(start() + DAY - Duration::from_nanos(1));
    assert!(
        runtime
            .block_on(world.approvals.expire_due())
            .unwrap()
            .is_empty()
    );
    assert!(!waiting.is_finished());

    world.clock.set(start() + DAY);
    let expired = runtime.block_on(world.approvals.expire_due()).unwrap();
    assert_eq!(
        expired.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![first.id]
    );
    let woken = runtime.block_on(waiting).unwrap().unwrap();
    assert_eq!(woken.status, ApprovalStatus::Expired);
    assert_eq!(
        verdict(&woken),
        PermissionVerdict::Deny {
            message: EXPIRED_MESSAGE.into()
        }
    );
    assert_eq!(
        world.store.stored(second.id).status,
        ApprovalStatus::Pending
    );
    assert_eq!(
        world.approvals.next_expiry().unwrap(),
        Some(start() + Duration::from_secs(3600) + DAY)
    );
    assert_eq!(
        world.store.stored_request(request_id).status,
        RequestStatus::Expired
    );
    let events = world.events.take();
    assert_eq!(
        approval_events(&events),
        vec![("approval.update", ApprovalStatus::Expired)]
    );
    assert_eq!(request_statuses(&events), vec![RequestStatus::Expired]);
    assert_eq!(
        world.notifier.take(),
        vec![Notice::Resolved(AttentionTarget::Approval {
            approval_id: first.id
        })]
    );
}

#[test]
fn a_waiting_call_gets_the_owner_decision() {
    // (choice, answer to Claude Code)
    let rows = [
        (
            ApprovalChoice::AllowOnce,
            PermissionVerdict::Allow {
                updated_input: bash(None, "npm test").input,
            },
        ),
        (
            ApprovalChoice::Deny,
            PermissionVerdict::Deny {
                message: DENIED_MESSAGE.into(),
            },
        ),
    ];
    for (choice, expected) in rows {
        let world = World::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let request_id = world.request(RequestStatus::Running);
        let request = PermissionRequest {
            prompt: PermissionPrompt {
                agent_id: world.agent_id,
                run_id: RunId::generate(),
                session_id: None,
                source: PermissionSource::PermissionTool,
                call: bash(Some("toolu_1"), "npm test"),
            },
            caller: world.teammate(),
            request_id: Some(request_id),
        };
        let approvals = world.approvals.clone();
        let asking = runtime.spawn(async move {
            approvals
                .settle(PolicyVerdict::NeedsApproval, &request)
                .await
        });
        let pending = (0..100)
            .find_map(|_| {
                runtime.block_on(tokio::task::yield_now());
                world
                    .store
                    .approvals(Some(ApprovalStatus::Pending))
                    .unwrap()
                    .pop()
            })
            .expect("the approval is raised");
        assert!(!asking.is_finished());
        world.decide(pending.id, choice).unwrap();
        assert_eq!(
            runtime.block_on(asking).unwrap().unwrap(),
            expected,
            "{choice:?}"
        );
    }
}

#[test]
fn settle_answers_allowed_and_denied_calls_without_asking() {
    let world = World::new();
    let request = PermissionRequest {
        prompt: PermissionPrompt {
            agent_id: world.agent_id,
            run_id: RunId::generate(),
            session_id: None,
            source: PermissionSource::PreToolUseHook,
            call: ToolCall {
                tool_use_id: Some("toolu_1".into()),
                tool_name: "Read".into(),
                input: json!({"file_path": "AGENTS.md"}),
            },
        },
        caller: Caller::Owner,
        request_id: None,
    };
    let allow = PolicyVerdict::Allow {
        reason: AllowReason::AlwaysAllowedRead,
    };
    assert_eq!(
        block_on(world.approvals.settle(allow, &request)).unwrap(),
        PermissionVerdict::Allow {
            updated_input: json!({"file_path": "AGENTS.md"})
        }
    );
    let deny = PolicyVerdict::Deny {
        reason: "not shared".into(),
    };
    assert_eq!(
        block_on(world.approvals.settle(deny, &request)).unwrap(),
        PermissionVerdict::Deny {
            message: "not shared".into()
        }
    );
    assert_eq!(world.store.count(), 0);
    assert!(world.events.take().is_empty());
}

#[test]
fn approvals_are_keyed_by_run() {
    let world = World::new();
    let request_id = world.request(RequestStatus::Running);
    let (owner_run, fork_run) = (RunId::generate(), RunId::generate());
    let call = bash(Some("toolu_1"), "npm test");

    let owner = world.raise(world.owner_ticket(owner_run, call.clone()));
    let teammate = world.raise(world.teammate_ticket(fork_run, request_id, call.clone()));
    assert_ne!(owner.id, teammate.id);
    assert_eq!(
        (owner.requester, owner.request_id, owner.purpose.clone()),
        (None, None, None)
    );
    assert_eq!(teammate.requester, Some(world.member));

    // The permission tool and the hook asking about the same call share one card.
    world.events.take();
    let joined = world.raise(world.teammate_ticket(fork_run, request_id, call.clone()));
    assert_eq!(joined.id, teammate.id);
    assert!(world.events.take().is_empty());

    // The same tool use ID with another input, or no ID, is another call.
    let changed = world.raise(world.teammate_ticket(
        fork_run,
        request_id,
        bash(Some("toolu_1"), "npm publish"),
    ));
    assert_ne!(changed.id, teammate.id);
    let unnamed = world.raise(world.owner_ticket(owner_run, bash(None, "npm test")));
    let unnamed_again = world.raise(world.owner_ticket(owner_run, bash(None, "npm test")));
    assert_ne!(unnamed.id, unnamed_again.id);

    // Deciding the owner's approval leaves the teammate's waiting.
    world.decide(owner.id, ApprovalChoice::Deny).unwrap();
    assert_eq!(
        world.store.stored(teammate.id).status,
        ApprovalStatus::Pending
    );
    assert_eq!(
        world.store.stored_request(request_id).status,
        RequestStatus::AwaitingApproval
    );
    assert_eq!(
        world.approvals.pending_for_run(owner_run),
        vec![unnamed.id, unnamed_again.id]
    );
    assert_eq!(
        world.approvals.pending_for_run(fork_run),
        vec![teammate.id, changed.id]
    );
}

#[test]
fn the_request_runs_again_only_when_no_approval_is_left() {
    let world = World::new();
    let request_id = world.request(RequestStatus::Running);
    let run = RunId::generate();
    let first =
        world.raise(world.teammate_ticket(run, request_id, bash(Some("toolu_1"), "npm test")));
    let second =
        world.raise(world.teammate_ticket(run, request_id, bash(Some("toolu_2"), "npm run lint")));
    world.events.take();

    world.decide(first.id, ApprovalChoice::AllowOnce).unwrap();
    assert_eq!(
        world.store.stored_request(request_id).status,
        RequestStatus::AwaitingApproval
    );
    assert!(request_statuses(&world.events.take()).is_empty());

    world.decide(second.id, ApprovalChoice::AllowOnce).unwrap();
    assert_eq!(
        world.store.stored_request(request_id).status,
        RequestStatus::Running
    );
    assert_eq!(
        request_statuses(&world.events.take()),
        vec![RequestStatus::Running]
    );
}

#[test]
fn ending_a_request_closes_its_other_pending_approvals() {
    // (how the first approval ends, the request's final status)
    let rows = [
        ("deny", RequestStatus::Denied),
        ("expire", RequestStatus::Expired),
    ];
    for (ending, final_status) in rows {
        let world = World::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let request_id = world.request(RequestStatus::Running);
        let run = RunId::generate();
        let first =
            world.raise(world.teammate_ticket(run, request_id, bash(Some("toolu_1"), "npm test")));
        world.clock.set(start() + Duration::from_secs(60));
        let second = world.raise(world.teammate_ticket(
            run,
            request_id,
            bash(Some("toolu_2"), "npm run lint"),
        ));
        let other_request = world.request(RequestStatus::Running);
        let unrelated = world.raise(world.teammate_ticket(
            RunId::generate(),
            other_request,
            bash(Some("toolu_3"), "make"),
        ));
        let approvals = world.approvals.clone();
        let id = second.id;
        let waiting = runtime.spawn(async move { approvals.wait(id).await });
        runtime.block_on(tokio::task::yield_now());
        world.events.take();

        if ending == "deny" {
            world.decide(first.id, ApprovalChoice::Deny).unwrap();
        } else {
            world.clock.set(start() + DAY);
            let expired = runtime.block_on(world.approvals.expire_due()).unwrap();
            assert_eq!(
                expired.iter().map(|a| a.id).collect::<Vec<_>>(),
                vec![first.id]
            );
        }

        let closed = world.store.stored(second.id);
        assert_eq!(
            (closed.status, closed.choice, closed.decided_at),
            (ApprovalStatus::Denied, None, None),
            "{ending}"
        );
        assert_eq!(
            verdict(&runtime.block_on(waiting).unwrap().unwrap()),
            PermissionVerdict::Deny {
                message: DENIED_MESSAGE.into()
            }
        );
        assert_eq!(world.store.stored_request(request_id).status, final_status);
        assert_eq!(
            world.store.stored(unrelated.id).status,
            ApprovalStatus::Pending
        );
        let events = world.events.take();
        assert_eq!(approval_events(&events).len(), 2, "{ending}");
        assert_eq!(request_statuses(&events), vec![final_status]);
        assert!(world.approvals.pending_for_run(run).is_empty());

        // Nothing is left to expire for the ended request.
        world.clock.set(start() + DAY + Duration::from_secs(60));
        let expired = runtime.block_on(world.approvals.expire_due()).unwrap();
        assert_eq!(
            expired.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![unrelated.id]
        );
    }
}

#[test]
fn only_the_owner_decides_and_saved_rules_wait_for_task_028() {
    let world = World::new();
    let approval =
        world.raise(world.owner_ticket(RunId::generate(), bash(Some("toolu_1"), "make")));
    world.events.take();

    let by_teammate = block_on(world.approvals.decide(
        &world.teammate(),
        &ApprovalDecide {
            approval_id: approval.id,
            choice: ApprovalChoice::AllowOnce,
            rule: None,
        },
    ));
    assert_eq!(code(by_teammate), ErrorCode::Denied);
    for choice in [
        ApprovalChoice::KeepAllowingSimilar,
        ApprovalChoice::AllowAll,
    ] {
        assert_eq!(
            code(world.decide(approval.id, choice)),
            ErrorCode::InvalidParams
        );
    }
    assert_eq!(
        world.store.stored(approval.id).status,
        ApprovalStatus::Pending
    );
    assert!(world.events.take().is_empty());

    let missing = ApprovalId::generate();
    assert_eq!(
        code(world.decide(missing, ApprovalChoice::AllowOnce)),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(block_on(world.approvals.wait(missing))),
        ErrorCode::NotFound
    );
}

#[test]
fn a_request_that_has_ended_raises_nothing() {
    for status in [
        RequestStatus::Canceled,
        RequestStatus::Completed,
        RequestStatus::Denied,
        RequestStatus::Expired,
        RequestStatus::Failed,
    ] {
        let world = World::new();
        let request_id = world.request(status);
        let error: ApiError = block_on(world.approvals.raise(world.teammate_ticket(
            RunId::generate(),
            request_id,
            bash(Some("toolu_1"), "npm test"),
        )))
        .expect_err("ended request");
        assert_eq!(error.code, ErrorCode::Denied, "{status:?}");
        assert_eq!(world.store.count(), 0);
        assert_eq!(world.store.stored_request(request_id).status, status);
    }
}

#[test]
fn ticket_from_a_permission_request_keeps_the_run_and_caller() {
    let world = World::new();
    let request = PermissionRequest {
        prompt: PermissionPrompt {
            agent_id: world.agent_id,
            run_id: RunId::generate(),
            session_id: None,
            source: PermissionSource::PermissionTool,
            call: bash(Some("toolu_1"), "npm test"),
        },
        caller: world.teammate(),
        request_id: Some(RequestId::generate()),
    };
    let ticket = ApprovalTicket::from(&request);
    assert_eq!(ticket.run_id, request.prompt.run_id);
    assert_eq!(ticket.agent_id, request.prompt.agent_id);
    assert_eq!(ticket.caller, request.caller);
    assert_eq!(ticket.request_id, request.request_id);
    assert_eq!(ticket.call, request.prompt.call);
}

#[test]
fn a_request_that_ends_while_waiting_denies_the_allowed_call() {
    let world = World::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let request_id = world.request(RequestStatus::Running);
    let ticket = world.teammate_ticket(
        RunId::generate(),
        request_id,
        bash(Some("toolu_1"), "npm test"),
    );
    let approvals = world.approvals.clone();
    let asking = runtime.spawn(async move { approvals.ask(ticket).await });
    let pending = (0..100)
        .find_map(|_| {
            runtime.block_on(tokio::task::yield_now());
            world
                .store
                .approvals(Some(ApprovalStatus::Pending))
                .unwrap()
                .pop()
        })
        .expect("the approval is raised");
    // The request ends elsewhere (for example canceled) before the owner answers.
    let mut request = world.store.stored_request(request_id);
    request.status = RequestStatus::Canceled;
    world.store.put_request(&request).unwrap();

    world.decide(pending.id, ApprovalChoice::AllowOnce).unwrap();
    let answer = runtime.block_on(asking).unwrap().unwrap();
    assert!(
        matches!(answer, PermissionVerdict::Deny { .. }),
        "{answer:?}"
    );
    assert_eq!(
        world.store.stored_request(request_id).status,
        RequestStatus::Canceled
    );
}

#[test]
fn deciding_one_input_keeps_a_later_input_with_the_same_tool_use_id_joinable() {
    let world = World::new();
    let run = RunId::generate();
    let first = world.raise(world.owner_ticket(run, bash(Some("toolu_1"), "make")));
    let changed = world.raise(world.owner_ticket(run, bash(Some("toolu_1"), "make install")));
    assert_ne!(first.id, changed.id);
    world.decide(first.id, ApprovalChoice::AllowOnce).unwrap();
    let joined = world.raise(world.owner_ticket(run, bash(Some("toolu_1"), "make install")));
    assert_eq!(joined.id, changed.id);
}

/// An event sink and notifier that look at the approvals service while it works.
#[derive(Default)]
struct Probe {
    approvals: OnceLock<Weak<Approvals>>,
    /// For each published event, whether the service's lock was held.
    published_locked: Mutex<Vec<bool>>,
    notices: Mutex<Vec<Notice>>,
    /// Decide this approval from inside `Notifier::raise`, as a fast owner would.
    decide_on_raise: Mutex<Option<ApprovalChoice>>,
}

impl Probe {
    fn approvals(&self) -> Arc<Approvals> {
        self.approvals.get().unwrap().upgrade().unwrap()
    }
}

impl EventSink for Probe {
    fn publish(&self, _event: Event) -> Seq {
        let held = self.approvals().state.try_lock().is_err();
        let mut published = self.published_locked.lock().unwrap();
        published.push(held);
        Seq(published.len() as u64)
    }
}

#[async_trait]
impl Notifier for Probe {
    async fn raise(
        &self,
        kind: AttentionKind,
        target: AttentionTarget,
        subject: String,
    ) -> Result<Attention> {
        let choice = self.decide_on_raise.lock().unwrap().take();
        if let (Some(choice), AttentionTarget::Approval { approval_id }) = (choice, &target) {
            self.approvals()
                .decide(
                    &Caller::Owner,
                    &ApprovalDecide {
                        approval_id: *approval_id,
                        choice,
                        rule: None,
                    },
                )
                .await
                .unwrap();
        }
        self.notices
            .lock()
            .unwrap()
            .push(Notice::Raised(kind, target, subject.clone()));
        Ok(Attention {
            id: AttentionId::generate(),
            kind,
            target,
            subject,
            at: start(),
            resolved_at: None,
        })
    }

    async fn resolve(&self, target: AttentionTarget) -> Result<()> {
        self.notices.lock().unwrap().push(Notice::Resolved(target));
        Ok(())
    }

    async fn inbox(&self) -> Result<Vec<Attention>> {
        Ok(Vec::new())
    }
}

#[test]
fn events_go_out_in_order_and_a_quick_decision_leaves_no_open_item() {
    let store = Arc::new(MemoryStore::default());
    let probe = Arc::new(Probe::default());
    let approvals = Arc::new(Approvals::new(
        store.clone(),
        probe.clone(),
        probe.clone(),
        Arc::new(ManualClock(Mutex::new(start()))),
    ));
    probe.approvals.set(Arc::downgrade(&approvals)).unwrap();
    *probe.decide_on_raise.lock().unwrap() = Some(ApprovalChoice::AllowOnce);

    let approval = block_on(approvals.raise(ApprovalTicket {
        run_id: RunId::generate(),
        agent_id: AgentId::generate(),
        caller: Caller::Owner,
        request_id: None,
        call: bash(Some("toolu_1"), "make"),
    }))
    .unwrap();

    // approval.raised, then approval.update from the decision, each published
    // while the status change still held the lock.
    assert_eq!(*probe.published_locked.lock().unwrap(), vec![true, true]);
    assert_eq!(store.stored(approval.id).status, ApprovalStatus::Allowed);
    let target = AttentionTarget::Approval {
        approval_id: approval.id,
    };
    let notices = probe.notices.lock().unwrap().clone();
    assert_eq!(
        notices.last(),
        Some(&Notice::Resolved(target)),
        "{notices:?}"
    );
    assert!(notices.contains(&Notice::Raised(
        AttentionKind::Approval,
        target,
        "make".into()
    )));
}

#[test]
fn summaries_show_the_exact_command_or_target() {
    let call = |tool_name: &str, input: Value| ToolCall {
        tool_use_id: None,
        tool_name: tool_name.into(),
        input,
    };
    let rows = [
        (
            call(
                "Bash",
                json!({"command": "npm test -- auth/*", "description": "x"}),
            ),
            "npm test -- auth/*",
        ),
        (
            call("Write", json!({"file_path": "/a/new.txt", "content": "x"})),
            "Write /a/new.txt",
        ),
        (
            call(
                "Edit",
                json!({"file_path": "/a/b.rs", "old_string": "x", "new_string": "y"}),
            ),
            "Edit /a/b.rs",
        ),
        (
            call("NotebookEdit", json!({"notebook_path": "/a/n.ipynb"})),
            "NotebookEdit /a/n.ipynb",
        ),
        (
            call(
                "WebFetch",
                json!({"url": "https://example.com", "prompt": "title"}),
            ),
            "WebFetch https://example.com",
        ),
        (
            call("WebSearch", json!({"query": "tailscale whois"})),
            "WebSearch tailscale whois",
        ),
        (
            call("Read", json!({"file_path": "/etc/hosts"})),
            "Read /etc/hosts",
        ),
        (
            call("Grep", json!({"pattern": "token", "path": "/etc"})),
            "Grep token in /etc",
        ),
        (call("Glob", json!({"pattern": "/etc/*"})), "Glob /etc/*"),
        (
            call("mcp__db__query", json!({"sql": "select 1"})),
            r#"mcp__db__query {"sql":"select 1"}"#,
        ),
        (
            call("Bash", json!({"description": "x"})),
            r#"Bash {"description":"x"}"#,
        ),
        (
            call("Bash", json!({"command": " "})),
            r#"Bash {"command":" "}"#,
        ),
    ];
    for (call, expected) in rows {
        assert_eq!(summarize(&call), expected);
    }
}

#[test]
fn the_purpose_is_the_first_line_of_the_prompt() {
    assert_eq!(
        purpose("\n  Fix login  \nmore").as_deref(),
        Some("Fix login")
    );
    assert_eq!(purpose("   \n\t"), None);
    let long = "가".repeat(300);
    let short = purpose(&long).unwrap();
    assert_eq!(short.chars().count(), 200);
    assert!(short.ends_with('…'));
}
