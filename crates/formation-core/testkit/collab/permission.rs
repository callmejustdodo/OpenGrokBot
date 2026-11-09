//! Owner: B. `FakePermissionGate`: scripted verdicts, and approvals that a test
//! (or C's fake service) decides or expires.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{broadcast, oneshot, watch};

use formation_proto::common::{ApiError, ApprovalId, ErrorCode, RunId};
use formation_proto::policy::{
    Approval, ApprovalChoice, ApprovalEvent, ApprovalStatus, Caller, PermissionSource,
    PermissionVerdict, ToolCall,
};

use crate::ports::{PermissionGate, PermissionRequest, Result};

use super::{APPROVAL_TTL, error, is_read, later, lock, now};

/// How the gate answers one prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum GateAnswer {
    /// Allow with the tool input unchanged.
    Allow,
    /// Allow with this tool input instead.
    AllowWith(Value),
    Deny(String),
    /// No decision. The contract allows it for `PreToolUse` hook prompts only, so a
    /// permission tool prompt answered `Defer` gets `invalid_params`.
    Defer,
    /// Raise a pending approval (`approval.raised`) and wait for the owner. A hook
    /// prompt defers instead.
    AskOwner,
    Fail(ApiError),
}

#[derive(Debug)]
enum Decision {
    Choice(ApprovalChoice),
    Expired,
}

/// A [`PermissionGate`] with scripted answers and owner-decided approvals.
///
/// Each prompt is answered by, in order: the next answer queued for its source
/// with [`FakePermissionGate::script_for`] ([`FakePermissionGate::script`] for the
/// permission tool), the answer set for the tool, the default answer, and
/// otherwise [`GateAnswer::Allow`] for [`READ_TOOLS`](super::READ_TOOLS) and
/// [`GateAnswer::AskOwner`] for everything else, the permission tool rules of
/// D-028 without saved rules.
///
/// A `PreToolUse` hook prompt only ever gets `deny`, an error, or `defer`, as the
/// contract's [`PermissionVerdict::Defer`] requires: [`GateAnswer::Deny`] and
/// [`GateAnswer::Fail`] apply, and every other answer becomes `defer`. In a real run
/// each call that is not a read reaches the hook first and then the permission tool
/// with the same `tool_use_id`, so only the permission tool prompt raises the
/// approval, and the hook never skips it by allowing.
///
/// [`GateAnswer::AskOwner`] records an [`Approval`] and waits. The owner side,
/// [`FakePermissionGate::decide_approval`] and [`FakePermissionGate::expire`],
/// resolves it: allow choices give `allow` with the original input, `deny` gives
/// `deny`, and expiry gives `approval_expired`. Allow choices that save rules in
/// the real gate (`keep_allowing_similar`, `allow_all`) allow only this call here;
/// script later calls with [`FakePermissionGate::set_tool_answer`]. Nothing expires
/// on its own.
///
/// Deny wins: once a tool call (same run and `tool_use_id`) is denied, every later
/// prompt for it, from the permission tool or the hook, is denied too.
pub struct FakePermissionGate {
    state: Mutex<GateState>,
    changes: watch::Sender<u64>,
    events: broadcast::Sender<ApprovalEvent>,
}

#[derive(Default)]
struct GateState {
    tool_script: VecDeque<GateAnswer>,
    hook_script: VecDeque<GateAnswer>,
    by_tool: HashMap<String, GateAnswer>,
    default: Option<GateAnswer>,
    approvals: Vec<Approval>,
    waiting: HashMap<ApprovalId, oneshot::Sender<Decision>>,
    denied: HashSet<(RunId, String)>,
    calls: Vec<PermissionRequest>,
    verdicts: Vec<(PermissionRequest, Result<PermissionVerdict>)>,
}

impl Default for FakePermissionGate {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changes: watch::channel(0).0,
            events: broadcast::channel(256).0,
        }
    }
}

impl std::fmt::Debug for FakePermissionGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("FakePermissionGate")
            .field("approvals", &state.approvals)
            .field("calls", &state.calls.len())
            .finish_non_exhaustive()
    }
}

impl FakePermissionGate {
    /// Reads allowed; everything else asks the owner through the permission tool
    /// prompt. Hook prompts defer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allows every permission tool prompt (for tests that are not about
    /// permissions). Hook prompts defer.
    pub fn allowing() -> Self {
        Self::with_default(GateAnswer::Allow)
    }

