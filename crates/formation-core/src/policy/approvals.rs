//! Approvals (FR-015, US-011, SC-011): a tool call that is neither an
//! always-allowed read nor covered by a saved rule (TASK-028) waits for the
//! owner, who allows it once or denies it. An approval left unanswered for 24
//! hours expires, which denies the call.
//!
//! Approvals are keyed by the run that raised them
//! ([`PermissionPrompt::run_id`]), so the owner's own turns and teammates' forks
//! never share one. Asks for the same tool call of the same run (same
//! `tool_use_id` and input) while it is pending share one approval, so the owner
//! sees one card. A decision answers that one call: allow once is used up by it,
//! and the next call, even an identical one, raises a new approval.
//!
//! What each change tells whom:
//! - Clients get `approval.raised` and `approval.update` through the
//!   [`EventSink`]; the owner gets a needs-attention item through the
//!   [`Notifier`], resolved once the approval is decided or expires.
//! - The requester is told through their work request: it moves to
//!   `awaiting_approval` while an approval is pending, back to `running` once
//!   none is, and ends as `denied` or `expired` with the matching error. Each
//!   change is stored and published as `request.update`, which the peer server
//!   forwards to the requester's Mac. When a request ends, its other pending
//!   approvals are denied with it, and its later tool calls are denied at once
//!   without raising an approval.
//!
//! Only the owner decides ([`Caller::Owner`], the local socket). Expiry is
//! checked whenever an approval is decided or waited on, and
//! [`Approvals::expire_due`] expires the rest; formationd calls it at start
//! (backend architecture, "Recovery") and when [`Approvals::next_expiry`] passes.
//!
//! [`PermissionPrompt::run_id`]: formation_proto::policy::PermissionPrompt::run_id

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::oneshot;

use formation_proto::agent::{AttentionKind, AttentionTarget};
use formation_proto::common::{
    AgentId, ApiError, ApprovalId, ErrorCode, RequestId, RunId, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::policy::{
    Approval, ApprovalChoice, ApprovalDecide, ApprovalEvent, ApprovalStatus, Caller,
    PermissionVerdict, PolicyVerdict, ToolCall,
};
use formation_proto::request::{RequestStatus, RequestUpdateEvent, WorkRequest};

use crate::ports::{ApprovalRepo, EventSink, Notifier, PermissionRequest, RequestRepo, Result};

/// An unanswered approval expires this long after it was raised (FR-015).
pub const APPROVAL_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The longest purpose taken from a work request's prompt, in characters.
const PURPOSE_MAX_CHARS: usize = 200;

pub const DENIED_MESSAGE: &str = "The owner denied this action.";
pub const EXPIRED_MESSAGE: &str =
    "The owner did not answer within 24 hours, so this action was denied.";

/// The time source, so tests can move it.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from(SystemTime::now())
    }
}

/// The repositories approvals use. Every [`CollabStore`](crate::ports::CollabStore)
/// is one.
pub trait ApprovalStore: ApprovalRepo + RequestRepo + Send + Sync {}

impl<T: ApprovalRepo + RequestRepo + Send + Sync> ApprovalStore for T {}

/// One tool call that needs the owner.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalTicket {
    pub run_id: RunId,
    pub agent_id: AgentId,
    /// The caller of the run: the owner, or the teammate whose request it runs.
    pub caller: Caller,
    pub request_id: Option<RequestId>,
    pub call: ToolCall,
}

impl From<&PermissionRequest> for ApprovalTicket {
    fn from(request: &PermissionRequest) -> Self {
        Self {
            run_id: request.prompt.run_id,
            agent_id: request.prompt.agent_id,
            caller: request.caller.clone(),
            request_id: request.request_id,
            call: request.prompt.call.clone(),
        }
    }
}

pub struct Approvals {
    store: Arc<dyn ApprovalStore>,
    events: Arc<dyn EventSink>,
    notifier: Arc<dyn Notifier>,
    clock: Arc<dyn Clock>,
    /// Serializes every status change, and holds what lives only while
    /// formationd runs.
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Pending approvals by the run and tool call that raised them.
    by_call: HashMap<(RunId, String), ApprovalId>,
    /// The run of each pending approval raised since start.
    runs: HashMap<ApprovalId, RaisedBy>,
    /// Who waits for each pending approval.
    waiters: HashMap<ApprovalId, Vec<oneshot::Sender<Approval>>>,
}

