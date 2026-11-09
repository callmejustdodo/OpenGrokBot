//! Owner: B. `FakeRequestQueue`: per-agent ordered queues in memory, with the
//! work requests behind them, for A's executor (TASK-024) and its tests.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{broadcast, watch};

use formation_proto::common::{AgentId, ApiError, ErrorCode, MessageId, RequestId, Timestamp};
use formation_proto::request::{QueueItem, QueueSource, RequestStatus, WorkRequest};

use crate::ports::{RequestQueue, Result, RunProgress};

use super::{error, later, lock, now};

/// One call to the fake, with its arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueCall {
    Enqueue {
        agent_id: AgentId,
        source: QueueSource,
        text: String,
    },
    Items(AgentId),
    Edit {
        agent_id: AgentId,
        id: MessageId,
        text: String,
    },
    Withdraw {
        agent_id: AgentId,
        id: MessageId,
    },
    Take {
        agent_id: AgentId,
        id: MessageId,
    },
    Restore {
        agent_id: AgentId,
        item: QueueItem,
    },
    Next(AgentId),
    Report {
        id: MessageId,
        progress: RunProgress,
    },
}

/// A [`RequestQueue`] method, for [`FakeRequestQueue::fail_next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueMethod {
    Enqueue,
    Items,
    Edit,
    Withdraw,
    Take,
    Restore,
    Next,
    Report,
}

/// A [`RequestQueue`] in memory.
///
/// Items keep their order by `queued_at`, which the fake keeps strictly increasing.
/// [`RequestQueue::next`] waits for an item and allows one waiting consumer per
/// agent: a second concurrent `next` for the same agent gets `conflict`. An item
/// handed out by `next` or `take` is in flight until a final
/// [`RequestQueue::report`] (`Completed`, `Failed`, `Interrupted`) or `restore`.
/// Every report is recorded and a final one always finishes its item, even when
/// its request was already made final elsewhere (for example canceled with
/// [`FakeRequestQueue::set_request_status`]); that request then stays as it is.
///
/// Work requests registered with [`FakeRequestQueue::track_request`] (or queued
/// with [`FakeRequestQueue::enqueue_request`]) follow their items: `queued` when
/// queued, `running` on `Started`, `completed` with the result, `canceled` when
/// withdrawn, and on `Failed` `denied` for a `denied` error, `expired` for
/// `approval_expired`, `failed` otherwise. `Interrupted` marks them `failed`, as
/// recovery does. Each change is sent to [`FakeRequestQueue::subscribe_requests`]
/// (the "tell the requester" part) in the order it happened, also across
/// threads; each queue change is recorded as an `agent.queue` publication.
pub struct FakeRequestQueue {
    state: Mutex<QueueState>,
    changes: watch::Sender<u64>,
    request_updates: broadcast::Sender<WorkRequest>,
}

#[derive(Default)]
struct QueueState {
    queues: HashMap<AgentId, Vec<QueueItem>>,
    consumers: HashSet<AgentId>,
    in_flight: HashMap<MessageId, QueueItem>,
    requests: HashMap<RequestId, WorkRequest>,
    progress: Vec<(MessageId, RunProgress)>,
    published: Vec<(AgentId, Vec<QueueItem>)>,
    failures: HashMap<QueueMethod, VecDeque<ApiError>>,
    last_queued_at: Option<Timestamp>,
    calls: Vec<QueueCall>,
}

impl Default for FakeRequestQueue {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changes: watch::channel(0).0,
            request_updates: broadcast::channel(256).0,
        }
    }
}

impl std::fmt::Debug for FakeRequestQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("FakeRequestQueue")
            .field("queues", &state.queues)
            .field("in_flight", &state.in_flight)
            .finish_non_exhaustive()
    }
}