    /// Denies every prompt, from the hook too.
    pub fn denying(message: impl Into<String>) -> Self {
        Self::with_default(GateAnswer::Deny(message.into()))
    }

    /// Asks the owner for every permission tool prompt, reads included. Hook prompts
    /// defer.
    pub fn asking_owner() -> Self {
        Self::with_default(GateAnswer::AskOwner)
    }

    fn with_default(answer: GateAnswer) -> Self {
        let gate = Self::new();
        gate.set_default(answer);
        gate
    }

    /// Answers the next permission tool prompt with `answer`. Answers queue up;
    /// hook prompts leave them alone.
    pub fn script(&self, answer: GateAnswer) {
        self.script_for(PermissionSource::PermissionTool, answer);
    }

    /// Answers the next prompt from `source` with `answer`. Answers queue up per
    /// source, so the hook prompt that comes first does not use up an answer meant
    /// for the permission tool.
    pub fn script_for(&self, source: PermissionSource, answer: GateAnswer) {
        lock(&self.state).script(source).push_back(answer);
    }

    pub fn set_tool_answer(&self, tool_name: impl Into<String>, answer: GateAnswer) {
        lock(&self.state).by_tool.insert(tool_name.into(), answer);
    }

    pub fn set_default(&self, answer: GateAnswer) {
        lock(&self.state).default = Some(answer);
    }

    /// Every prompt received, in order, including ones still waiting.
    pub fn calls(&self) -> Vec<PermissionRequest> {
        lock(&self.state).calls.clone()
    }

    /// Every prompt answered so far, with its answer, in the order answered.
    pub fn verdicts(&self) -> Vec<(PermissionRequest, Result<PermissionVerdict>)> {
        lock(&self.state).verdicts.clone()
    }

    /// Every approval raised, oldest first.
    pub fn approvals(&self) -> Vec<Approval> {
        lock(&self.state).approvals.clone()
    }

    pub fn pending(&self) -> Vec<Approval> {
        lock(&self.state)
            .approvals
            .iter()
            .filter(|approval| approval.status == ApprovalStatus::Pending)
            .cloned()
            .collect()
    }

    /// The oldest pending approval, waiting until one is raised.
    pub async fn next_pending(&self) -> Approval {
        let mut changes = self.changes.subscribe();
        loop {
            if let Some(approval) = self.pending().into_iter().next() {
                return approval;
            }
            // The sender lives in `self`, so this only returns on a change.
            let _ = changes.changed().await;
        }
    }

    /// `approval.raised` (status `pending`) and `approval.update` events.
    pub fn subscribe(&self) -> broadcast::Receiver<ApprovalEvent> {
        self.events.subscribe()
    }

    /// The owner's decision, as `approval_decide` takes it. `not_found` for an
    /// unknown approval, `approval_expired` for an expired one, `conflict` for one
    /// already decided.
    pub fn decide_approval(&self, id: ApprovalId, choice: ApprovalChoice) -> Result<Approval> {
        let status = match choice {
            ApprovalChoice::Deny => ApprovalStatus::Denied,
            _ => ApprovalStatus::Allowed,
        };
        self.resolve(id, status, Some(choice), Decision::Choice(choice))
    }

    /// Expires a pending approval, as 24 hours without an answer would.
    pub fn expire(&self, id: ApprovalId) -> Result<Approval> {
        self.resolve(id, ApprovalStatus::Expired, None, Decision::Expired)
    }

