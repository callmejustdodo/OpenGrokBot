use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;

use formation_proto::agent::{Agent, AgentState, Avatar, HarnessKind, HarnessStatus};
use formation_proto::common::{
    AgentId, ApiError, ChainId, ErrorCode, MemberId, MessageId, RequestId, RunId, ScheduleId,
    SessionId,
};
use formation_proto::events::Event;
use formation_proto::peer::PeerRequestCreate;
use formation_proto::policy::{Caller, Chain};
use formation_proto::request::{QueueItem, QueueSource, RequestCreate, RequestStatus, WorkRequest};

use super::testing::{LoopbackPeers, MemoryCollab, ScriptedPolicy};
use super::{RequestClient, Requests, endpoints};
use crate::ports::{
    AgentRepo, CommandCall, CommandHandler, RequestQueue, RequestRepo, RunInfo, RunProgress,
    TeamRepo,
};
use crate::testkit::{FakeHarness, MemoryStore, RecordingEventSink};

/// The owner's Mac with one agent that is shared and accepts requests.
struct Owner {
    me: MemberId,
    agent: AgentId,
    collab: Arc<MemoryCollab>,
    policy: Arc<ScriptedPolicy>,
    events: Arc<RecordingEventSink>,
    requests: Arc<Requests>,
}

fn owner() -> Owner {
    let me = MemberId::generate();
    let agent = AgentId::generate();
    let collab = Arc::new(MemoryCollab::new(me));
    collab.share_agent(agent, true);
    let policy = Arc::new(ScriptedPolicy::default());
    let events = Arc::new(RecordingEventSink::default());
    let requests = Arc::new(Requests::new(
        collab.clone(),
        policy.clone(),
        events.clone(),
    ));
    Owner {
        me,
        agent,
        collab,
        policy,
        events,
        requests,
    }
}

impl Owner {
    /// The same Mac after formationd restarts: same store, fresh memory.
    fn restarted(&self) -> Requests {
        Requests::new(
            self.collab.clone(),
            self.policy.clone(),
            self.events.clone(),
        )
    }

    fn request_events(&self) -> Vec<RequestStatus> {
        self.events
            .events()
            .into_iter()
            .filter_map(|(_, event)| match event {
                Event::RequestUpdate(update) => Some(update.request.status),
                _ => None,
            })
            .collect()
    }

    fn queue_events(&self) -> Vec<Vec<QueueItem>> {
        self.events
            .events()
            .into_iter()
            .filter_map(|(_, event)| match event {
                Event::AgentQueue(update) => Some(update.items),
                _ => None,
            })
            .collect()
    }

    async fn queue(&self) -> Vec<QueueItem> {
        self.requests.items(self.agent).await.unwrap()
    }

    /// A request from a new teammate, queued.
    async fn queued_request(&self, key: &str) -> (MemberId, WorkRequest) {
        let teammate = MemberId::generate();
        let request = self
            .requests
            .create(teammate, body(self.agent, key, "Add token expiry tests"))
            .await
            .unwrap();
        (teammate, request)
    }

    async fn status(&self, id: RequestId) -> RequestStatus {
        self.collab.request(id).unwrap().unwrap().status
    }
}

fn body(agent_id: AgentId, key: &str, prompt: &str) -> PeerRequestCreate {
    PeerRequestCreate {
        request_id: RequestId::generate(),
        agent_id,
        prompt: prompt.into(),
        idempotency_key: key.into(),
        via_agent: Some("web".into()),
        chain: Chain {
            id: ChainId::generate(),
            depth: 0,
        },
    }
}

fn code<T: std::fmt::Debug>(result: Result<T, ApiError>) -> ErrorCode {
    result.unwrap_err().code
}

fn request_id_of(item: &QueueItem) -> Option<RequestId> {
    match item.source {
        QueueSource::Request { request_id, .. } => Some(request_id),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Creating requests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_request_is_queued_with_the_exact_prompt() {
    let owner = owner();
    let teammate = MemberId::generate();
    let mut updates = owner.requests.subscribe();
    let sent = body(owner.agent, "k1", "Add token expiry tests");

    let request = endpoints::create(&owner.requests, teammate, sent.clone())
        .await
        .unwrap();

    assert_eq!(request.id, sent.request_id);
    assert_eq!(request.status, RequestStatus::Queued);
    assert_eq!((request.requester, request.owner), (teammate, owner.me));
    assert_eq!(request.requester_agent.as_deref(), Some("web"));
    assert_eq!(request.chain, sent.chain);
    let queue = owner.queue().await;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].text, "Add token expiry tests");
    assert_eq!(
        queue[0].source,
        QueueSource::Request {
            request_id: request.id,
            requester: teammate
        }
    );
    assert_eq!(
        owner.request_events(),
        [RequestStatus::Received, RequestStatus::Queued]
    );
    assert_eq!(owner.queue_events(), std::slice::from_ref(&queue));
    assert_eq!(
        updates.recv().await.unwrap().status,
        RequestStatus::Received
    );
    assert_eq!(updates.recv().await.unwrap(), request);
    assert_eq!(
        owner.policy.checked.lock().unwrap().as_slice(),
        [(
            Caller::Member {
                member_id: teammate,
                via_agent: Some("web".into()),
                chain: sent.chain
            },
            owner.agent
        )]
    );
}