impl FakeRequestQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next call of `method` fails with `error`. Failures queue up.
    pub fn fail_next(&self, method: QueueMethod, error: ApiError) {
        lock(&self.state)
            .failures
            .entry(method)
            .or_default()
            .push_back(error);
    }

    /// Follows `request` through its queue item from now on.
    pub fn track_request(&self, request: WorkRequest) {
        lock(&self.state).requests.insert(request.id, request);
    }

    /// Tracks `request` and queues its prompt for its agent, as the requests module
    /// does once the policy accepts it.
    pub async fn enqueue_request(&self, request: WorkRequest) -> Result<QueueItem> {
        let source = QueueSource::Request {
            request_id: request.id,
            requester: request.requester,
        };
        let (agent_id, text) = (request.agent_id, request.prompt.clone());
        self.track_request(request);
        self.enqueue(agent_id, source, text).await
    }

    pub fn request(&self, id: RequestId) -> Option<WorkRequest> {
        lock(&self.state).requests.get(&id).cloned()
    }

    /// Moves a tracked request to `status` and tells the requester, for states the
    /// queue does not drive itself (for example `awaiting_approval` while the gate
    /// waits). `conflict` once the request is final.
    pub fn set_request_status(&self, id: RequestId, status: RequestStatus) -> Result<WorkRequest> {
        let mut state = lock(&self.state);
        let request = state.update_request(id, status, None, None)?;
        self.notify(state, Some(request.clone()));
        Ok(request)
    }

    /// Request updates, as the requester's Mac would receive them.
    pub fn subscribe_requests(&self) -> broadcast::Receiver<WorkRequest> {
        self.request_updates.subscribe()
    }

    /// Items handed out by `next` or `take` and not finished yet.
    pub fn in_flight(&self) -> Vec<QueueItem> {
        let mut items: Vec<_> = lock(&self.state).in_flight.values().cloned().collect();
        items.sort_by_key(|item| item.queued_at);
        items
    }

    /// Every report, in order.
    pub fn progress(&self) -> Vec<(MessageId, RunProgress)> {
        lock(&self.state).progress.clone()
    }

    /// Every `agent.queue` publication: the agent and its queue afterwards.
    pub fn published(&self) -> Vec<(AgentId, Vec<QueueItem>)> {
        lock(&self.state).published.clone()
    }

    pub fn calls(&self) -> Vec<QueueCall> {
        lock(&self.state).calls.clone()
    }

    fn begin(&self, method: QueueMethod, call: QueueCall) -> Result<MutexGuard<'_, QueueState>> {
        let mut state = lock(&self.state);
        state.calls.push(call);
        if let Some(error) = state
            .failures
            .get_mut(&method)
            .and_then(VecDeque::pop_front)
        {
            return Err(error);
        }
        Ok(state)
    }

    /// Sends the request updates while `state` is still locked, so updates from
    /// different threads reach subscribers in the order they happened, then
    /// unlocks and wakes waiting consumers. `broadcast::Sender::send` never takes
    /// our lock.
    fn notify(
        &self,
        state: MutexGuard<'_, QueueState>,
        updates: impl IntoIterator<Item = WorkRequest>,
    ) {
        for request in updates {
            // No subscriber is fine; updates are not kept.
            let _ = self.request_updates.send(request);
        }
        drop(state);
        self.changes.send_modify(|version| *version += 1);
    }
}

impl QueueState {
    fn stamp(&mut self) -> Timestamp {
        let at = match self.last_queued_at {
            Some(last) if now() <= last => later(last, Duration::from_micros(1)),
            _ => now(),
        };
        self.last_queued_at = Some(at);
        at
    }

    fn publish(&mut self, agent_id: AgentId) {
        let items = self.queues.get(&agent_id).cloned().unwrap_or_default();
        self.published.push((agent_id, items));
    }

    fn position(&self, agent_id: AgentId, id: MessageId) -> Result<usize> {
        self.queues
            .get(&agent_id)
            .and_then(|items| items.iter().position(|item| item.id == id))
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no queued message {id}")))
    }