struct RaisedBy {
    run_id: RunId,
    tool_use_id: Option<String>,
}

/// A status change, to announce once the lock is released.
struct Change {
    approval: Approval,
    request: Option<WorkRequest>,
    waiters: Vec<oneshot::Sender<Approval>>,
}

impl Approvals {
    pub fn new(
        store: Arc<dyn ApprovalStore>,
        events: Arc<dyn EventSink>,
        notifier: Arc<dyn Notifier>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            events,
            notifier,
            clock,
            state: Mutex::default(),
        }
    }

    /// The gate's answer once the policy has spoken (`PermissionGate::decide`
    /// without the hook's defer, TASK-044): allowed and denied calls are answered
    /// at once; a call that needs approval raises one and waits for it.
    pub async fn settle(
        &self,
        verdict: PolicyVerdict,
        request: &PermissionRequest,
    ) -> Result<PermissionVerdict> {
        match verdict {
            PolicyVerdict::Allow { .. } => Ok(PermissionVerdict::Allow {
                updated_input: request.prompt.call.input.clone(),
            }),
            PolicyVerdict::Deny { reason } => Ok(PermissionVerdict::Deny { message: reason }),
            PolicyVerdict::NeedsApproval => self.ask(ApprovalTicket::from(request)).await,
        }
    }

    /// Raises an approval for the call, waits for the owner or expiry, and
    /// returns the answer. A call from a request that has ended is denied, also
    /// when it ended while the approval was waiting.
    pub async fn ask(&self, ticket: ApprovalTicket) -> Result<PermissionVerdict> {
        let request_id = ticket.request_id;
        let approval = match self.raise(ticket).await {
            Ok(approval) => approval,
            Err(error) if error.code == ErrorCode::Denied => {
                return Ok(PermissionVerdict::Deny {
                    message: error.message,
                });
            }
            Err(error) => return Err(error),
        };
        let approval = self.wait(approval.id).await?;
        if approval.status == ApprovalStatus::Allowed
            && let Some(id) = request_id
            && let Some(request) = self.store.request(id)?
            && request.status.is_final()
        {
            return Ok(PermissionVerdict::Deny {
                message: format!(
                    "The work request has ended ({}), so this action was denied.",
                    status_name(request.status)
                ),
            });
        }
        Ok(verdict(&approval))
    }

    /// Raises an approval, or returns the pending one for the same call of the
    /// same run. `denied` when the call's work request has already ended.
    pub async fn raise(&self, ticket: ApprovalTicket) -> Result<Approval> {
        let approval = {
            let mut state = self.lock();
            if let Some(approval) = self.pending_for_call(&state, &ticket)? {
                return Ok(approval);
            }
            let now = self.clock.now();
            let request = match ticket.request_id {
                Some(id) => self.store.request(id)?,
                None => None,
            };
            if let Some(request) = request.as_ref().filter(|r| r.status.is_final()) {
                return Err(ApiError::new(
                    ErrorCode::Denied,
                    format!(
                        "The work request has ended ({}), so this action was denied.",
                        status_name(request.status)
                    ),
                ));
            }
            let (requester, requester_agent) = match &ticket.caller {
                Caller::Owner => (None, None),
                Caller::Member {
                    member_id,
                    via_agent,
                    ..
                } => (Some(*member_id), via_agent.clone()),
            };
            let approval = Approval {
                id: ApprovalId::generate(),
                agent_id: ticket.agent_id,
                request_id: ticket.request_id,
                requester,
                requester_agent,
                purpose: request.as_ref().and_then(|r| purpose(&r.prompt)),
                summary: summarize(&ticket.call),
                call: ticket.call,
                proposed_rule: None,
                status: ApprovalStatus::Pending,
                choice: None,
                created_at: now,
                decided_at: None,
                expires_at: now + APPROVAL_TTL,
            };
            self.store.put_approval(&approval)?;
            let request = match request {
                Some(mut request) if request.status != RequestStatus::AwaitingApproval => {
                    request.status = RequestStatus::AwaitingApproval;
                    request.updated_at = now;
                    self.store.put_request(&request)?;
                    Some(request)
                }
                _ => None,
            };
            let tool_use_id = approval.call.tool_use_id.clone();
            if let Some(tool_use_id) = &tool_use_id {
                state
                    .by_call
                    .insert((ticket.run_id, tool_use_id.clone()), approval.id);
            }
            state.runs.insert(
                approval.id,
                RaisedBy {
                    run_id: ticket.run_id,
                    tool_use_id,
                },
            );
            // Published under the lock, so a decision taken right after cannot
            // reach clients before the approval and request it changes.
            self.events.publish(Event::ApprovalRaised(ApprovalEvent {
                approval: approval.clone(),
            }));
            if let Some(request) = request {
                self.events
                    .publish(Event::RequestUpdate(RequestUpdateEvent { request }));
            }
            approval
        };
        // The card already reached clients through `approval.raised`; a failed
        // notification must not deny the call.
        let _ = self
            .notifier
            .raise(
                AttentionKind::Approval,
                attention(approval.id),
                approval.summary.clone(),
            )
            .await;
        // A decision or expiry that came in while the item was being raised
        // resolved nothing yet; resolve it now so no stale item stays open.
        if let Ok(Some(current)) = self.store.approval(approval.id)
            && current.status != ApprovalStatus::Pending
        {
            let _ = self.notifier.resolve(attention(approval.id)).await;
        }
        Ok(approval)
    }

    /// The owner's decision (`approval_decide`). `denied` for anyone but the
    /// owner; `invalid_params` for the saved-rule choices until TASK-028;
    /// `approval_expired` once 24 hours have passed; `conflict` when already
    /// decided.
    pub async fn decide(&self, caller: &Caller, decide: &ApprovalDecide) -> Result<Approval> {
        if *caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "Only the owner can decide approvals.",
            ));
        }
        let status = match decide.choice {
            ApprovalChoice::AllowOnce => ApprovalStatus::Allowed,
            ApprovalChoice::Deny => ApprovalStatus::Denied,
            ApprovalChoice::KeepAllowingSimilar | ApprovalChoice::AllowAll => {
                return Err(ApiError::new(
                    ErrorCode::InvalidParams,
                    "Saved rules are not available yet (TASK-028); allow once or deny.",
                ));
            }
        };
        self.change(|state, changes| {
            let approval = self.load(decide.approval_id)?;
            let now = self.clock.now();
            let current = approval.status;
            match current {
                ApprovalStatus::Pending if now >= approval.expires_at => {
                    self.finish(state, approval, ApprovalStatus::Expired, None, now, changes)?;
                    Err(expired_error())
                }
                ApprovalStatus::Pending => {
                    let choice = Some(decide.choice);
                    self.finish(state, approval, status, choice, now, changes)?;
                    Ok(changes[0].approval.clone())
                }
                ApprovalStatus::Expired => Err(expired_error()),
                ApprovalStatus::Allowed | ApprovalStatus::Denied => Err(ApiError::new(
                    ErrorCode::Conflict,
                    "This approval was already decided.",
                )),
            }
        })
        .await
    }

    /// Waits until the approval is allowed, denied, or expired, and returns it.
    pub async fn wait(&self, id: ApprovalId) -> Result<Approval> {
        enum Next {
            Done(Box<Approval>),
            Wait(oneshot::Receiver<Approval>),
        }
        let next = self
            .change(|state, changes| {
                let approval = self.load(id)?;
                let now = self.clock.now();
                let current = approval.status;
                Ok(match current {
                    ApprovalStatus::Pending if now >= approval.expires_at => {
                        self.finish(state, approval, ApprovalStatus::Expired, None, now, changes)?;
                        Next::Done(Box::new(changes[0].approval.clone()))
                    }
                    ApprovalStatus::Pending => {
                        let (sender, receiver) = oneshot::channel();
                        state.waiters.entry(id).or_default().push(sender);
                        Next::Wait(receiver)
                    }
                    _ => Next::Done(Box::new(approval)),
                })
            })
            .await?;
        match next {
            Next::Done(approval) => Ok(*approval),
            Next::Wait(receiver) => receiver.await.map_err(|_| {
                ApiError::new(ErrorCode::Internal, "Approvals stopped before a decision.")
            }),
        }
    }

    /// Expires every pending approval whose 24 hours have passed, and returns them.
    pub async fn expire_due(&self) -> Result<Vec<Approval>> {
        let mut expired = Vec::new();
        self.change(|state, changes| {
            let now = self.clock.now();
            let due = self.store.approvals(Some(ApprovalStatus::Pending))?;
            for id in due.iter().filter(|a| now >= a.expires_at).map(|a| a.id) {
                // An earlier one may have closed it along with their request.
                let approval = self.load(id)?;
                if approval.status != ApprovalStatus::Pending {
                    continue;
                }
                let index = changes.len();
                self.finish(state, approval, ApprovalStatus::Expired, None, now, changes)?;
                expired.push(changes[index].approval.clone());
            }
            Ok(())
        })
        .await?;
        Ok(expired)
    }

    /// When the next pending approval expires.
    pub fn next_expiry(&self) -> Result<Option<Timestamp>> {
        Ok(self
            .store
            .approvals(Some(ApprovalStatus::Pending))?
            .iter()
            .map(|a| a.expires_at)
            .min())
    }

    /// Pending approvals the run raised since formationd started, oldest first.
    pub fn pending_for_run(&self, run_id: RunId) -> Vec<ApprovalId> {
        let state = self.lock();
        let mut ids: Vec<ApprovalId> = state
            .runs
            .iter()
            .filter(|(_, raised)| raised.run_id == run_id)
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn load(&self, id: ApprovalId) -> Result<Approval> {
        self.store
            .approval(id)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "No such approval."))
    }

    /// The pending approval for the same call of the same run, if any.
    fn pending_for_call(&self, state: &State, ticket: &ApprovalTicket) -> Result<Option<Approval>> {
        let Some(tool_use_id) = &ticket.call.tool_use_id else {
            return Ok(None);
        };
        let Some(id) = state.by_call.get(&(ticket.run_id, tool_use_id.clone())) else {
            return Ok(None);
        };
        Ok(self.store.approval(*id)?.filter(|approval| {
            approval.status == ApprovalStatus::Pending
                && approval.call == ticket.call
                && approval.agent_id == ticket.agent_id
        }))
    }

    /// Runs a status change under the lock and publishes every change it made,
    /// even when it failed partway, before releasing the lock, so clients see
    /// changes in the order they were stored. Waiters and the notifier are told
    /// after the lock is released.
    async fn change<T>(
        &self,
        apply: impl FnOnce(&mut State, &mut Vec<Change>) -> Result<T>,
    ) -> Result<T> {
        let mut changes = Vec::new();
        let result = {
            let mut state = self.lock();
            let result = apply(&mut state, &mut changes);
            for change in &mut changes {
                self.publish(change);
            }
            result
        };
        for change in changes {
            self.announce(change).await;
        }
        result
    }

    /// Stores the new status, updates the request, and takes the waiters; the
    /// approval's change comes first in `changes`. When this ends the request,
    /// its other pending approvals are denied too, since nothing more from it
    /// may run. Call with the lock held.
    fn finish(
        &self,
        state: &mut State,
        mut approval: Approval,
        status: ApprovalStatus,
        choice: Option<ApprovalChoice>,
        now: Timestamp,
        changes: &mut Vec<Change>,
    ) -> Result<()> {
        approval.status = status;
        approval.choice = choice;
        approval.decided_at = choice.map(|_| now);
        self.store.put_approval(&approval)?;
        let request = self.settle_request(&approval, now)?;
        let ended = request
            .as_ref()
            .filter(|request| request.status.is_final())
            .map(|request| request.id);
        if let Some(raised) = state.runs.remove(&approval.id)
            && let Some(tool_use_id) = raised.tool_use_id
        {
            // The key may already name a later approval for the same tool use ID
            // with another input; leave that one joinable.
            let key = (raised.run_id, tool_use_id);
            if state.by_call.get(&key) == Some(&approval.id) {
                state.by_call.remove(&key);
            }
        }
        let waiters = state.waiters.remove(&approval.id).unwrap_or_default();
        changes.push(Change {
            approval,
            request,
            waiters,
        });
        if let Some(request_id) = ended {
            let pending = self.store.approvals(Some(ApprovalStatus::Pending))?;
            for sibling in pending
                .into_iter()
                .filter(|a| a.request_id == Some(request_id))
            {
                self.finish(state, sibling, ApprovalStatus::Denied, None, now, changes)?;
            }
        }
        Ok(())
    }

    /// Moves the approval's work request after a decision or expiry; returns it
    /// when it changed.
    fn settle_request(&self, approval: &Approval, now: Timestamp) -> Result<Option<WorkRequest>> {
        let Some(request_id) = approval.request_id else {
            return Ok(None);
        };
        let Some(mut request) = self.store.request(request_id)? else {
            return Ok(None);
        };
        if request.status.is_final() {
            return Ok(None);
        }
        match approval.status {
            ApprovalStatus::Allowed => {
                let others_pending = self
                    .store
                    .approvals(Some(ApprovalStatus::Pending))?
                    .iter()
                    .any(|a| a.request_id == Some(request_id) && a.id != approval.id);
                if request.status != RequestStatus::AwaitingApproval || others_pending {
                    return Ok(None);
                }
                request.status = RequestStatus::Running;
            }
            ApprovalStatus::Denied => {
                request.status = RequestStatus::Denied;
                request.error = Some(ApiError::new(
                    ErrorCode::Denied,
                    format!("The owner denied: {}", approval.summary),
                ));
            }
            ApprovalStatus::Expired => {
                request.status = RequestStatus::Expired;
                request.error = Some(ApiError::new(
                    ErrorCode::ApprovalExpired,
                    format!("No answer within 24 hours: {}", approval.summary),
                ));
            }
            ApprovalStatus::Pending => return Ok(None),
        }
        request.updated_at = now;
        self.store.put_request(&request)?;
        Ok(Some(request))
    }

    /// Publishes `approval.update` and, when the request moved, `request.update`.
    /// Call with the lock held.
    fn publish(&self, change: &mut Change) {
        self.events.publish(Event::ApprovalUpdate(ApprovalEvent {
            approval: change.approval.clone(),
        }));
        if let Some(request) = change.request.take() {
            self.events
                .publish(Event::RequestUpdate(RequestUpdateEvent { request }));
        }
    }

    /// Wakes the waiters and resolves the attention item, after the lock is
    /// released.
    async fn announce(&self, change: Change) {
        let Change {
            approval, waiters, ..
        } = change;
        for waiter in waiters {
            // A waiter that went away (its run ended) needs no answer.
            let _ = waiter.send(approval.clone());
        }
        // As in `raise`: a notifier failure leaves a stale inbox item, not a
        // wrong decision.
        let _ = self.notifier.resolve(attention(approval.id)).await;
    }
}