    fn resolve(
        &self,
        id: ApprovalId,
        status: ApprovalStatus,
        choice: Option<ApprovalChoice>,
        decision: Decision,
    ) -> Result<Approval> {
        let approval = {
            let mut state = lock(&self.state);
            let approval = state
                .approvals
                .iter_mut()
                .find(|approval| approval.id == id)
                .ok_or_else(|| error(ErrorCode::NotFound, format!("no approval {id}")))?;
            match approval.status {
                ApprovalStatus::Pending => {}
                ApprovalStatus::Expired => {
                    return Err(error(
                        ErrorCode::ApprovalExpired,
                        format!("approval {id} expired"),
                    ));
                }
                decided => {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("approval {id} is already {decided:?}"),
                    ));
                }
            }
            approval.status = status;
            approval.choice = choice;
            if status != ApprovalStatus::Expired {
                approval.decided_at = Some(now());
            }
            let approval = approval.clone();
            if let Some(waiter) = state.waiting.remove(&id) {
                // The prompt may have gone away (its run stopped); nothing waits then.
                let _ = waiter.send(decision);
            }
            self.send_event(&approval);
            approval
        };
        self.changed();
        Ok(approval)
    }

    /// Sends the approval event. Callers hold the state lock, so a decider on
    /// another thread cannot get an approval's update out before its
    /// `approval.raised`. `broadcast::Sender::send` never takes our lock.
    fn send_event(&self, approval: &Approval) {
        // No subscriber is fine; events are not kept.
        let _ = self.events.send(ApprovalEvent {
            approval: approval.clone(),
        });
    }

    /// Wakes [`FakePermissionGate::next_pending`].
    fn changed(&self) {
        self.changes.send_modify(|version| *version += 1);
    }

    fn finish(
        &self,
        request: PermissionRequest,
        outcome: Result<PermissionVerdict>,
    ) -> Result<PermissionVerdict> {
        let mut state = lock(&self.state);
        if let (Ok(PermissionVerdict::Deny { .. }), Some(key)) = (&outcome, denial_key(&request)) {
            state.denied.insert(key);
        }
        state.verdicts.push((request, outcome.clone()));
        outcome
    }
}

impl GateState {
    fn script(&mut self, source: PermissionSource) -> &mut VecDeque<GateAnswer> {
        match source {
            PermissionSource::PermissionTool => &mut self.tool_script,
            PermissionSource::PreToolUseHook => &mut self.hook_script,
        }
    }

    fn next_answer(&mut self, source: PermissionSource, tool_name: &str) -> GateAnswer {
        let answer = self
            .script(source)
            .pop_front()
            .or_else(|| {
                self.by_tool
                    .get(tool_name)
                    .or(self.default.as_ref())
                    .cloned()
            })
            .unwrap_or_else(|| {
                if is_read(tool_name) {
                    GateAnswer::Allow
                } else {
                    GateAnswer::AskOwner
                }
            });
        match (source, answer) {
            (PermissionSource::PermissionTool, answer) => answer,
            (_, answer @ (GateAnswer::Deny(_) | GateAnswer::Fail(_))) => answer,
            // The hook leaves allowing and asking to Claude Code's permission flow and
            // the permission tool prompt that follows.
            (PermissionSource::PreToolUseHook, _) => GateAnswer::Defer,
        }
    }
}

fn denial_key(request: &PermissionRequest) -> Option<(RunId, String)> {
    let tool_use_id = request.prompt.call.tool_use_id.clone()?;
    Some((request.prompt.run_id, tool_use_id))
}

fn approval_for(request: &PermissionRequest) -> Approval {
    let (requester, requester_agent) = match &request.caller {
        Caller::Owner => (None, None),
        Caller::Member {
            member_id,
            via_agent,
            ..
        } => (Some(*member_id), via_agent.clone()),
    };
    let created_at = now();
    Approval {
        id: ApprovalId::generate(),
        agent_id: request.prompt.agent_id,
        request_id: request.request_id,
        requester,
        requester_agent,
        purpose: None,
        call: request.prompt.call.clone(),
        summary: summary(&request.prompt.call),
        proposed_rule: None,
        status: ApprovalStatus::Pending,
        choice: None,
        created_at,
        decided_at: None,
        expires_at: later(created_at, APPROVAL_TTL),
    }
}

/// The command for `Bash`; otherwise the tool and the path or URL it touches.
fn summary(call: &ToolCall) -> String {
    let field = |key: &str| call.input.get(key).and_then(Value::as_str);
    if let Some(command) = field("command") {
        return command.to_owned();
    }
    ["file_path", "notebook_path", "path", "url"]
        .into_iter()
        .find_map(field)
        .map_or_else(
            || call.tool_name.clone(),
            |target| format!("{} {target}", call.tool_name),
        )
}

