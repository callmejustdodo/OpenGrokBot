//! The owner's side: per-agent queues and the requests teammates send to this Mac's
//! agents.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::sync::{Notify, broadcast};

use formation_proto::common::{
    AgentId, ApiError, ErrorCode, MemberId, MessageId, RequestId, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::harness::AgentQueueEvent;
use formation_proto::peer::PeerRequestCreate;
use formation_proto::policy::{Caller, MAX_CHAIN_DEPTH};
use formation_proto::request::{
    QueueItem, QueueSource, RequestStatus, RequestUpdateEvent, WorkRequest,
};

use super::status::{check_transition, status_name};
use crate::ports::{CollabStore, EventSink, Policy, RequestQueue, Result, RunProgress};

/// Updates waiting for a slow subscriber before it starts missing them.
const UPDATES_CAPACITY: usize = 256;

/// Implements [`RequestQueue`] for A's executor and keeps the lifecycle of the work
/// requests teammates send to this Mac's agents.
///
/// - One ordered queue per agent holds owner messages, schedule runs, and
///   requests. It is stored with [`RequestRepo`](crate::ports::RequestRepo) so a
///   restart finds it again, and [`RequestQueue::next`] serves one consumer per
///   agent.
/// - A request is created at most once per (requester, idempotency key), checked
///   against the agent's share settings, the chain depth (FR-019), and
///   [`Policy::check_request`], then queued with its prompt exactly as sent.
/// - Every change is published as `request.update` or `agent.queue` and sent to
///   [`Requests::subscribe`] so the peer server can tell the requester.
///
/// One lock orders every change, so a cancel and the executor taking the same item
/// cannot both win. Repositories are called inside it; they are short SQLite
/// transactions.
pub struct Requests {
    collab: Arc<dyn CollabStore>,
    policy: Arc<dyn Policy>,
    events: Arc<dyn EventSink>,
    updates: broadcast::Sender<WorkRequest>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Loaded from the store on first use of each agent.
    queues: HashMap<AgentId, AgentQueue>,
    /// Items `next` handed to the executor that have not reported an end yet.
    taken: HashMap<MessageId, Option<RequestId>>,
}

struct AgentQueue {
    /// Ordered by `queued_at`, which increases strictly within a queue.
    items: VecDeque<QueueItem>,
    wake: Arc<Notify>,
    consuming: bool,
}

impl Requests {
    pub fn new(
        collab: Arc<dyn CollabStore>,
        policy: Arc<dyn Policy>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            collab,
            policy,
            events,
            updates: broadcast::channel(UPDATES_CAPACITY).0,
            state: Mutex::new(State::default()),
        }
    }

    /// Every change to a request on this Mac, for the peer server to send to its
    /// requester as `PeerEvent::RequestUpdate`. Receivers may lag and miss updates;
    /// the requester can always read the current request with `GET /v1/requests/{id}`.
    pub fn subscribe(&self) -> broadcast::Receiver<WorkRequest> {
        self.updates.subscribe()
    }

    /// `POST /v1/requests` from `requester`, already authenticated by the peer
    /// server. A repeated idempotency key, or a repeated request ID, returns the
    /// stored request as it is now; the same key with another agent or prompt is a
    /// `conflict`. Otherwise checks, in order: the agent is shared
    /// (`agent_not_shared`) and accepts requests (`requests_not_accepted`), the
    /// chain depth is at most [`MAX_CHAIN_DEPTH`] (`rate_limited`), and
    /// [`Policy::check_request`]. Returns the request once it is queued.
    pub async fn create(
        &self,
        requester: MemberId,
        body: PeerRequestCreate,
    ) -> Result<WorkRequest> {
        require_text("prompt", &body.prompt)?;
        require_text("idempotency_key", &body.idempotency_key)?;
        if let Some(existing) = self.existing(requester, &body)? {
            return Ok(existing);
        }

        let share = self.collab.share(body.agent_id)?;
        if !share.shared {
            return Err(error(
                ErrorCode::AgentNotShared,
                "the agent is not shared with the team",
            ));
        }
        if !share.accepts_requests {
            return Err(error(
                ErrorCode::RequestsNotAccepted,
                "the agent does not accept work requests",
            ));
        }
        check_chain_depth(body.chain.depth)?;
        let caller = Caller::Member {
            member_id: requester,
            via_agent: body.via_agent.clone(),
            chain: body.chain,
        };
        self.policy.check_request(&caller, body.agent_id).await?;

        let owner = self
            .collab
            .me()?
            .ok_or_else(|| error(ErrorCode::Internal, "this Mac has no team"))?;
        let at = now();
        let request = WorkRequest {
            id: body.request_id,
            requester,
            requester_agent: body.via_agent.clone(),
            chain: body.chain,
            owner,
            agent_id: body.agent_id,
            prompt: body.prompt.clone(),
            idempotency_key: body.idempotency_key.clone(),
            status: RequestStatus::Received,
            result: None,
            error: None,
            created_at: at,
            updated_at: at,
        };

        let mut state = self.lock();
        let (stored, inserted) = self.collab.insert_request(&request)?;
        if !inserted {
            return replay(stored, &body);
        }
        self.publish_request(&stored);
        self.enqueue_request(&mut state.queues, stored)
    }

    /// `GET /v1/requests/{id}`: only the requester can read it; anyone else gets
    /// `not_found`.
    pub fn get(&self, requester: MemberId, id: RequestId) -> Result<WorkRequest> {
        self.collab
            .request(id)?
            .filter(|request| request.requester == requester)
            .ok_or_else(request_not_found)
    }

    /// Cancels before the run starts: `POST /v1/requests/{id}/cancel` from the
    /// requester (`by` is `Some`), or the owner on this Mac (`None`). Canceling a
    /// canceled request returns it unchanged, so retries are safe. Once the
    /// executor has taken the request, or it has finished, the answer is `conflict`.
    pub fn cancel(&self, by: Option<MemberId>, id: RequestId) -> Result<WorkRequest> {
        let me = self.collab.me()?;
        let mut state = self.lock();
        let mut request = self
            .collab
            .request(id)?
            .filter(|request| match by {
                Some(requester) => request.requester == requester,
                None => Some(request.owner) == me,
            })
            .ok_or_else(request_not_found)?;
        match request.status {
            RequestStatus::Canceled => return Ok(request),
            RequestStatus::Received => {}
            RequestStatus::Queued => {
                let queue = load_queue(&*self.collab, &mut state.queues, request.agent_id)?;
                let Some(position) = position_of_request(queue, id) else {
                    return Err(already_started());
                };
                // Store the cancel before dropping the item, so a failed write leaves
                // the request queued and cancelable rather than queued with no item.
                self.apply(&mut request, RequestStatus::Canceled, None, None)?;
                // A stored item left behind is harmless: `pop` drops items whose
                // request is no longer queued.
                let _ = self.collab.remove_queue_item(queue.items[position].id);
                queue.items.remove(position);
                self.publish_queue(request.agent_id, queue);
                return Ok(request);
            }
            RequestStatus::Running | RequestStatus::AwaitingApproval => {
                return Err(already_started());
            }
            status => {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("the request is already {}", status_name(status)),
                ));
            }
        }
        self.apply(&mut request, RequestStatus::Canceled, None, None)?;
        Ok(request)
    }

    /// For the approvals module (TASK-025), on a request that runs: `running` to
    /// `awaiting_approval` while a tool call waits for the owner, then back to
    /// `running` after an allow, or to `denied` or `expired` (FR-015). Other moves
    /// are `invalid_params` (queueing, canceling, and run results go through
    /// [`Requests::create`], [`Requests::cancel`], and [`RequestQueue::report`]), or
    /// `conflict` when the request is not in the status the move starts from.
    pub fn set_status(
        &self,
        id: RequestId,
        to: RequestStatus,
        error: Option<ApiError>,
    ) -> Result<WorkRequest> {
        let from = match to {
            RequestStatus::AwaitingApproval => RequestStatus::Running,
            RequestStatus::Running | RequestStatus::Denied | RequestStatus::Expired => {
                RequestStatus::AwaitingApproval
            }
            _ => {
                return Err(self::error(
                    ErrorCode::InvalidParams,
                    format!("set_status cannot set {}", status_name(to)),
                ));
            }
        };
        let me = self.collab.me()?;
        let _state = self.lock();
        let mut request = self
            .collab
            .request(id)?
            .filter(|request| Some(request.owner) == me)
            .ok_or_else(request_not_found)?;
        if request.status != from {
            return Err(self::error(
                ErrorCode::Conflict,
                format!(
                    "only a {} request can become {}",
                    status_name(from),
                    status_name(to)
                ),
            ));
        }
        self.apply(&mut request, to, None, error)?;
        Ok(request)
    }

    /// Rebuilds request state after formationd starts (backend architecture,
    /// "Recovery"): a request stored but not yet queued is queued; a queued request
    /// whose item the executor had already taken, and a running one, become
    /// `failed`, because the run may have started and a request must never run twice
    /// (NFR-007). Returns the requests it changed. Call it once at start, before
    /// the executor takes items: it cannot tell an item taken by this process from
    /// one taken before the restart.
    pub fn recover(&self) -> Result<Vec<WorkRequest>> {
        let Some(me) = self.collab.me()? else {
            return Ok(Vec::new());
        };
        let mut state = self.lock();
        let mut changed = Vec::new();
        for mut request in self.collab.requests()? {
            if request.owner != me {
                continue;
            }
            match request.status {
                RequestStatus::Received => {
                    changed.push(self.enqueue_request(&mut state.queues, request)?);
                }
                RequestStatus::Queued => {
                    let queue = load_queue(&*self.collab, &mut state.queues, request.agent_id)?;
                    if position_of_request(queue, request.id).is_none() {
                        self.apply(&mut request, RequestStatus::Failed, None, Some(restarted()))?;
                        changed.push(request);
                    }
                }
                RequestStatus::Running | RequestStatus::AwaitingApproval => {
                    self.apply(&mut request, RequestStatus::Failed, None, Some(restarted()))?;
                    changed.push(request);
                }
                _ => {}
            }
        }
        Ok(changed)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The stored request a retry refers to, by request ID or by idempotency key.
    fn existing(
        &self,
        requester: MemberId,
        body: &PeerRequestCreate,
    ) -> Result<Option<WorkRequest>> {
        if let Some(request) = self.collab.request(body.request_id)? {
            if request.requester == requester && request.idempotency_key == body.idempotency_key {
                return replay(request, body).map(Some);
            }
            return Err(error(ErrorCode::Conflict, "the request ID is already used"));
        }
        self.collab
            .requests()?
            .into_iter()
            .find(|request| {
                request.requester == requester && request.idempotency_key == body.idempotency_key
            })
            .map(|request| replay(request, body))
            .transpose()
    }

    /// Moves a stored `received` request to `queued` and appends its item. Called
    /// with the lock held.
    fn enqueue_request(
        &self,
        queues: &mut HashMap<AgentId, AgentQueue>,
        mut request: WorkRequest,
    ) -> Result<WorkRequest> {
        let queue = load_queue(&*self.collab, queues, request.agent_id)?;
        let item = QueueItem {
            id: MessageId::generate(),
            agent_id: request.agent_id,
            source: QueueSource::Request {
                request_id: request.id,
                requester: request.requester,
            },
            text: request.prompt.clone(),
            queued_at: next_queued_at(queue),
        };
        self.apply(&mut request, RequestStatus::Queued, None, None)?;
        if let Err(failure) = self.collab.put_queue_item(&item) {
            // Best effort: a queued request without an item would never run.
            let _ = self.apply(
                &mut request,
                RequestStatus::Failed,
                None,
                Some(failure.clone()),
            );
            return Err(failure);
        }
        queue.items.push_back(item);
        queue.wake.notify_one();
        self.publish_queue(request.agent_id, queue);
        Ok(request)
    }

    /// Checks the move, stores it, and publishes it. Called with the lock held.
    fn apply(
        &self,
        request: &mut WorkRequest,
        to: RequestStatus,
        result: Option<String>,
        error: Option<ApiError>,
    ) -> Result<()> {
        check_transition(request.status, to)?;
        let mut next = request.clone();
        next.status = to;
        if result.is_some() {
            next.result = result;
        }
        if error.is_some() {
            next.error = error;
        }
        next.updated_at = now().max(request.updated_at);
        self.collab.put_request(&next)?;
        *request = next;
        self.publish_request(request);
        Ok(())
    }

    fn publish_request(&self, request: &WorkRequest) {
        self.events
            .publish(Event::RequestUpdate(RequestUpdateEvent {
                request: request.clone(),
            }));
        // No receiver is fine: no teammate is connected right now.
        let _ = self.updates.send(request.clone());
    }

    fn publish_queue(&self, agent_id: AgentId, queue: &AgentQueue) {
        self.events.publish(Event::AgentQueue(AgentQueueEvent {
            agent_id,
            items: queue.items.iter().cloned().collect(),
        }));
    }

    /// Removes and returns the agent's first item, if any, and remembers it for
    /// `report`. An item whose request is no longer `queued` is dropped instead, so
    /// a request that ended while it waited never runs.
    fn pop(&self, agent_id: AgentId) -> Result<Option<QueueItem>> {
        let mut state = self.lock();
        let State { queues, taken } = &mut *state;
        let queue = load_queue(&*self.collab, queues, agent_id)?;
        while let Some(item) = queue.items.front().cloned() {
            let request_id = match item.source {
                QueueSource::Request { request_id, .. } => Some(request_id),
                QueueSource::Owner | QueueSource::Schedule { .. } => None,
            };
            let runnable = match request_id {
                Some(request_id) => self
                    .collab
                    .request(request_id)?
                    .is_some_and(|request| request.status == RequestStatus::Queued),
                None => true,
            };
            self.collab.remove_queue_item(item.id)?;
            queue.items.pop_front();
            self.publish_queue(agent_id, queue);
            if runnable {
                taken.insert(item.id, request_id);
                return Ok(Some(item));
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl RequestQueue for Requests {
    /// For owner messages and schedule runs. Requests enter through
    /// [`Requests::create`], so a `Request` source is `invalid_params`.
    async fn enqueue(
        &self,
        agent_id: AgentId,
        source: QueueSource,
        text: String,
    ) -> Result<QueueItem> {
        reject_request_source(&source)?;
        require_text("text", &text)?;
        let mut state = self.lock();
        let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
        let item = QueueItem {
            id: MessageId::generate(),
            agent_id,
            source,
            text,
            queued_at: next_queued_at(queue),
        };
        self.collab.put_queue_item(&item)?;
        queue.items.push_back(item.clone());
        queue.wake.notify_one();
        self.publish_queue(agent_id, queue);
        Ok(item)
    }

    async fn items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>> {
        let mut state = self.lock();
        let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
        Ok(queue.items.iter().cloned().collect())
    }

    /// A teammate's request is delivered exactly as they saw it (FR-014), so editing
    /// one is a `conflict`.
    async fn edit(&self, agent_id: AgentId, id: MessageId, text: String) -> Result<QueueItem> {
        require_text("text", &text)?;
        let mut state = self.lock();
        let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
        let position = position_of(queue, id)?;
        if matches!(queue.items[position].source, QueueSource::Request { .. }) {
            return Err(error(
                ErrorCode::Conflict,
                "a teammate's request is delivered exactly as sent",
            ));
        }
        let mut item = queue.items[position].clone();
        item.text = text;
        self.collab.put_queue_item(&item)?;
        queue.items[position] = item.clone();
        self.publish_queue(agent_id, queue);
        Ok(item)
    }

    /// Withdrawing a teammate's request cancels it, with the reason in `error`.
    async fn withdraw(&self, agent_id: AgentId, id: MessageId) -> Result<()> {
        let mut state = self.lock();
        let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
        let position = position_of(queue, id)?;
        match queue.items[position].source {
            QueueSource::Request { request_id, .. } => {
                let mut request = self
                    .collab
                    .request(request_id)?
                    .ok_or_else(request_not_found)?;
                // As in `cancel`: store the cancel first; a stored item left behind
                // is dropped by `pop`.
                self.apply(
                    &mut request,
                    RequestStatus::Canceled,
                    None,
                    Some(error(
                        ErrorCode::Denied,
                        "the owner withdrew the request before it started",
                    )),
                )?;
                let _ = self.collab.remove_queue_item(id);
            }
            QueueSource::Owner | QueueSource::Schedule { .. } => {
                self.collab.remove_queue_item(id)?;
            }
        }
        queue.items.remove(position);
        self.publish_queue(agent_id, queue);
        Ok(())
    }

    /// Only the owner writes to the main session (#14), so a teammate's request
    /// cannot steer it: `conflict`.
    async fn take(&self, agent_id: AgentId, id: MessageId) -> Result<QueueItem> {
        let mut state = self.lock();
        let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
        let position = position_of(queue, id)?;
        if matches!(queue.items[position].source, QueueSource::Request { .. }) {
            return Err(error(
                ErrorCode::Conflict,
                "a teammate's request runs in its own fork and cannot steer the main session",
            ));
        }
        self.collab.remove_queue_item(id)?;
        let item = queue.items.remove(position).expect("position is in range");
        self.publish_queue(agent_id, queue);
        Ok(item)
    }

    async fn restore(&self, agent_id: AgentId, item: QueueItem) -> Result<()> {
        if item.agent_id != agent_id {
            return Err(error(
                ErrorCode::InvalidParams,
                "the item belongs to another agent",
            ));
        }
        reject_request_source(&item.source)?;
        let mut state = self.lock();
        let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
        if queue.items.iter().any(|queued| queued.id == item.id) {
            return Err(error(ErrorCode::Conflict, "the item is already queued"));
        }
        let position = queue
            .items
            .partition_point(|queued| queued.queued_at <= item.queued_at);
        self.collab.put_queue_item(&item)?;
        queue.items.insert(position, item);
        queue.wake.notify_one();
        self.publish_queue(agent_id, queue);
        Ok(())
    }

    /// A second consumer for the same agent while one waits is a `conflict`.
    async fn next(&self, agent_id: AgentId) -> Result<QueueItem> {
        let (wake, _consumer) = {
            let mut state = self.lock();
            let queue = load_queue(&*self.collab, &mut state.queues, agent_id)?;
            if queue.consuming {
                return Err(error(
                    ErrorCode::Conflict,
                    "another consumer already takes this agent's queue",
                ));
            }
            queue.consuming = true;
            (
                queue.wake.clone(),
                Consumer {
                    requests: self,
                    agent_id,
                },
            )
        };
        loop {
            if let Some(item) = self.pop(agent_id)? {
                return Ok(item);
            }
            wake.notified().await;
        }
    }

    /// `not_found` for an item `next` did not hand out. A request that already
    /// ended (denied or expired while a tool call waited) keeps its status.
    async fn report(&self, id: MessageId, progress: RunProgress) -> Result<()> {
        let mut state = self.lock();
        let request_id = *state
            .taken
            .get(&id)
            .ok_or_else(|| error(ErrorCode::NotFound, "no running item has this ID"))?;
        let ends = !matches!(progress, RunProgress::Started);
        if let Some(request_id) = request_id {
            let mut request = self
                .collab
                .request(request_id)?
                .ok_or_else(request_not_found)?;
            if !request.status.is_final() {
                match progress {
                    RunProgress::Started => {
                        if request.status == RequestStatus::Queued {
                            self.apply(&mut request, RequestStatus::Running, None, None)?;
                        }
                    }
                    RunProgress::Completed { result } => {
                        // The run ended without a `Started` report, or while an
                        // approval was being recorded: pass through `running` so the
                        // request still completes instead of staying stuck.
                        if request.status != RequestStatus::Running {
                            self.apply(&mut request, RequestStatus::Running, None, None)?;
                        }
                        self.apply(&mut request, RequestStatus::Completed, result, None)?;
                    }
                    RunProgress::Failed { error } => {
                        self.apply(&mut request, RequestStatus::Failed, None, Some(error))?;
                    }
                    RunProgress::Interrupted => {
                        self.apply(
                            &mut request,
                            RequestStatus::Failed,
                            None,
                            Some(error(
                                ErrorCode::Internal,
                                "the run was interrupted before it finished",
                            )),
                        )?;
                    }
                }
            }
        }
        if ends {
            state.taken.remove(&id);
        }
        Ok(())
    }
}

/// Marks the agent's queue free again when `next` returns or is dropped.
struct Consumer<'a> {
    requests: &'a Requests,
    agent_id: AgentId,
}

impl Drop for Consumer<'_> {
    fn drop(&mut self) {
        if let Some(queue) = self.requests.lock().queues.get_mut(&self.agent_id) {
            queue.consuming = false;
        }
    }
}

fn load_queue<'a>(
    collab: &dyn CollabStore,
    queues: &'a mut HashMap<AgentId, AgentQueue>,
    agent_id: AgentId,
) -> Result<&'a mut AgentQueue> {
    match queues.entry(agent_id) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(entry) => {
            let mut items = collab.queue_items(agent_id)?;
            items.sort_by_key(|item| item.queued_at);
            Ok(entry.insert(AgentQueue {
                items: items.into(),
                wake: Arc::new(Notify::new()),
                consuming: false,
            }))
        }
    }
}