/// The answer to Claude Code for a decided approval. Allowed runs exactly the
/// input the owner saw.
pub fn verdict(approval: &Approval) -> PermissionVerdict {
    match approval.status {
        ApprovalStatus::Allowed => PermissionVerdict::Allow {
            updated_input: approval.call.input.clone(),
        },
        ApprovalStatus::Expired => PermissionVerdict::Deny {
            message: EXPIRED_MESSAGE.into(),
        },
        ApprovalStatus::Denied | ApprovalStatus::Pending => PermissionVerdict::Deny {
            message: DENIED_MESSAGE.into(),
        },
    }
}

/// The exact command, or the tool and what it acts on, for the approval sheet.
pub fn summarize(call: &ToolCall) -> String {
    let field = |name: &str| call.input.get(name).and_then(|v| v.as_str());
    let target = match call.tool_name.as_str() {
        // A call without a command shows its whole input, never an empty card.
        "Bash" => match field("command") {
            Some(command) if !command.trim().is_empty() => return command.to_owned(),
            _ => None,
        },
        "Read" | "Write" | "Edit" | "MultiEdit" => field("file_path"),
        "NotebookEdit" | "NotebookRead" => field("notebook_path"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "LS" => field("path"),
        "Glob" | "Grep" => {
            let pattern = field("pattern").unwrap_or_default();
            return match field("path") {
                Some(path) => format!("{} {pattern} in {path}", call.tool_name),
                None => format!("{} {pattern}", call.tool_name),
            };
        }
        _ => None,
    };
    match target {
        Some(target) => format!("{} {target}", call.tool_name),
        None => format!("{} {}", call.tool_name, call.input),
    }
}

/// The first line of the request's prompt, shortened.
fn purpose(prompt: &str) -> Option<String> {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    if line.chars().count() <= PURPOSE_MAX_CHARS {
        return Some(line.to_owned());
    }
    let mut short: String = line.chars().take(PURPOSE_MAX_CHARS - 1).collect();
    short.push('…');
    Some(short)
}

fn attention(approval_id: ApprovalId) -> AttentionTarget {
    AttentionTarget::Approval { approval_id }
}

fn expired_error() -> ApiError {
    ApiError::new(
        ErrorCode::ApprovalExpired,
        "This approval expired after 24 hours without an answer.",
    )
}

fn status_name(status: RequestStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}"))
}

#[cfg(test)]
mod tests;