#[async_trait]
impl PermissionGate for FakePermissionGate {
    async fn decide(&self, request: PermissionRequest) -> Result<PermissionVerdict> {
        let receiver = {
            let mut state = lock(&self.state);
            state.calls.push(request.clone());
            let answer = if denial_key(&request).is_some_and(|key| state.denied.contains(&key)) {
                GateAnswer::Deny("denied earlier for this tool call".into())
            } else {
                state.next_answer(request.prompt.source, &request.prompt.call.tool_name)
            };
            let outcome = match answer {
                GateAnswer::AskOwner => None,
                GateAnswer::Allow => Some(Ok(PermissionVerdict::Allow {
                    updated_input: request.prompt.call.input.clone(),
                })),
                GateAnswer::AllowWith(updated_input) => {
                    Some(Ok(PermissionVerdict::Allow { updated_input }))
                }
                GateAnswer::Deny(message) => Some(Ok(PermissionVerdict::Deny { message })),
                GateAnswer::Defer if request.prompt.source == PermissionSource::PreToolUseHook => {
                    Some(Ok(PermissionVerdict::Defer))
                }
                GateAnswer::Defer => Some(Err(error(
                    ErrorCode::InvalidParams,
                    "defer answers PreToolUse hook prompts only",
                ))),
                GateAnswer::Fail(error) => Some(Err(error)),
            };
            if let Some(outcome) = outcome {
                drop(state);
                return self.finish(request, outcome);
            }
            let approval = approval_for(&request);
            let (sender, receiver) = oneshot::channel();
            state.waiting.insert(approval.id, sender);
            state.approvals.push(approval.clone());
            self.send_event(&approval);
            drop(state);
            self.changed();
            receiver
        };
        let outcome = match receiver.await {
            Ok(Decision::Choice(ApprovalChoice::Deny)) => Ok(PermissionVerdict::Deny {
                message: "Denied by the owner".into(),
            }),
            Ok(Decision::Choice(_)) => Ok(PermissionVerdict::Allow {
                updated_input: request.prompt.call.input.clone(),
            }),
            Ok(Decision::Expired) => Err(error(
                ErrorCode::ApprovalExpired,
                "the owner did not answer the approval in time",
            )),
            Err(_) => Err(error(ErrorCode::Internal, "the approval was dropped")),
        };
        self.finish(request, outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use formation_proto::common::{AgentId, MemberId, RequestId};
    use formation_proto::policy::{Caller, Chain};

    use super::super::block_on;
    use super::super::samples::{bash, from_hook, member_caller, permission_request, tool_call};
    use super::*;

    fn owner_prompt(call: ToolCall) -> PermissionRequest {
        permission_request(
            AgentId::generate(),
            RunId::generate(),
            Caller::Owner,
            None,
            call,
        )
    }

    #[test]
    fn reads_are_allowed_and_other_calls_wait_for_the_owner() {
        let gate = Arc::new(FakePermissionGate::new());
        let port: Arc<dyn PermissionGate> = gate.clone();
        block_on(async {
            let read = tool_call("Read", serde_json::json!({"file_path": "README.md"}));
            assert_eq!(
                port.decide(owner_prompt(read.clone())).await.unwrap(),
                PermissionVerdict::Allow {
                    updated_input: read.input
                }
            );
            assert!(gate.approvals().is_empty());

            let mut events = gate.subscribe();
            let waiting = tokio::spawn({
                let port = port.clone();
                async move { port.decide(owner_prompt(bash("npm test"))).await }
            });
            let approval = gate.next_pending().await;
            assert_eq!(approval.summary, "npm test");
            assert_eq!(approval.requester, None);
            assert_eq!(
                events.recv().await.unwrap().approval.status,
                ApprovalStatus::Pending
            );

            let decided = gate
                .decide_approval(approval.id, ApprovalChoice::AllowOnce)
                .unwrap();
            assert_eq!(decided.status, ApprovalStatus::Allowed);
            assert_eq!(decided.choice, Some(ApprovalChoice::AllowOnce));
            assert!(decided.decided_at.is_some());
            assert_eq!(
                waiting.await.unwrap().unwrap(),
                PermissionVerdict::Allow {
                    updated_input: serde_json::json!({"command": "npm test"})
                }
            );
            assert_eq!(
                events.recv().await.unwrap().approval.status,
                ApprovalStatus::Allowed
            );
            assert!(gate.pending().is_empty());
        });
        assert_eq!(gate.calls().len(), 2);
        assert_eq!(gate.verdicts().len(), 2);
    }

    #[test]
    fn approvals_carry_the_requester_and_expire_after_a_day() {
        let gate = Arc::new(FakePermissionGate::asking_owner());
        let (minsu, request_id) = (MemberId::generate(), RequestId::generate());
        let caller = Caller::Member {
            member_id: minsu,
            via_agent: Some("web".into()),
            chain: Chain {
                id: formation_proto::common::ChainId::generate(),
                depth: 1,
            },
        };
        let prompt = permission_request(
            AgentId::generate(),
            RunId::generate(),
            caller,
            Some(request_id),
            tool_call("Write", serde_json::json!({"file_path": "src/health.rs"})),
        );
        block_on(async {
            let waiting = tokio::spawn({
                let gate = gate.clone();
                async move { gate.decide(prompt).await }
            });
            let approval = gate.next_pending().await;
            assert_eq!(approval.requester, Some(minsu));
            assert_eq!(approval.requester_agent.as_deref(), Some("web"));
            assert_eq!(approval.request_id, Some(request_id));
            assert_eq!(approval.summary, "Write src/health.rs");
            assert_eq!((approval.expires_at - approval.created_at).num_hours(), 24);

            let expired = gate.expire(approval.id).unwrap();
            assert_eq!(expired.status, ApprovalStatus::Expired);
            let error = waiting.await.unwrap().unwrap_err();
            assert_eq!(error.code, ErrorCode::ApprovalExpired);

            let late = gate
                .decide_approval(approval.id, ApprovalChoice::AllowOnce)
                .unwrap_err();
            assert_eq!(late.code, ErrorCode::ApprovalExpired);
            let unknown = gate
                .decide_approval(ApprovalId::generate(), ApprovalChoice::Deny)
                .unwrap_err();
            assert_eq!(unknown.code, ErrorCode::NotFound);
        });
    }

    #[test]
    fn an_owner_deny_wins_over_a_later_prompt_for_the_same_call() {
        let gate = Arc::new(FakePermissionGate::new());
        let prompt = owner_prompt(bash("rm -rf build"));
        block_on(async {
            let waiting = tokio::spawn({
                let (gate, prompt) = (gate.clone(), prompt.clone());
                async move { gate.decide(prompt).await }
            });
            let approval = gate.next_pending().await;
            gate.decide_approval(approval.id, ApprovalChoice::Deny)
                .unwrap();
            assert!(matches!(
                waiting.await.unwrap().unwrap(),
                PermissionVerdict::Deny { .. }
            ));
            let again = gate
                .decide_approval(approval.id, ApprovalChoice::AllowOnce)
                .unwrap_err();
            assert_eq!(again.code, ErrorCode::Conflict);

            gate.script(GateAnswer::Allow);
            gate.script(GateAnswer::Defer);
            assert!(matches!(
                gate.decide(prompt.clone()).await.unwrap(),
                PermissionVerdict::Deny { .. }
            ));
            assert!(matches!(
                gate.decide(from_hook(prompt)).await.unwrap(),
                PermissionVerdict::Deny { .. }
            ));
        });
    }

    #[test]
    fn a_hook_prompt_defers_so_one_tool_call_raises_one_approval() {
        let gate = Arc::new(FakePermissionGate::new());
        let edit = tool_call(
            "Edit",
            serde_json::json!({"file_path": "edit.txt", "old_string": "blue", "new_string": "red"}),
        );
        // A real run sends the hook prompt, then the permission tool prompt with the
        // same `tool_use_id` (fixture `C-manual-allow`).
        let prompt = owner_prompt(edit.clone());
        block_on(async {
            assert_eq!(
                gate.decide(from_hook(prompt.clone())).await.unwrap(),
                PermissionVerdict::Defer
            );
            assert!(gate.approvals().is_empty());

            let waiting = tokio::spawn({
                let (gate, prompt) = (gate.clone(), prompt.clone());
                async move { gate.decide(prompt).await }
            });
            let approval = gate.next_pending().await;
            gate.decide_approval(approval.id, ApprovalChoice::AllowOnce)
                .unwrap();
            assert_eq!(
                waiting.await.unwrap().unwrap(),
                PermissionVerdict::Allow {
                    updated_input: edit.input
                }
            );

            // Reads and owner-asking defaults defer on the hook as well.
            let read = tool_call("Read", serde_json::json!({"file_path": "note.txt"}));
            assert_eq!(
                gate.decide(from_hook(owner_prompt(read))).await.unwrap(),
                PermissionVerdict::Defer
            );
            let asking = FakePermissionGate::asking_owner();
            assert_eq!(
                asking
                    .decide(from_hook(owner_prompt(bash("ls"))))
                    .await
                    .unwrap(),
                PermissionVerdict::Defer
            );
            assert!(asking.approvals().is_empty());
        });
        assert_eq!(gate.approvals().len(), 1);
        assert_eq!(gate.calls().len(), 3);
    }

    #[test]
    fn scripted_answers_apply_per_source_in_order_then_per_tool_then_default() {
        let gate = FakePermissionGate::allowing();
        let changed = serde_json::json!({"command": "npm test -- --ci"});
        gate.script(GateAnswer::AllowWith(changed.clone()));
        gate.script(GateAnswer::Deny("not today".into()));
        gate.script(GateAnswer::Defer);
        gate.script(GateAnswer::Fail(ApiError::new(ErrorCode::Internal, "boom")));
        gate.script_for(
            PermissionSource::PreToolUseHook,
            GateAnswer::Deny("hook says no".into()),
        );
        gate.script_for(PermissionSource::PreToolUseHook, GateAnswer::Allow);
        gate.set_tool_answer("WebFetch", GateAnswer::Deny("no network".into()));
        block_on(async {
            let prompt = || owner_prompt(bash("npm test"));
            // The hook prompt that comes first does not use up the permission tool's
            // scripted answer.
            assert_eq!(
                gate.decide(from_hook(prompt())).await.unwrap(),
                PermissionVerdict::Deny {
                    message: "hook says no".into()
                }
            );
            assert_eq!(
                gate.decide(prompt()).await.unwrap(),
                PermissionVerdict::Allow {
                    updated_input: changed
                }
            );
            // A scripted hook allow still defers.
            assert_eq!(
                gate.decide(from_hook(prompt())).await.unwrap(),
                PermissionVerdict::Defer
            );
            assert_eq!(
                gate.decide(prompt()).await.unwrap(),
                PermissionVerdict::Deny {
                    message: "not today".into()
                }
            );
            let misuse = gate.decide(prompt()).await.unwrap_err();
            assert_eq!(misuse.code, ErrorCode::InvalidParams);
            assert_eq!(
                gate.decide(prompt()).await.unwrap_err().code,
                ErrorCode::Internal
            );

            let fetch = || {
                owner_prompt(tool_call(
                    "WebFetch",
                    serde_json::json!({"url": "https://example.com"}),
                ))
            };
            assert!(matches!(
                gate.decide(from_hook(fetch())).await.unwrap(),
                PermissionVerdict::Deny { .. }
            ));
            assert!(matches!(
                gate.decide(fetch()).await.unwrap(),
                PermissionVerdict::Deny { .. }
            ));
            assert!(matches!(
                gate.decide(prompt()).await.unwrap(),
                PermissionVerdict::Allow { .. }
            ));
            assert_eq!(
                gate.decide(from_hook(prompt())).await.unwrap(),
                PermissionVerdict::Defer
            );
            let teammate = permission_request(
                AgentId::generate(),
                RunId::generate(),
                member_caller(MemberId::generate()),
                None,
                bash("ls"),
            );
            let denying = FakePermissionGate::denying("closed");
            assert!(matches!(
                denying.decide(from_hook(teammate.clone())).await.unwrap(),
                PermissionVerdict::Deny { .. }
            ));
            assert!(matches!(
                denying.decide(teammate).await.unwrap(),
                PermissionVerdict::Deny { .. }
            ));
        });
    }

    /// The asker and the decider on separate threads: `approval.raised` must still
    /// arrive before the decision's `approval.update`.
    #[test]
    fn approval_events_stay_in_order_across_threads() {
        for _ in 0..200 {
            let gate = Arc::new(FakePermissionGate::new());
            let mut events = gate.subscribe();
            let asker = std::thread::spawn({
                let gate = gate.clone();
                move || block_on(gate.decide(owner_prompt(bash("npm test"))))
            });
            let approval = loop {
                if let Some(approval) = gate.pending().into_iter().next() {
                    break approval;
                }
                std::thread::yield_now();
            };
            gate.decide_approval(approval.id, ApprovalChoice::AllowOnce)
                .unwrap();
            asker.join().unwrap().unwrap();
            let order = [events.try_recv().unwrap(), events.try_recv().unwrap()]
                .map(|event| event.approval.status);
            assert_eq!(order, [ApprovalStatus::Pending, ApprovalStatus::Allowed]);
        }
    }
}