/// Now, or just after the last item, so `queued_at` orders the queue.
fn next_queued_at(queue: &AgentQueue) -> Timestamp {
    let at = now();
    match queue.items.back() {
        Some(last) if last.queued_at >= at => last.queued_at + Duration::from_micros(1),
        _ => at,
    }
}

fn position_of(queue: &AgentQueue, id: MessageId) -> Result<usize> {
    queue
        .items
        .iter()
        .position(|item| item.id == id)
        .ok_or_else(|| error(ErrorCode::NotFound, "no queued message has this ID"))
}

fn position_of_request(queue: &AgentQueue, id: RequestId) -> Option<usize> {
    queue.items.iter().position(
        |item| matches!(item.source, QueueSource::Request { request_id, .. } if request_id == id),
    )
}

/// A retry returns the stored request; the same key for different work is refused.
fn replay(stored: WorkRequest, body: &PeerRequestCreate) -> Result<WorkRequest> {
    if stored.agent_id == body.agent_id && stored.prompt == body.prompt {
        Ok(stored)
    } else {
        Err(error(
            ErrorCode::Conflict,
            "the idempotency key was already used for a different request",
        ))
    }
}

pub(super) fn check_chain_depth(depth: u8) -> Result<()> {
    if depth > MAX_CHAIN_DEPTH {
        Err(error(
            ErrorCode::RateLimited,
            format!("agent-to-agent chains stop at depth {MAX_CHAIN_DEPTH}"),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn require_text(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(error(
            ErrorCode::InvalidParams,
            format!("{field} must not be empty"),
        ))
    } else {
        Ok(())
    }
}

fn reject_request_source(source: &QueueSource) -> Result<()> {
    if matches!(source, QueueSource::Request { .. }) {
        Err(error(
            ErrorCode::InvalidParams,
            "work requests are queued when they are created",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn now() -> Timestamp {
    SystemTime::now().into()
}

pub(super) fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}

pub(super) fn request_not_found() -> ApiError {
    error(ErrorCode::NotFound, "no such request")
}

fn already_started() -> ApiError {
    error(ErrorCode::Conflict, "the request has already started")
}

fn restarted() -> ApiError {
    error(
        ErrorCode::Internal,
        "formationd restarted after the run was taken; it was not run again",
    )
}