#[tokio::test]
async fn a_repeated_idempotency_key_returns_the_original_request() {
    let owner = owner();
    let teammate = MemberId::generate();
    let first = body(owner.agent, "k1", "Add token expiry tests");
    let original = owner
        .requests
        .create(teammate, first.clone())
        .await
        .unwrap();

    // A retry with the same request ID, and one with a new ID, both return it.
    let same_id = owner
        .requests
        .create(teammate, first.clone())
        .await
        .unwrap();
    let new_id = owner
        .requests
        .create(teammate, body(owner.agent, "k1", "Add token expiry tests"))
        .await
        .unwrap();
    assert_eq!(same_id, original);
    assert_eq!(new_id, original);
    assert_eq!(owner.queue().await.len(), 1);
    assert_eq!(owner.collab.inserts.load(Ordering::SeqCst), 1);

    // The retry returns the request as it is now.
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    let retried = owner
        .requests
        .create(teammate, first.clone())
        .await
        .unwrap();
    assert_eq!(retried.status, RequestStatus::Running);

    // The same key for other work, or a used request ID with another key, is refused.
    let other_prompt = body(owner.agent, "k1", "Delete the tests");
    assert_eq!(
        code(owner.requests.create(teammate, other_prompt).await),
        ErrorCode::Conflict
    );
    let mut reused_id = body(owner.agent, "k2", "Add token expiry tests");
    reused_id.request_id = original.id;
    assert_eq!(
        code(owner.requests.create(teammate, reused_id).await),
        ErrorCode::Conflict
    );
    // Keys are per requester: another teammate's "k1" is a new request.
    let other = owner
        .requests
        .create(
            MemberId::generate(),
            body(owner.agent, "k1", "Add token expiry tests"),
        )
        .await
        .unwrap();
    assert_ne!(other.id, original.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_creates_with_one_key_make_one_request() {
    let owner = owner();
    let teammate = MemberId::generate();
    let shared_id = body(owner.agent, "retry-key", "Add token expiry tests");
    let mut tasks = Vec::new();
    for attempt in 0..64 {
        let requests = owner.requests.clone();
        // Half retry with the same request ID, half with a new one.
        let sent = if attempt % 2 == 0 {
            shared_id.clone()
        } else {
            body(owner.agent, "retry-key", "Add token expiry tests")
        };
        tasks.push(tokio::spawn(async move {
            requests.create(teammate, sent).await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap().unwrap().id);
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "every create returned the same request");
    assert_eq!(owner.collab.inserts.load(Ordering::SeqCst), 1);
    assert_eq!(owner.collab.requests().unwrap().len(), 1);
    let queue = owner.queue().await;
    assert_eq!(queue.len(), 1);
    assert_eq!(request_id_of(&queue[0]), Some(ids[0]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_creates_with_different_keys_queue_each_once_in_order() {
    let owner = owner();
    let mut tasks = Vec::new();
    for attempt in 0..32 {
        let requests = owner.requests.clone();
        let sent = body(
            owner.agent,
            &format!("k{attempt}"),
            "Add token expiry tests",
        );
        tasks.push(tokio::spawn(async move {
            requests.create(MemberId::generate(), sent).await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap().unwrap().id);
    }
    let queue = owner.queue().await;
    assert_eq!(queue.len(), ids.len());
    assert!(
        queue
            .windows(2)
            .all(|pair| pair[0].queued_at < pair[1].queued_at)
    );
    let mut queued: Vec<RequestId> = queue.iter().filter_map(request_id_of).collect();
    queued.sort();
    ids.sort();
    assert_eq!(queued, ids, "each request is queued exactly once");
    // The store keeps the same order for a restart.
    let stored: Vec<MessageId> = owner
        .collab
        .queue_items(owner.agent)
        .unwrap()
        .iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(stored, queue.iter().map(|item| item.id).collect::<Vec<_>>());
}

#[tokio::test]
async fn requests_need_a_shared_agent_that_accepts_them() {
    let owner = owner();
    let teammate = MemberId::generate();
    let private = AgentId::generate();
    assert_eq!(
        code(
            owner
                .requests
                .create(teammate, body(private, "k1", "Work"))
                .await
        ),
        ErrorCode::AgentNotShared
    );

    owner.collab.share_agent(owner.agent, false);
    assert_eq!(
        code(
            owner
                .requests
                .create(teammate, body(owner.agent, "k2", "Work"))
                .await
        ),
        ErrorCode::RequestsNotAccepted
    );
    assert!(owner.collab.requests().unwrap().is_empty());
    assert!(owner.policy.checked.lock().unwrap().is_empty());

    // Turning requests back on lets the same key through.
    owner.collab.share_agent(owner.agent, true);
    let request = owner
        .requests
        .create(teammate, body(owner.agent, "k2", "Work"))
        .await
        .unwrap();
    assert_eq!(request.status, RequestStatus::Queued);

    // A retry still returns the original after requests are turned off again.
    owner.collab.share_agent(owner.agent, false);
    let mut retry = body(owner.agent, "k2", "Work");
    retry.request_id = request.id;
    assert_eq!(
        owner.requests.create(teammate, retry).await.unwrap().id,
        request.id
    );
}

#[tokio::test]
async fn chains_stop_at_depth_three_and_the_policy_has_the_last_word() {
    let owner = owner();
    let teammate = MemberId::generate();
    let mut deep = body(owner.agent, "k1", "Work");
    deep.chain.depth = 4;
    assert_eq!(
        code(owner.requests.create(teammate, deep).await),
        ErrorCode::RateLimited
    );
    let mut limit = body(owner.agent, "k2", "Work");
    limit.chain.depth = 3;
    assert!(owner.requests.create(teammate, limit).await.is_ok());

    *owner.policy.refusal.lock().unwrap() =
        Some(ApiError::new(ErrorCode::RateLimited, "30 per minute"));
    assert_eq!(
        code(
            owner
                .requests
                .create(teammate, body(owner.agent, "k3", "Work"))
                .await
        ),
        ErrorCode::RateLimited
    );
    assert_eq!(owner.collab.requests().unwrap().len(), 1);
}

#[tokio::test]
async fn empty_prompts_and_keys_are_invalid() {
    let owner = owner();
    let teammate = MemberId::generate();
    for (key, prompt) in [("k1", "  "), ("", "Work"), (" ", "Work")] {
        assert_eq!(
            code(
                owner
                    .requests
                    .create(teammate, body(owner.agent, key, prompt))
                    .await
            ),
            ErrorCode::InvalidParams
        );
    }
    assert!(owner.collab.requests().unwrap().is_empty());
}

#[tokio::test]
async fn only_the_requester_reads_a_request() {
    let owner = owner();
    let (teammate, request) = owner.queued_request("k1").await;
    assert_eq!(
        endpoints::get(&owner.requests, teammate, request.id).unwrap(),
        request
    );
    assert_eq!(
        code(endpoints::get(
            &owner.requests,
            MemberId::generate(),
            request.id
        )),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(endpoints::get(
            &owner.requests,
            teammate,
            RequestId::generate()
        )),
        ErrorCode::NotFound
    );
}

#[test]
fn path_ids_parse_or_are_invalid() {
    let id = RequestId::generate();
    assert_eq!(endpoints::parse_request_id(&id.to_string()).unwrap(), id);
    assert_eq!(
        code(endpoints::parse_request_id("req_8f21")),
        ErrorCode::InvalidParams
    );
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_queued_request_can_be_canceled_once_and_retried() {
    let owner = owner();
    let (teammate, request) = owner.queued_request("k1").await;
    assert_eq!(
        code(endpoints::cancel(
            &owner.requests,
            MemberId::generate(),
            request.id
        )),
        ErrorCode::NotFound
    );

    let canceled = endpoints::cancel(&owner.requests, teammate, request.id).unwrap();
    assert_eq!(canceled.status, RequestStatus::Canceled);
    assert!(owner.queue().await.is_empty());
    assert_eq!(
        owner.queue_events().last().unwrap(),
        &Vec::<QueueItem>::new()
    );
    assert_eq!(
        owner.request_events().last(),
        Some(&RequestStatus::Canceled)
    );

    // A retried cancel returns the canceled request.
    assert_eq!(
        endpoints::cancel(&owner.requests, teammate, request.id).unwrap(),
        canceled
    );
}

#[tokio::test]
async fn a_cancel_that_cannot_be_stored_leaves_the_request_cancelable() {
    let owner = owner();
    let (teammate, request) = owner.queued_request("k1").await;
    owner
        .collab
        .fail_request_write
        .store(true, Ordering::SeqCst);
    assert_eq!(
        code(endpoints::cancel(&owner.requests, teammate, request.id)),
        ErrorCode::Internal
    );
    // Still queued, with its item, so the retry succeeds.
    assert_eq!(owner.status(request.id).await, RequestStatus::Queued);
    assert_eq!(owner.queue().await.len(), 1);
    let canceled = endpoints::cancel(&owner.requests, teammate, request.id).unwrap();
    assert_eq!(canceled.status, RequestStatus::Canceled);
    assert!(owner.queue().await.is_empty());

    // The same for the owner withdrawing a request.
    let (_, second) = owner.queued_request("k2").await;
    let item = owner.queue().await[0].clone();
    owner
        .collab
        .fail_request_write
        .store(true, Ordering::SeqCst);
    assert_eq!(
        code(owner.requests.withdraw(owner.agent, item.id).await),
        ErrorCode::Internal
    );
    assert_eq!(owner.status(second.id).await, RequestStatus::Queued);
    assert_eq!(owner.queue().await, std::slice::from_ref(&item));
    owner.requests.withdraw(owner.agent, item.id).await.unwrap();
    assert_eq!(owner.status(second.id).await, RequestStatus::Canceled);
}

#[tokio::test]
async fn a_request_cannot_be_canceled_once_the_executor_has_it() {
    let owner = owner();
    let (teammate, request) = owner.queued_request("k1").await;
    let item = owner.requests.next(owner.agent).await.unwrap();

    // Taken but not reported yet: still queued, and no longer cancelable.
    assert_eq!(owner.status(request.id).await, RequestStatus::Queued);
    assert_eq!(
        code(endpoints::cancel(&owner.requests, teammate, request.id)),
        ErrorCode::Conflict
    );

    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    assert_eq!(
        code(endpoints::cancel(&owner.requests, teammate, request.id)),
        ErrorCode::Conflict
    );
    owner
        .requests
        .set_status(request.id, RequestStatus::AwaitingApproval, None)
        .unwrap();
    assert_eq!(
        code(endpoints::cancel(&owner.requests, teammate, request.id)),
        ErrorCode::Conflict
    );
    owner
        .requests
        .set_status(request.id, RequestStatus::Running, None)
        .unwrap();
    owner
        .requests
        .report(
            item.id,
            RunProgress::Completed {
                result: Some("Done".into()),
            },
        )
        .await
        .unwrap();
    let error = endpoints::cancel(&owner.requests, teammate, request.id).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.message, "the request is already completed");
}

#[tokio::test]
async fn the_owner_can_cancel_and_withdraw_before_the_start() {
    let owner = owner();
    let (_, first) = owner.queued_request("k1").await;
    let canceled = owner.requests.cancel(None, first.id).unwrap();
    assert_eq!(canceled.status, RequestStatus::Canceled);

    let (_, second) = owner.queued_request("k2").await;
    let item = owner.queue().await[0].clone();
    owner.requests.withdraw(owner.agent, item.id).await.unwrap();
    let withdrawn = owner.collab.request(second.id).unwrap().unwrap();
    assert_eq!(withdrawn.status, RequestStatus::Canceled);
    assert_eq!(withdrawn.error.unwrap().code, ErrorCode::Denied);
    assert!(owner.queue().await.is_empty());

    // Only this Mac's own requests can be canceled by its owner.
    let (_, third) = owner.queued_request("k3").await;
    owner.collab.set_me(MemberId::generate()).unwrap();
    assert_eq!(
        code(owner.requests.cancel(None, third.id)),
        ErrorCode::NotFound
    );
}

// ---------------------------------------------------------------------------
// Queue and executor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_queue_keeps_arrival_order_across_sources() {
    let owner = owner();
    let schedule = QueueSource::Schedule {
        schedule_id: ScheduleId::generate(),
    };
    let a = owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "first".into())
        .await
        .unwrap();
    let (_, request) = owner.queued_request("k1").await;
    let c = owner
        .requests
        .enqueue(owner.agent, schedule, "third".into())
        .await
        .unwrap();
    let d = owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "fourth".into())
        .await
        .unwrap();

    let queue = owner.queue().await;
    assert!(
        queue
            .windows(2)
            .all(|pair| pair[0].queued_at < pair[1].queued_at)
    );
    let mut taken = Vec::new();
    for _ in 0..4 {
        taken.push(owner.requests.next(owner.agent).await.unwrap());
    }
    assert_eq!(taken[0].id, a.id);
    assert_eq!(request_id_of(&taken[1]), Some(request.id));
    assert_eq!(taken[2].id, c.id);
    assert_eq!(taken[3].id, d.id);
    assert!(owner.queue().await.is_empty());

    // Queues are per agent.
    let other = AgentId::generate();
    owner
        .requests
        .enqueue(other, QueueSource::Owner, "elsewhere".into())
        .await
        .unwrap();
    assert!(owner.queue().await.is_empty());
    assert_eq!(owner.requests.items(other).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_request_that_ended_while_queued_is_never_handed_out() {
    let owner = owner();
    let (_, ended) = owner.queued_request("k1").await;
    let (_, waiting) = owner.queued_request("k2").await;
    // Something outside this module ended it without removing its item.
    let failed = WorkRequest {
        status: RequestStatus::Failed,
        ..owner.collab.request(ended.id).unwrap().unwrap()
    };
    owner.collab.put_request(&failed).unwrap();

    let item = owner.requests.next(owner.agent).await.unwrap();
    assert_eq!(request_id_of(&item), Some(waiting.id));
    assert!(owner.queue().await.is_empty());
    assert_eq!(owner.status(ended.id).await, RequestStatus::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_waits_for_an_item_and_serves_one_consumer() {
    let owner = owner();
    let requests = owner.requests.clone();
    let agent = owner.agent;
    let waiting = tokio::spawn(async move { requests.next(agent).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished());
    assert_eq!(
        code(owner.requests.next(owner.agent).await),
        ErrorCode::Conflict
    );

    let item = owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "hello".into())
        .await
        .unwrap();
    let taken = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(taken, item);

    // The consumer is free again, also after a waiting `next` is dropped.
    let abandoned =
        tokio::time::timeout(Duration::from_millis(20), owner.requests.next(owner.agent)).await;
    assert!(abandoned.is_err());
    owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "again".into())
        .await
        .unwrap();
    assert_eq!(
        owner.requests.next(owner.agent).await.unwrap().text,
        "again"
    );
}

#[tokio::test]
async fn the_executor_reports_move_the_request_to_its_end() {
    let owner = owner();
    for (key, progress, status) in [
        (
            "done",
            RunProgress::Completed {
                result: Some("Added 3 tests".into()),
            },
            RequestStatus::Completed,
        ),
        (
            "broke",
            RunProgress::Failed {
                error: ApiError::new(ErrorCode::HarnessUnavailable, "signed out"),
            },
            RequestStatus::Failed,
        ),
        ("stopped", RunProgress::Interrupted, RequestStatus::Failed),
    ] {
        let (_, request) = owner.queued_request(key).await;
        let item = owner.requests.next(owner.agent).await.unwrap();
        owner
            .requests
            .report(item.id, RunProgress::Started)
            .await
            .unwrap();
        // A repeated `Started` changes nothing.
        owner
            .requests
            .report(item.id, RunProgress::Started)
            .await
            .unwrap();
        assert_eq!(owner.status(request.id).await, RequestStatus::Running);
        owner.requests.report(item.id, progress).await.unwrap();
        let ended = owner.collab.request(request.id).unwrap().unwrap();
        assert_eq!(ended.status, status);
        match status {
            RequestStatus::Completed => assert_eq!(ended.result.as_deref(), Some("Added 3 tests")),
            _ => assert!(ended.error.is_some()),
        }
        // The item is forgotten once it ends.
        assert_eq!(
            code(owner.requests.report(item.id, RunProgress::Started).await),
            ErrorCode::NotFound
        );
    }
    assert_eq!(
        code(
            owner
                .requests
                .report(MessageId::generate(), RunProgress::Started)
                .await
        ),
        ErrorCode::NotFound
    );

    // A completion reported without `Started`, or while an approval was being
    // recorded, still completes the request and frees the item.
    let (_, unstarted) = owner.queued_request("unstarted").await;
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Completed { result: None })
        .await
        .unwrap();
    assert_eq!(owner.status(unstarted.id).await, RequestStatus::Completed);
    let (_, approving) = owner.queued_request("approving").await;
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    owner
        .requests
        .set_status(approving.id, RequestStatus::AwaitingApproval, None)
        .unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Completed { result: None })
        .await
        .unwrap();
    assert_eq!(owner.status(approving.id).await, RequestStatus::Completed);
    assert_eq!(
        code(owner.requests.report(item.id, RunProgress::Started).await),
        ErrorCode::NotFound
    );

    // Owner messages have no request behind them.
    owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "mine".into())
        .await
        .unwrap();
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Completed { result: None })
        .await
        .unwrap();
}

#[tokio::test]
async fn approvals_move_a_running_request_and_end_it() {
    let owner = owner();
    let (_, request) = owner.queued_request("k1").await;
    // Not before it runs: only the executor starts a request.
    for status in [RequestStatus::AwaitingApproval, RequestStatus::Running] {
        assert_eq!(
            code(owner.requests.set_status(request.id, status, None)),
            ErrorCode::Conflict
        );
    }
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    for status in [
        RequestStatus::AwaitingApproval,
        RequestStatus::Running,
        RequestStatus::AwaitingApproval,
    ] {
        assert_eq!(
            owner
                .requests
                .set_status(request.id, status, None)
                .unwrap()
                .status,
            status
        );
    }
    let denied = owner
        .requests
        .set_status(
            request.id,
            RequestStatus::Denied,
            Some(ApiError::new(ErrorCode::Denied, "npm test was denied")),
        )
        .unwrap();
    assert_eq!(denied.error.unwrap().code, ErrorCode::Denied);

    // The run ending afterwards keeps the request denied.
    owner
        .requests
        .report(item.id, RunProgress::Completed { result: None })
        .await
        .unwrap();
    assert_eq!(owner.status(request.id).await, RequestStatus::Denied);

    // Only approval moves go through set_status, and final statuses never change.
    for status in [
        RequestStatus::Received,
        RequestStatus::Queued,
        RequestStatus::Completed,
        RequestStatus::Failed,
        RequestStatus::Canceled,
    ] {
        assert_eq!(
            code(owner.requests.set_status(request.id, status, None)),
            ErrorCode::InvalidParams
        );
    }
    assert_eq!(
        code(
            owner
                .requests
                .set_status(request.id, RequestStatus::Expired, None)
        ),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(
            owner
                .requests
                .set_status(RequestId::generate(), RequestStatus::Expired, None)
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        owner.request_events(),
        [
            RequestStatus::Received,
            RequestStatus::Queued,
            RequestStatus::Running,
            RequestStatus::AwaitingApproval,
            RequestStatus::Running,
            RequestStatus::AwaitingApproval,
            RequestStatus::Denied,
        ]
    );
}

#[tokio::test]
async fn owner_messages_can_be_edited_taken_and_restored_but_requests_cannot() {
    let owner = owner();
    let first = owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "first".into())
        .await
        .unwrap();
    let (_, request) = owner.queued_request("k1").await;
    let third = owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "third".into())
        .await
        .unwrap();
    let request_item = owner.queue().await[1].clone();
    assert_eq!(request_id_of(&request_item), Some(request.id));

    let edited = owner
        .requests
        .edit(owner.agent, first.id, "first, edited".into())
        .await
        .unwrap();
    assert_eq!(edited.text, "first, edited");
    assert_eq!(
        code(
            owner
                .requests
                .edit(owner.agent, request_item.id, "changed".into())
                .await
        ),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(owner.requests.take(owner.agent, request_item.id).await),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(
            owner
                .requests
                .edit(owner.agent, MessageId::generate(), "x".into())
                .await
        ),
        ErrorCode::NotFound
    );

    // Taking the first message to steer, then restoring it, puts it back in place.
    let taken = owner.requests.take(owner.agent, first.id).await.unwrap();
    assert_eq!(owner.queue().await.len(), 2);
    owner
        .requests
        .restore(owner.agent, taken.clone())
        .await
        .unwrap();
    let ids: Vec<MessageId> = owner.queue().await.iter().map(|item| item.id).collect();
    assert_eq!(ids, [first.id, request_item.id, third.id]);
    assert_eq!(
        code(owner.requests.restore(owner.agent, taken.clone()).await),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(owner.requests.restore(AgentId::generate(), taken).await),
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code(owner.requests.restore(owner.agent, request_item).await),
        ErrorCode::InvalidParams
    );

    // Requests enter only through create; empty messages are refused.
    let source = QueueSource::Request {
        request_id: request.id,
        requester: request.requester,
    };
    assert_eq!(
        code(
            owner
                .requests
                .enqueue(owner.agent, source, "sneaky".into())
                .await
        ),
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code(
            owner
                .requests
                .enqueue(owner.agent, QueueSource::Owner, " ".into())
                .await
        ),
        ErrorCode::InvalidParams
    );
}

// ---------------------------------------------------------------------------
// Restart
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_restart_finds_the_queue_and_never_runs_a_taken_request_twice() {
    let owner = owner();
    let (_, taken) = owner.queued_request("taken").await;
    let (_, running) = owner.queued_request("running").await;
    let (_, waiting) = owner.queued_request("waiting").await;
    let message = owner
        .requests
        .enqueue(owner.agent, QueueSource::Owner, "mine".into())
        .await
        .unwrap();
    owner.requests.next(owner.agent).await.unwrap();
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    // A request stored just before a crash, not queued yet.
    let teammate = MemberId::generate();
    let stored = WorkRequest {
        status: RequestStatus::Received,
        ..owner.collab.request(waiting.id).unwrap().unwrap()
    };
    let unqueued = WorkRequest {
        id: RequestId::generate(),
        requester: teammate,
        idempotency_key: "unqueued".into(),
        ..stored
    };
    owner.collab.insert_request(&unqueued).unwrap();

    let restarted = owner.restarted();
    let changed = restarted.recover().unwrap();
    let status_of = |id| owner.collab.request(id).unwrap().unwrap().status;
    assert_eq!(status_of(taken.id), RequestStatus::Failed);
    assert_eq!(status_of(running.id), RequestStatus::Failed);
    assert_eq!(status_of(waiting.id), RequestStatus::Queued);
    assert_eq!(status_of(unqueued.id), RequestStatus::Queued);
    assert_eq!(changed.len(), 3);

    let queue = restarted.items(owner.agent).await.unwrap();
    assert_eq!(
        queue.iter().map(request_id_of).collect::<Vec<_>>(),
        [Some(waiting.id), None, Some(unqueued.id)]
    );
    assert_eq!(queue[1].id, message.id);
    // Recovering again changes nothing.
    assert!(restarted.recover().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_queue_write_fails_the_request() {
    let owner = owner();
    owner.collab.fail_queue_write.store(true, Ordering::SeqCst);
    let teammate = MemberId::generate();
    let sent = body(owner.agent, "k1", "Work");
    assert_eq!(
        code(owner.requests.create(teammate, sent.clone()).await),
        ErrorCode::Internal
    );
    assert_eq!(owner.status(sent.request_id).await, RequestStatus::Failed);
    assert!(owner.queue().await.is_empty());
}

// ---------------------------------------------------------------------------
// The requester's side
// ---------------------------------------------------------------------------

/// Minsu's Mac sending to Dohyeon's (the `owner()`), in one process.
struct Requester {
    me: MemberId,
    collab: Arc<MemoryCollab>,
    peers: Arc<LoopbackPeers>,
    harness: Arc<FakeHarness>,
    agents: Arc<MemoryStore>,
    events: Arc<RecordingEventSink>,
    client: RequestClient,
}

fn requester(owner: &Owner) -> Requester {
    let me = MemberId::generate();
    let collab = Arc::new(MemoryCollab::new(me));
    let peers = Arc::new(LoopbackPeers::new(me, owner.requests.clone()));
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
    let agents = Arc::new(MemoryStore::default());
    let events = Arc::new(RecordingEventSink::default());
    let own_side = Arc::new(Requests::new(
        collab.clone(),
        Arc::new(ScriptedPolicy::default()),
        events.clone(),
    ));
    let client = RequestClient::new(
        collab.clone(),
        peers.clone(),
        harness.clone(),
        agents.clone(),
        events.clone(),
        own_side,
    );
    Requester {
        me,
        collab,
        peers,
        harness,
        agents,
        events,
        client,
    }
}

impl Requester {
    fn command(&self, owner: &Owner, key: &str, from_run: Option<RunId>) -> RequestCreate {
        RequestCreate {
            owner: owner.me,
            agent_id: owner.agent,
            prompt: "Add token expiry tests".into(),
            idempotency_key: key.into(),
            from_question: None,
            from_run,
        }
    }

    fn sent(&self) -> Vec<PeerRequestCreate> {
        self.peers.sent.lock().unwrap().clone()
    }

    /// An agent on this Mac running `source`, as the harness reports it.
    fn run(&self, source: QueueSource) -> RunId {
        let agent_id = match self.agents.agent_by_slug("web").unwrap() {
            Some(agent) => agent.id,
            None => self.add_agent(),
        };
        let run_id = RunId::generate();
        self.harness.set_run(RunInfo {
            run_id,
            agent_id,
            session_id: SessionId("fork".into()),
            item: QueueItem {
                id: MessageId::generate(),
                agent_id,
                source,
                text: "work".into(),
                queued_at: std::time::SystemTime::now().into(),
            },
        });
        run_id
    }

    fn add_agent(&self) -> AgentId {
        let agent_id = AgentId::generate();
        self.agents
            .insert_agent(&Agent {
                id: agent_id,
                slug: "web".into(),
                name: "Web".into(),
                role: "Frontend developer".into(),
                harness: HarnessKind::ClaudeCode,
                folder_path: PathBuf::from("/Agents/web"),
                repositories: Vec::new(),
                avatar: Avatar {
                    shape: "square".into(),
                    color: "green".into(),
                },
                state: AgentState::Working,
                files_revision: "1".into(),
                created_at: std::time::SystemTime::now().into(),
                last_activity_at: None,
                detected: None,
                running_requests: vec![],
            })
            .unwrap();
        agent_id
    }
}

#[tokio::test]
async fn a_sent_request_follows_the_owners_copy() {
    let owner = owner();
    let minsu = requester(&owner);

    let request = minsu
        .client
        .create(minsu.command(&owner, "k1", None))
        .await
        .unwrap();
    assert_eq!(request.status, RequestStatus::Queued);
    assert_eq!((request.requester, request.owner), (minsu.me, owner.me));
    assert_eq!(request.chain.depth, 0);
    assert_eq!(request.requester_agent, None);
    assert_eq!(minsu.collab.request(request.id).unwrap().unwrap(), request);
    assert_eq!(owner.collab.request(request.id).unwrap().unwrap(), request);

    // A repeated key returns the request and sends nothing.
    let again = minsu
        .client
        .create(minsu.command(&owner, "k1", None))
        .await
        .unwrap();
    assert_eq!(again.id, request.id);
    assert_eq!(minsu.sent().len(), 1);
    let mut other_work = minsu.command(&owner, "k1", None);
    other_work.prompt = "Something else".into();
    assert_eq!(
        code(minsu.client.create(other_work).await),
        ErrorCode::Conflict
    );

    // Status follows the owner: on read, and from pushed updates.
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    assert_eq!(
        minsu.client.get(request.id).await.unwrap().status,
        RequestStatus::Running
    );
    owner
        .requests
        .report(
            item.id,
            RunProgress::Completed {
                result: Some("Added".into()),
            },
        )
        .await
        .unwrap();
    let pushed = owner.collab.request(request.id).unwrap().unwrap();
    // From someone else, or for a request Minsu did not send: ignored.
    minsu
        .client
        .apply_update(MemberId::generate(), pushed.clone())
        .unwrap();
    assert_eq!(
        minsu.collab.request(request.id).unwrap().unwrap().status,
        RequestStatus::Running
    );
    minsu.client.apply_update(owner.me, pushed.clone()).unwrap();
    assert_eq!(minsu.collab.request(request.id).unwrap().unwrap(), pushed);
    // An older update does not overwrite a newer copy.
    let older = WorkRequest {
        status: RequestStatus::Running,
        updated_at: request.updated_at,
        ..pushed.clone()
    };
    minsu.client.apply_update(owner.me, older).unwrap();
    assert_eq!(minsu.collab.request(request.id).unwrap().unwrap(), pushed);
    let statuses: Vec<RequestStatus> = minsu
        .events
        .events()
        .into_iter()
        .filter_map(|(_, event)| match event {
            Event::RequestUpdate(update) => Some(update.request.status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        [
            RequestStatus::Received,
            RequestStatus::Queued,
            RequestStatus::Running,
            RequestStatus::Completed
        ]
    );
}

#[tokio::test]
async fn the_requester_cancels_on_the_owners_mac() {
    let owner = owner();
    let minsu = requester(&owner);
    let request = minsu
        .client
        .create(minsu.command(&owner, "k1", None))
        .await
        .unwrap();
    let canceled = minsu.client.cancel(request.id).await.unwrap();
    assert_eq!(canceled.status, RequestStatus::Canceled);
    assert_eq!(owner.status(request.id).await, RequestStatus::Canceled);
    assert!(owner.queue().await.is_empty());
    assert_eq!(
        code(minsu.client.cancel(RequestId::generate()).await),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn an_agents_request_continues_its_runs_chain() {
    let owner = owner();
    let minsu = requester(&owner);

    // Minsu's agent working for Minsu starts a chain one level down.
    let own_run = minsu.run(QueueSource::Owner);
    let first = minsu
        .client
        .create(minsu.command(&owner, "k1", Some(own_run)))
        .await
        .unwrap();
    assert_eq!(first.chain.depth, 1);
    assert_eq!(first.requester_agent.as_deref(), Some("web"));

    // A run serving a teammate's request at depth 2 continues that chain at 3.
    let parent = WorkRequest {
        id: RequestId::generate(),
        requester: MemberId::generate(),
        owner: minsu.me,
        idempotency_key: "parent".into(),
        chain: Chain {
            id: ChainId::generate(),
            depth: 2,
        },
        ..first.clone()
    };
    minsu.collab.insert_request(&parent).unwrap();
    let run = minsu.run(QueueSource::Request {
        request_id: parent.id,
        requester: parent.requester,
    });
    let second = minsu
        .client
        .create(minsu.command(&owner, "k2", Some(run)))
        .await
        .unwrap();
    assert_eq!(
        second.chain,
        Chain {
            id: parent.chain.id,
            depth: 3
        }
    );

    // One more level is refused before anything is sent.
    let deepest = WorkRequest {
        id: RequestId::generate(),
        idempotency_key: "deepest".into(),
        chain: Chain {
            id: parent.chain.id,
            depth: 3,
        },
        ..parent
    };
    minsu.collab.insert_request(&deepest).unwrap();
    let run = minsu.run(QueueSource::Request {
        request_id: deepest.id,
        requester: deepest.requester,
    });
    let sent_before = minsu.sent().len();
    assert_eq!(
        code(
            minsu
                .client
                .create(minsu.command(&owner, "k3", Some(run)))
                .await
        ),
        ErrorCode::RateLimited
    );
    assert_eq!(minsu.sent().len(), sent_before);
    assert_eq!(
        code(
            minsu
                .client
                .create(minsu.command(&owner, "k4", Some(RunId::generate())))
                .await
        ),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn offline_owners_and_refusals_are_kept_on_the_requesters_mac() {
    let owner = owner();
    let minsu = requester(&owner);

    minsu.peers.offline.store(true, Ordering::SeqCst);
    assert_eq!(
        code(minsu.client.create(minsu.command(&owner, "k1", None)).await),
        ErrorCode::PeerOffline
    );
    let waiting = minsu.collab.requests().unwrap()[0].clone();
    assert_eq!(waiting.status, RequestStatus::Received);
    // While offline, reading returns the local copy.
    assert_eq!(minsu.client.get(waiting.id).await.unwrap(), waiting);
    minsu.peers.offline.store(false, Ordering::SeqCst);
    // Not delivered yet: the owner does not know it, so the local copy stands.
    assert_eq!(minsu.client.get(waiting.id).await.unwrap(), waiting);

    // The outbox delivers it later. Minsu's clock runs an hour ahead of Dohyeon's,
    // and the owner's copy still wins.
    let ahead = WorkRequest {
        updated_at: waiting.updated_at + Duration::from_secs(3600),
        ..waiting.clone()
    };
    minsu.collab.put_request(&ahead).unwrap();
    let outbox = minsu.sent()[0].clone();
    let delivered = endpoints::create(&owner.requests, minsu.me, outbox)
        .await
        .unwrap();
    minsu
        .client
        .apply_update(owner.me, delivered.clone())
        .unwrap();
    assert_eq!(
        minsu.collab.request(waiting.id).unwrap().unwrap(),
        delivered
    );

    owner.collab.share_agent(owner.agent, false);
    assert_eq!(
        code(minsu.client.create(minsu.command(&owner, "k2", None)).await),
        ErrorCode::RequestsNotAccepted
    );
    let refused = minsu
        .collab
        .requests()
        .unwrap()
        .into_iter()
        .find(|request| request.idempotency_key == "k2")
        .unwrap();
    assert_eq!(refused.status, RequestStatus::Failed);
    assert_eq!(refused.error.unwrap().code, ErrorCode::RequestsNotAccepted);

    // Sending to yourself is refused.
    let mut to_self = minsu.command(&owner, "k3", None);
    to_self.owner = minsu.me;
    assert_eq!(
        code(minsu.client.create(to_self).await),
        ErrorCode::InvalidParams
    );
}

#[tokio::test]
async fn a_retry_after_the_owner_was_offline_delivers_the_request_once() {
    let owner = owner();
    let minsu = requester(&owner);
    minsu.peers.offline.store(true, Ordering::SeqCst);
    assert_eq!(
        code(minsu.client.create(minsu.command(&owner, "k1", None)).await),
        ErrorCode::PeerOffline
    );
    minsu.peers.offline.store(false, Ordering::SeqCst);

    let delivered = minsu
        .client
        .create(minsu.command(&owner, "k1", None))
        .await
        .unwrap();
    assert_eq!(delivered.status, RequestStatus::Queued);
    let again = minsu
        .client
        .create(minsu.command(&owner, "k1", None))
        .await
        .unwrap();
    assert_eq!(again, delivered);
    let sent = minsu.sent();
    assert_eq!(sent.len(), 2, "sent again only while unacknowledged");
    assert_eq!(sent[0], sent[1]);
    assert_eq!(owner.collab.requests().unwrap().len(), 1);
    assert_eq!(owner.queue().await.len(), 1);
}

#[tokio::test]
async fn a_failed_send_does_not_overwrite_a_copy_a_concurrent_retry_stored() {
    let owner = owner();
    let minsu = requester(&owner);
    *minsu.peers.lose_reply.lock().unwrap() = Some((
        minsu.collab.clone(),
        ApiError::new(ErrorCode::Internal, "connection reset"),
    ));
    assert_eq!(
        code(minsu.client.create(minsu.command(&owner, "k1", None)).await),
        ErrorCode::Internal
    );
    let local = minsu.collab.requests().unwrap()[0].clone();
    assert_eq!(local.status, RequestStatus::Queued);
    // Owner updates still apply, because the copy was not marked failed.
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    let running = owner.collab.request(local.id).unwrap().unwrap();
    minsu
        .client
        .apply_update(owner.me, running.clone())
        .unwrap();
    assert_eq!(minsu.collab.request(local.id).unwrap().unwrap(), running);
}

#[tokio::test]
async fn request_commands_decode_params_and_reach_the_client() {
    let owner = owner();
    let minsu = requester(&owner);
    let call = |method: &str, params: serde_json::Value| CommandCall {
        caller: Caller::Owner,
        method: method.into(),
        params,
    };

    assert_eq!(
        minsu.client.methods(),
        ["request_create", "request_get", "request_cancel"]
    );
    let created = minsu
        .client
        .handle(call(
            "request_create",
            serde_json::to_value(minsu.command(&owner, "k1", None)).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(created["status"], "queued");
    let id = created["id"].clone();
    let read = minsu
        .client
        .handle(call("request_get", json!({ "request_id": id })))
        .await
        .unwrap();
    assert_eq!(read, created);
    let canceled = minsu
        .client
        .handle(call("request_cancel", json!({ "request_id": id })))
        .await
        .unwrap();
    assert_eq!(canceled["status"], "canceled");

    assert_eq!(
        code(minsu.client.handle(call("request_get", json!({}))).await),
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code(minsu.client.handle(call("ask", json!({}))).await),
        ErrorCode::InvalidParams
    );
    let mut from_teammate = call("request_get", json!({ "request_id": id }));
    from_teammate.caller = Caller::Member {
        member_id: owner.me,
        via_agent: None,
        chain: Chain {
            id: ChainId::generate(),
            depth: 0,
        },
    };
    assert_eq!(
        code(minsu.client.handle(from_teammate).await),
        ErrorCode::Denied
    );
}

#[tokio::test]
async fn requests_to_my_own_agents_are_read_and_canceled_locally() {
    let owner = owner();
    let (teammate, request) = owner.queued_request("k1").await;
    // Dohyeon's own client, on Dohyeon's store and owner side.
    let parts = requester(&owner);
    let client = RequestClient::new(
        owner.collab.clone(),
        parts.peers.clone(),
        parts.harness.clone(),
        parts.agents.clone(),
        owner.events.clone(),
        owner.requests.clone(),
    );
    assert_eq!(client.get(request.id).await.unwrap(), request);
    assert_eq!(
        client.cancel(request.id).await.unwrap().status,
        RequestStatus::Canceled
    );
    assert!(parts.sent().is_empty());
    assert_eq!(request.requester, teammate);
}

#[tokio::test]
async fn the_requester_sees_the_result_and_error_masked_and_the_owner_does_not() {
    // Built at run time so secret scanners do not flag the test string.
    let secret = format!("{}{}", "ghp_", "AbCdEfGhIjKlMnOpQrStUvWxYz012345");
    let owner = owner();
    let mut updates = owner.requests.subscribe();
    let (teammate, request) = owner.queued_request("mask").await;
    let item = owner.requests.next(owner.agent).await.unwrap();
    owner
        .requests
        .report(item.id, RunProgress::Started)
        .await
        .unwrap();
    owner
        .requests
        .report(
            item.id,
            RunProgress::Completed {
                result: Some(format!("Pushed with token {secret}.")),
            },
        )
        .await
        .unwrap();

    // The owner's copy keeps the original.
    let stored = owner.collab.request(request.id).unwrap().unwrap();
    assert!(stored.result.as_deref().unwrap().contains(&secret));

    // Every path to the requester is masked.
    let read = endpoints::get(&owner.requests, teammate, request.id).unwrap();
    let read_result = read.result.unwrap();
    assert!(!read_result.contains(&secret), "{read_result}");
    assert!(read_result.starts_with("Pushed with token "));
    let mut last = None;
    while let Ok(update) = updates.try_recv() {
        last = Some(update);
    }
    let pushed = last.expect("an update for the requester").result.unwrap();
    assert!(!pushed.contains(&secret), "{pushed}");

    let failed = super::outgoing::for_requester(&WorkRequest {
        result: None,
        error: Some(ApiError::new(
            ErrorCode::Internal,
            format!("git push used {secret}"),
        )),
        ..stored
    });
    assert!(!failed.error.unwrap().message.contains(&secret));
}