    /// Updates the request behind `source`, if it is tracked and not final.
    fn follow(
        &mut self,
        source: &QueueSource,
        status: RequestStatus,
        result: Option<String>,
        failure: Option<ApiError>,
    ) -> Result<Option<WorkRequest>> {
        match source {
            QueueSource::Request { request_id, .. } if self.requests.contains_key(request_id) => {
                self.update_request(*request_id, status, result, failure)
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    fn update_request(
        &mut self,
        id: RequestId,
        status: RequestStatus,
        result: Option<String>,
        failure: Option<ApiError>,
    ) -> Result<WorkRequest> {
        let request = self
            .requests
            .get_mut(&id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no tracked request {id}")))?;
        if request.status.is_final() {
            return Err(error(
                ErrorCode::Conflict,
                format!("request {id} is already {:?}", request.status),
            ));
        }
        request.status = status;
        if result.is_some() {
            request.result = result;
        }
        if failure.is_some() {
            request.error = failure;
        }
        request.updated_at = now();
        Ok(request.clone())
    }
}

#[async_trait]
impl RequestQueue for FakeRequestQueue {
    async fn enqueue(
        &self,
        agent_id: AgentId,
        source: QueueSource,
        text: String,
    ) -> Result<QueueItem> {
        let mut state = self.begin(
            QueueMethod::Enqueue,
            QueueCall::Enqueue {
                agent_id,
                source,
                text: text.clone(),
            },
        )?;
        let update = state.follow(&source, RequestStatus::Queued, None, None)?;
        let item = QueueItem {
            id: MessageId::generate(),
            agent_id,
            source,
            text,
            queued_at: state.stamp(),
        };
        state.queues.entry(agent_id).or_default().push(item.clone());
        state.publish(agent_id);
        self.notify(state, update);
        Ok(item)
    }

    async fn items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>> {
        let state = self.begin(QueueMethod::Items, QueueCall::Items(agent_id))?;
        Ok(state.queues.get(&agent_id).cloned().unwrap_or_default())
    }

    async fn edit(&self, agent_id: AgentId, id: MessageId, text: String) -> Result<QueueItem> {
        let mut state = self.begin(
            QueueMethod::Edit,
            QueueCall::Edit {
                agent_id,
                id,
                text: text.clone(),
            },
        )?;
        let index = state.position(agent_id, id)?;
        let item = &mut state.queues.get_mut(&agent_id).expect("queue")[index];
        item.text = text;
        let item = item.clone();
        state.publish(agent_id);
        self.notify(state, None);
        Ok(item)
    }

    /// A withdrawn work request is `canceled`.
    async fn withdraw(&self, agent_id: AgentId, id: MessageId) -> Result<()> {
        let mut state = self.begin(QueueMethod::Withdraw, QueueCall::Withdraw { agent_id, id })?;
        let index = state.position(agent_id, id)?;
        let source = state.queues[&agent_id][index].source;
        let update = state.follow(&source, RequestStatus::Canceled, None, None)?;
        state
            .queues
            .get_mut(&agent_id)
            .expect("queue")
            .remove(index);
        state.publish(agent_id);
        self.notify(state, update);
        Ok(())
    }

    async fn take(&self, agent_id: AgentId, id: MessageId) -> Result<QueueItem> {
        let mut state = self.begin(QueueMethod::Take, QueueCall::Take { agent_id, id })?;
        let index = state.position(agent_id, id)?;
        let item = state
            .queues
            .get_mut(&agent_id)
            .expect("queue")
            .remove(index);
        state.in_flight.insert(item.id, item.clone());
        state.publish(agent_id);
        self.notify(state, None);
        Ok(item)
    }

    /// `invalid_params` for an item of another agent; `conflict` when an item with
    /// the same ID is already queued.
    async fn restore(&self, agent_id: AgentId, item: QueueItem) -> Result<()> {
        let mut state = self.begin(
            QueueMethod::Restore,
            QueueCall::Restore {
                agent_id,
                item: item.clone(),
            },
        )?;
        if item.agent_id != agent_id {
            return Err(error(
                ErrorCode::InvalidParams,
                format!("message {} belongs to agent {}", item.id, item.agent_id),
            ));
        }
        if state.position(agent_id, item.id).is_ok() {
            return Err(error(
                ErrorCode::Conflict,
                format!("message {} is already queued", item.id),
            ));
        }
        state.in_flight.remove(&item.id);
        let queue = state.queues.entry(agent_id).or_default();
        let index = queue
            .iter()
            .position(|queued| queued.queued_at > item.queued_at)
            .unwrap_or(queue.len());
        queue.insert(index, item);
        state.publish(agent_id);
        self.notify(state, None);
        Ok(())
    }

    async fn next(&self, agent_id: AgentId) -> Result<QueueItem> {
        let mut changes = self.changes.subscribe();
        let _consumer = {
            let mut state = self.begin(QueueMethod::Next, QueueCall::Next(agent_id))?;
            if !state.consumers.insert(agent_id) {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("agent {agent_id} already has a consumer waiting"),
                ));
            }
            Consumer {
                queue: self,
                agent_id,
            }
        };
        loop {
            {
                let mut state = lock(&self.state);
                let queue = state.queues.entry(agent_id).or_default();
                if !queue.is_empty() {
                    let item = queue.remove(0);
                    state.in_flight.insert(item.id, item.clone());
                    state.publish(agent_id);
                    self.notify(state, None);
                    return Ok(item);
                }
            }
            // The sender lives in `self`, so this only returns on a change.
            let _ = changes.changed().await;
        }
    }

    /// `not_found` for an item not in flight. A report for an item whose request is
    /// already final is recorded (and a final one finishes the item) without
    /// changing the request.
    async fn report(&self, id: MessageId, progress: RunProgress) -> Result<()> {
        let mut state = self.begin(
            QueueMethod::Report,
            QueueCall::Report {
                id,
                progress: progress.clone(),
            },
        )?;
        let source = state
            .in_flight
            .get(&id)
            .map(|item| item.source)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("message {id} is not running")))?;
        let (status, result, failure, done) = match &progress {
            RunProgress::Started => (RequestStatus::Running, None, None, false),
            RunProgress::Completed { result } => {
                (RequestStatus::Completed, result.clone(), None, true)
            }
            RunProgress::Failed { error } => {
                let status = match error.code {
                    ErrorCode::Denied => RequestStatus::Denied,
                    ErrorCode::ApprovalExpired => RequestStatus::Expired,
                    _ => RequestStatus::Failed,
                };
                (status, None, Some(error.clone()), true)
            }
            RunProgress::Interrupted => (
                RequestStatus::Failed,
                None,
                Some(error(ErrorCode::Internal, "interrupted")),
                true,
            ),
        };
        state.progress.push((id, progress));
        if done {
            state.in_flight.remove(&id);
        }
        let update = match source {
            QueueSource::Request { request_id, .. }
                if state
                    .requests
                    .get(&request_id)
                    .is_some_and(|request| request.status.is_final()) =>
            {
                None
            }
            _ => state.follow(&source, status, result, failure)?,
        };
        self.notify(state, update);
        Ok(())
    }
}

/// Frees the agent's consumer slot when `next` returns or is dropped.
struct Consumer<'a> {
    queue: &'a FakeRequestQueue,
    agent_id: AgentId,
}

impl Drop for Consumer<'_> {
    fn drop(&mut self) {
        lock(&self.queue.state).consumers.remove(&self.agent_id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use formation_proto::common::{MemberId, ScheduleId};

    use super::super::block_on;
    use super::super::samples::work_request;
    use super::*;

    async fn wait_for_consumer(queue: &FakeRequestQueue, agent_id: AgentId) {
        while !queue.calls().contains(&QueueCall::Next(agent_id)) {
            tokio::task::yield_now().await;
        }
    }

    #[test]
    fn keeps_order_and_edits_and_withdraws_queued_messages() {
        let queue = FakeRequestQueue::new();
        let (agent, other) = (AgentId::generate(), AgentId::generate());
        block_on(async {
            let first = queue
                .enqueue(agent, QueueSource::Owner, "one".into())
                .await
                .unwrap();
            let schedule = QueueSource::Schedule {
                schedule_id: ScheduleId::generate(),
            };
            let second = queue.enqueue(agent, schedule, "two".into()).await.unwrap();
            let third = queue
                .enqueue(agent, QueueSource::Owner, "three".into())
                .await
                .unwrap();
            assert!(first.queued_at < second.queued_at && second.queued_at < third.queued_at);
            assert!(queue.items(other).await.unwrap().is_empty());

            let edited = queue
                .edit(agent, second.id, "two, edited".into())
                .await
                .unwrap();
            assert_eq!(edited.text, "two, edited");
            queue.withdraw(agent, first.id).await.unwrap();
            let texts: Vec<_> = queue
                .items(agent)
                .await
                .unwrap()
                .into_iter()
                .map(|item| item.text)
                .collect();
            assert_eq!(texts, ["two, edited", "three"]);

            let missing = queue.withdraw(agent, first.id).await.unwrap_err();
            assert_eq!(missing.code, ErrorCode::NotFound);
            let wrong_agent = queue.edit(other, second.id, "x".into()).await.unwrap_err();
            assert_eq!(wrong_agent.code, ErrorCode::NotFound);
        });
        // Three enqueues, one edit, one withdraw.
        assert_eq!(queue.published().len(), 5);
        assert_eq!(queue.published().last().unwrap().1.len(), 2);
    }

    #[test]
    fn restores_a_taken_item_at_its_original_position() {
        let queue = FakeRequestQueue::new();
        let agent = AgentId::generate();
        block_on(async {
            let mut ids = vec![];
            for text in ["a", "b", "c"] {
                ids.push(
                    queue
                        .enqueue(agent, QueueSource::Owner, text.into())
                        .await
                        .unwrap()
                        .id,
                );
            }
            let taken = queue.take(agent, ids[1]).await.unwrap();
            assert_eq!(queue.in_flight(), vec![taken.clone()]);
            assert_eq!(queue.items(agent).await.unwrap().len(), 2);

            queue.restore(agent, taken.clone()).await.unwrap();
            let order: Vec<_> = queue
                .items(agent)
                .await
                .unwrap()
                .iter()
                .map(|i| i.id)
                .collect();
            assert_eq!(order, ids);
            assert!(queue.in_flight().is_empty());

            let twice = queue.restore(agent, taken.clone()).await.unwrap_err();
            assert_eq!(twice.code, ErrorCode::Conflict);
            let elsewhere = queue.restore(AgentId::generate(), taken).await.unwrap_err();
            assert_eq!(elsewhere.code, ErrorCode::InvalidParams);
        });
    }

    #[test]
    fn next_waits_for_a_message_and_allows_one_consumer_per_agent() {
        let queue = Arc::new(FakeRequestQueue::new());
        let port: Arc<dyn RequestQueue> = queue.clone();
        let agent = AgentId::generate();
        block_on(async {
            let consumer = tokio::spawn({
                let port = port.clone();
                async move { port.next(agent).await }
            });
            wait_for_consumer(&queue, agent).await;
            let second = port.next(agent).await.unwrap_err();
            assert_eq!(second.code, ErrorCode::Conflict);

            let item = port
                .enqueue(agent, QueueSource::Owner, "go".into())
                .await
                .unwrap();
            assert_eq!(consumer.await.unwrap().unwrap(), item);
            assert!(port.items(agent).await.unwrap().is_empty());
            assert_eq!(queue.in_flight(), vec![item.clone()]);

            // The slot is free again once `next` returned.
            let again = port
                .enqueue(agent, QueueSource::Owner, "more".into())
                .await
                .unwrap();
            assert_eq!(port.next(agent).await.unwrap(), again);

            port.report(item.id, RunProgress::Completed { result: None })
                .await
                .unwrap();
            let unknown = port
                .report(item.id, RunProgress::Started)
                .await
                .unwrap_err();
            assert_eq!(unknown.code, ErrorCode::NotFound);
        });
    }

    #[test]
    fn work_requests_follow_their_items_and_reach_the_requester() {
        let queue = FakeRequestQueue::new();
        let (requester, owner, agent) = (
            MemberId::generate(),
            MemberId::generate(),
            AgentId::generate(),
        );
        let request = work_request(requester, owner, agent, "Add a health endpoint", "k1");
        let mut updates = queue.subscribe_requests();
        block_on(async {
            let item = queue.enqueue_request(request.clone()).await.unwrap();
            assert_eq!(
                item.source,
                QueueSource::Request {
                    request_id: request.id,
                    requester
                }
            );
            assert_eq!(updates.recv().await.unwrap().status, RequestStatus::Queued);

            let running = queue.next(agent).await.unwrap();
            queue
                .report(running.id, RunProgress::Started)
                .await
                .unwrap();
            assert_eq!(updates.recv().await.unwrap().status, RequestStatus::Running);

            let waiting = queue
                .set_request_status(request.id, RequestStatus::AwaitingApproval)
                .unwrap();
            assert_eq!(waiting.status, RequestStatus::AwaitingApproval);
            assert_eq!(
                updates.recv().await.unwrap().status,
                RequestStatus::AwaitingApproval
            );
            queue
                .set_request_status(request.id, RequestStatus::Running)
                .unwrap();
            updates.recv().await.unwrap();

            let result = Some("Added GET /health".to_owned());
            queue
                .report(
                    running.id,
                    RunProgress::Completed {
                        result: result.clone(),
                    },
                )
                .await
                .unwrap();
            let done = updates.recv().await.unwrap();
            assert_eq!(done.status, RequestStatus::Completed);
            assert_eq!(done.result, result);
            assert_eq!(queue.request(request.id).unwrap(), done);

            let final_state = queue
                .set_request_status(request.id, RequestStatus::Running)
                .unwrap_err();
            assert_eq!(final_state.code, ErrorCode::Conflict);
        });
        assert_eq!(queue.progress().len(), 2);
    }

    #[test]
    fn failures_map_to_denied_expired_failed_and_withdrawals_cancel() {
        let queue = FakeRequestQueue::new();
        let (owner, agent) = (MemberId::generate(), AgentId::generate());
        let outcomes = [
            (ErrorCode::Denied, RequestStatus::Denied),
            (ErrorCode::ApprovalExpired, RequestStatus::Expired),
            (ErrorCode::HarnessUnavailable, RequestStatus::Failed),
        ];
        block_on(async {
            for (index, (code, status)) in outcomes.into_iter().enumerate() {
                let request = work_request(
                    MemberId::generate(),
                    owner,
                    agent,
                    "p",
                    &format!("k{index}"),
                );
                queue.enqueue_request(request.clone()).await.unwrap();
                let item = queue.next(agent).await.unwrap();
                queue.report(item.id, RunProgress::Started).await.unwrap();
                let failure = ApiError::new(code, "stopped");
                queue
                    .report(
                        item.id,
                        RunProgress::Failed {
                            error: failure.clone(),
                        },
                    )
                    .await
                    .unwrap();
                let request = queue.request(request.id).unwrap();
                assert_eq!(request.status, status);
                assert_eq!(request.error, Some(failure));
            }

            let interrupted = work_request(MemberId::generate(), owner, agent, "p", "ki");
            queue.enqueue_request(interrupted.clone()).await.unwrap();
            let item = queue.next(agent).await.unwrap();
            queue
                .report(item.id, RunProgress::Interrupted)
                .await
                .unwrap();
            assert_eq!(
                queue.request(interrupted.id).unwrap().status,
                RequestStatus::Failed
            );

            let withdrawn = work_request(MemberId::generate(), owner, agent, "p", "kw");
            let item = queue.enqueue_request(withdrawn.clone()).await.unwrap();
            queue.withdraw(agent, item.id).await.unwrap();
            assert_eq!(
                queue.request(withdrawn.id).unwrap().status,
                RequestStatus::Canceled
            );
            let requeue = queue
                .enqueue_request(queue.request(withdrawn.id).unwrap())
                .await;
            assert_eq!(requeue.unwrap_err().code, ErrorCode::Conflict);
        });
    }

    #[test]
    fn a_final_report_finishes_its_item_after_the_request_was_finalized_elsewhere() {
        let queue = FakeRequestQueue::new();
        let agent = AgentId::generate();
        let request = work_request(MemberId::generate(), MemberId::generate(), agent, "p", "kc");
        let mut updates = queue.subscribe_requests();
        block_on(async {
            queue.enqueue_request(request.clone()).await.unwrap();
            let item = queue.next(agent).await.unwrap();
            queue.report(item.id, RunProgress::Started).await.unwrap();
            queue
                .set_request_status(request.id, RequestStatus::Canceled)
                .unwrap();
            queue
                .report(
                    item.id,
                    RunProgress::Completed {
                        result: Some("done".into()),
                    },
                )
                .await
                .unwrap();
            assert!(queue.in_flight().is_empty());
            assert_eq!(queue.progress().len(), 2);
            let canceled = queue.request(request.id).unwrap();
            assert_eq!(canceled.status, RequestStatus::Canceled);
            assert_eq!(canceled.result, None);
            let statuses: Vec<_> = std::iter::from_fn(|| updates.try_recv().ok())
                .map(|update| update.status)
                .collect();
            assert_eq!(
                statuses,
                [
                    RequestStatus::Queued,
                    RequestStatus::Running,
                    RequestStatus::Canceled
                ]
            );
            let finished = queue
                .report(item.id, RunProgress::Started)
                .await
                .unwrap_err();
            assert_eq!(finished.code, ErrorCode::NotFound);
        });
    }

    #[test]
    fn scripted_failures_apply_once() {
        let queue = FakeRequestQueue::new();
        let agent = AgentId::generate();
        queue.fail_next(
            QueueMethod::Restore,
            ApiError::new(ErrorCode::Internal, "disk full"),
        );
        block_on(async {
            let item = queue
                .enqueue(agent, QueueSource::Owner, "steer me".into())
                .await
                .unwrap();
            let taken = queue.take(agent, item.id).await.unwrap();
            let failed = queue.restore(agent, taken.clone()).await.unwrap_err();
            assert_eq!(failed.code, ErrorCode::Internal);
            queue.restore(agent, taken).await.unwrap();
            assert_eq!(queue.items(agent).await.unwrap(), vec![item]);
        });
    }
}
