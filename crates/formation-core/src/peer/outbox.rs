//! Owner: B. The asker's outbox (FR-016, US-018).
//!
//! A question or work request for a teammate whose Mac cannot be reached waits
//! here and is sent when the Mac is back: on a retry timer that backs off from
//! [`RetryPolicy::initial`] to [`RetryPolicy::max`], and at once when the
//! teammate's events connection reopens ([`Outbox::nudge`]). Entries for one
//! teammate go out in the order they were queued; an unreachable teammate never
//! holds up another.
//!
//! Every entry is keyed by the ID the asker's Mac chose (`question_id`,
//! `request_id`), and a replay sends the same body again, so the owner's Mac
//! returns the original instead of acting twice. An answer from the owner's Mac,
//! including a refusal such as `agent_not_shared`, is final: the entry leaves the
//! outbox and its [`OutboxOutcome`] says what happened.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, watch};

use formation_proto::common::{ApiError, ErrorCode, MemberId, QuestionId, RequestId, Timestamp};
use formation_proto::peer::{PeerAsk, PeerAskAccepted, PeerRequestCreate};
use formation_proto::request::WorkRequest;

use crate::ports::Result;

/// A call waiting for its teammate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxPayload {
    /// `POST /v1/ask`
    Ask { ask: PeerAsk },
    /// `POST /v1/requests`
    Request { request: PeerRequestCreate },
}

impl OutboxPayload {
    pub fn key(&self) -> OutboxKey {
        match self {
            Self::Ask { ask } => OutboxKey::Question(ask.question_id),
            Self::Request { request } => OutboxKey::Request(request.request_id),
        }
    }
}

/// The asker-chosen ID an entry is known by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum OutboxKey {
    Question(QuestionId),
    Request(RequestId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// The teammate whose Mac the call is for.
    pub member: MemberId,
    pub payload: OutboxPayload,
    pub queued_at: Timestamp,
    /// Failed attempts so far.
    pub attempts: u32,
    /// Why the last attempt failed.
    pub last_error: Option<ApiError>,
}

impl OutboxEntry {
    pub fn key(&self) -> OutboxKey {
        self.payload.key()
    }
}

/// Where entries survive a restart, so formationd retries them when it starts
/// again (backend architecture, "Recovery"). formationd uses
/// [`SqliteCollabStore`](crate::collab_store::SqliteCollabStore) (migration
/// 2002); [`MemoryOutboxStore`] keeps them in memory, for tests.
pub trait OutboxStore: Send + Sync {
    /// Every waiting entry, oldest first.
    fn load(&self) -> Result<Vec<OutboxEntry>>;
    /// Inserts the entry, or replaces the one with the same key in place.
    fn put(&self, entry: &OutboxEntry) -> Result<()>;
    /// Removes the entry; a key that is not there is not an error.
    fn remove(&self, key: OutboxKey) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct MemoryOutboxStore {
    entries: Mutex<Vec<OutboxEntry>>,
}

impl OutboxStore for MemoryOutboxStore {
    fn load(&self) -> Result<Vec<OutboxEntry>> {
        Ok(lock(&self.entries).clone())
    }

    fn put(&self, entry: &OutboxEntry) -> Result<()> {
        let mut entries = lock(&self.entries);
        match entries.iter_mut().find(|e| e.key() == entry.key()) {
            Some(slot) => *slot = entry.clone(),
            None => entries.push(entry.clone()),
        }
        Ok(())
    }

    fn remove(&self, key: OutboxKey) -> Result<()> {
        lock(&self.entries).retain(|e| e.key() != key);
        Ok(())
    }
}

/// How long to wait between attempts. Also used for the events connection's
/// reconnects ([`super::client::PeerClientConfig::reconnect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The wait after the first failure; it doubles after each further one.
    pub initial: Duration,
    /// The longest wait, jitter included.
    pub max: Duration,
    /// Give up on an entry this long after it was queued (`peer_offline`). `None`
    /// keeps it until it is sent or canceled; the documents set no limit.
    pub max_age: Option<Duration>,
}

impl Default for RetryPolicy {
    /// 2 s doubling to 60 s, no age limit (assumed; config key to come).
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(2),
            max: Duration::from_secs(60),
            max_age: None,
        }
    }
}

impl RetryPolicy {
    /// The wait before attempt `failures + 1`: doubling from `initial`, spread by
    /// up to 20% either way so teammates do not retry in step, and never above
    /// `max` (the cap is applied after the spread).
    pub fn delay(&self, failures: u32) -> Duration {
        self.delay_with(failures, random_unit())
    }

    /// [`RetryPolicy::delay`] with `unit` in `[0, 1)` in place of the random draw.
    pub fn delay_with(&self, failures: u32, unit: f64) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        let base = self.initial.saturating_mul(1 << doublings).min(self.max);
        let factor = 0.8 + 0.4 * unit.clamp(0.0, 1.0);
        base.mul_f64(factor).min(self.max)
    }
}

/// What the owner's Mac answered to a delivered call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    Ask(PeerAskAccepted),
    Request(Box<WorkRequest>),
}

/// The result of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Delivered(Delivered),
    /// The owner's Mac answered with an error. Final; never retried.
    Rejected(ApiError),
    /// The owner's Mac was not reached (`peer_offline`). Retried later.
    Unreachable(ApiError),
}

/// Sends one entry to its teammate. [`super::client::PeerClient`] does it over
/// HTTP; tests use a script.
#[async_trait]
pub trait Deliver: Send + Sync {
    async fn deliver(&self, member: MemberId, payload: &OutboxPayload) -> Delivery;
}

/// An entry left the outbox: delivered, refused by the owner's Mac, or given up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxOutcome {
    pub entry: OutboxEntry,
    pub result: std::result::Result<Delivered, ApiError>,
}

/// What [`Outbox::cancel`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Canceled {
    /// Removed before it was sent; the owner's Mac never saw it.
    Removed(OutboxEntry),
    /// Being sent right now; cancel it on the owner's Mac instead.
    InFlight,
    NotQueued,
}

/// The outbox. Cheap to clone; clones share one queue.
#[derive(Clone)]
pub struct Outbox {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn OutboxStore>,
    deliver: Arc<dyn Deliver>,
    retry: RetryPolicy,
    queues: Mutex<HashMap<MemberId, MemberQueue>>,
    outcomes: broadcast::Sender<OutboxOutcome>,
    shutdown: watch::Sender<bool>,
}

#[derive(Default)]
struct MemberQueue {
    entries: VecDeque<OutboxEntry>,
    /// A sender task is running for this member.
    running: bool,
    in_flight: Option<OutboxKey>,
    nudge: Arc<Notify>,
}

impl Outbox {
    pub fn new(store: Arc<dyn OutboxStore>, deliver: Arc<dyn Deliver>, retry: RetryPolicy) -> Self {
        let (outcomes, _) = broadcast::channel(256);
        let (shutdown, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                store,
                deliver,
                retry,
                queues: Mutex::new(HashMap::new()),
                outcomes,
                shutdown,
            }),
        }
    }

    /// Loads what the store kept and starts sending it. Call once, inside the
    /// tokio runtime, after subscribing to outcomes.
    pub fn start(&self) -> Result<()> {
        for entry in self.inner.store.load()? {
            self.enqueue(entry);
        }
        Ok(())
    }

    /// Queues a call for `member` and starts sending it. Returns `false` when an
    /// entry with the same key is already waiting (the call is a repeat); that
    /// entry is kept and retried at once.
    pub fn push(&self, member: MemberId, payload: OutboxPayload) -> Result<bool> {
        let key = payload.key();
        if let Some(member) = self.member_of(key) {
            self.nudge(member);
            return Ok(false);
        }
        let entry = OutboxEntry {
            member,
            payload,
            queued_at: now(),
            attempts: 0,
            last_error: None,
        };
        self.inner.store.put(&entry)?;
        self.enqueue(entry);
        Ok(true)
    }

    /// Retries `member`'s entries now, for example because their Mac is back.
    pub fn nudge(&self, member: MemberId) {
        if let Some(queue) = lock(&self.inner.queues).get(&member) {
            queue.nudge.notify_one();
        }
    }

    pub fn cancel(&self, key: OutboxKey) -> Result<Canceled> {
        let removed = {
            let mut queues = lock(&self.inner.queues);
            let Some(queue) = queues.values_mut().find(|q| q.contains(key)) else {
                return Ok(Canceled::NotQueued);
            };
            if queue.in_flight == Some(key) {
                return Ok(Canceled::InFlight);
            }
            let index = queue.position(key).expect("contains");
            queue.entries.remove(index).expect("index")
        };
        self.inner.store.remove(key)?;
        Ok(Canceled::Removed(removed))
    }

    pub fn contains(&self, key: OutboxKey) -> bool {
        self.member_of(key).is_some()
    }

    /// Whether anything waits for `member`. New calls for that member queue behind
    /// it, so a teammate's calls arrive in order.
    pub fn has_pending(&self, member: MemberId) -> bool {
        lock(&self.inner.queues)
            .get(&member)
            .is_some_and(|q| !q.entries.is_empty())
    }

    /// Waiting entries, oldest first per member; all members when `None`.
    pub fn pending(&self, member: Option<MemberId>) -> Vec<OutboxEntry> {
        let queues = lock(&self.inner.queues);
        let mut entries: Vec<OutboxEntry> = queues
            .iter()
            .filter(|(m, _)| member.is_none_or(|wanted| **m == wanted))
            .flat_map(|(_, q)| q.entries.iter().cloned())
            .collect();
        entries.sort_by_key(|e| e.queued_at);
        entries
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutboxOutcome> {
        self.inner.outcomes.subscribe()
    }

    /// Stops sending. Entries stay in the store for the next start.
    pub fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
    }

    fn member_of(&self, key: OutboxKey) -> Option<MemberId> {
        lock(&self.inner.queues)
            .iter()
            .find(|(_, q)| q.contains(key))
            .map(|(member, _)| *member)
    }

    fn enqueue(&self, entry: OutboxEntry) {
        let member = entry.member;
        let mut queues = lock(&self.inner.queues);
        let queue = queues.entry(member).or_default();
        if queue.contains(entry.key()) {
            return;
        }
        queue.entries.push_back(entry);
        if !queue.running {
            queue.running = true;
            tokio::spawn(send_loop(self.inner.clone(), member));
        }
    }
}

impl MemberQueue {
    fn position(&self, key: OutboxKey) -> Option<usize> {
        self.entries.iter().position(|e| e.key() == key)
    }

    fn contains(&self, key: OutboxKey) -> bool {
        self.position(key).is_some()
    }
}

/// Sends `member`'s entries one at a time, oldest first, until none is left.
async fn send_loop(inner: Arc<Inner>, member: MemberId) {
    let mut shutdown = inner.shutdown.subscribe();
    let mut failures = 0u32;
    loop {
        let next = {
            let mut queues = lock(&inner.queues);
            let queue = queues.entry(member).or_default();
            match queue.entries.front().cloned() {
                Some(entry) if !*shutdown.borrow() => {
                    queue.in_flight = Some(entry.key());
                    Some((entry, queue.nudge.clone()))
                }
                _ => {
                    queue.running = false;
                    queue.in_flight = None;
                    if queue.entries.is_empty() {
                        queues.remove(&member);
                    }
                    None
                }
            }
        };
        let Some((entry, nudge)) = next else {
            return;
        };

        if let Some(max_age) = inner.retry.max_age
            && age(&entry) > max_age
        {
            let error = ApiError::new(
                ErrorCode::PeerOffline,
                format!(
                    "gave up after {} s: the teammate's Mac did not come back",
                    max_age.as_secs()
                ),
            );
            inner.finish(entry, Err(error));
            continue;
        }

        match inner.deliver.deliver(member, &entry.payload).await {
            Delivery::Delivered(delivered) => {
                failures = 0;
                inner.finish(entry, Ok(delivered));
            }
            Delivery::Rejected(error) => {
                failures = 0;
                inner.finish(entry, Err(error));
            }
            Delivery::Unreachable(error) => {
                failures = failures.saturating_add(1);
                inner.record_failure(&entry, error);
                let delay = inner.retry.delay(failures);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = nudge.notified() => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
    }
}

impl Inner {
    /// Removes a delivered, refused, or expired entry and reports it.
    fn finish(&self, entry: OutboxEntry, result: std::result::Result<Delivered, ApiError>) {
        let key = entry.key();
        {
            let mut queues = lock(&self.queues);
            if let Some(queue) = queues.get_mut(&entry.member) {
                if let Some(index) = queue.position(key) {
                    queue.entries.remove(index);
                }
                queue.in_flight = None;
            }
        }
        // A failed removal only means a replay after restart, which the owner's
        // Mac answers with the original.
        let _ = self.store.remove(key);
        let _ = self.outcomes.send(OutboxOutcome { entry, result });
    }

    fn record_failure(&self, entry: &OutboxEntry, error: ApiError) {
        let key = entry.key();
        let updated = {
            let mut queues = lock(&self.queues);
            let Some(queue) = queues.get_mut(&entry.member) else {
                return;
            };
            queue.in_flight = None;
            let Some(index) = queue.position(key) else {
                return;
            };
            let slot = &mut queue.entries[index];
            slot.attempts = slot.attempts.saturating_add(1);
            slot.last_error = Some(error);
            slot.clone()
        };
        let _ = self.store.put(&updated);
    }
}

pub(crate) fn now() -> Timestamp {
    Timestamp::from(SystemTime::now())
}

fn age(entry: &OutboxEntry) -> Duration {
    (now() - entry.queued_at).to_std().unwrap_or_default()
}

/// A number in `[0, 1)` for jitter. `RandomState` is seeded per instance, which
/// is plenty for spreading retries and needs no extra crate.
fn random_unit() -> f64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// Resolves once `stop` turns true (or its sender is gone). Drops the watch
/// guard before returning, so it can sit in a `select!` of a spawned task.
pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use formation_proto::ask::AskTarget;
    use formation_proto::common::{AgentId, ChainId};
    use formation_proto::policy::Chain;

    use super::*;

    fn ask(owner: MemberId) -> OutboxPayload {
        OutboxPayload::Ask {
            ask: PeerAsk {
                question_id: QuestionId::generate(),
                thread_id: None,
                target: AskTarget::Agent {
                    member_id: owner,
                    agent_id: AgentId::generate(),
                },
                text: "Is the login API done?".into(),
                via_agent: None,
                chain: Chain {
                    id: ChainId::generate(),
                    depth: 0,
                },
            },
        }
    }

    fn accepted(payload: &OutboxPayload) -> Delivery {
        match payload {
            OutboxPayload::Ask { ask } => Delivery::Delivered(Delivered::Ask(PeerAskAccepted {
                question_id: ask.question_id,
            })),
            OutboxPayload::Request { .. } => unreachable!("tests send asks"),
        }
    }

    /// Unreachable while `down`; refuses keys in `refuse`; records every attempt.
    #[derive(Default)]
    struct Script {
        down: AtomicBool,
        refuse: Mutex<Vec<OutboxKey>>,
        attempts: Mutex<Vec<OutboxKey>>,
    }

    #[async_trait]
    impl Deliver for Script {
        async fn deliver(&self, _member: MemberId, payload: &OutboxPayload) -> Delivery {
            lock(&self.attempts).push(payload.key());
            if self.down.load(Ordering::SeqCst) {
                return Delivery::Unreachable(ApiError::new(ErrorCode::PeerOffline, "down"));
            }
            if lock(&self.refuse).contains(&payload.key()) {
                return Delivery::Rejected(ApiError::new(ErrorCode::AgentNotShared, "unshared"));
            }
            accepted(payload)
        }
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(40),
            max_age: None,
        }
    }

    async fn next(rx: &mut broadcast::Receiver<OutboxOutcome>) -> OutboxOutcome {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("outcome in time")
            .expect("outcome")
    }

    #[test]
    fn delay_doubles_and_never_passes_the_cap() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay_with(1, 0.5), Duration::from_secs(2));
        assert_eq!(policy.delay_with(2, 0.5), Duration::from_secs(4));
        assert_eq!(policy.delay_with(3, 0.5), Duration::from_secs(8));
        assert_eq!(policy.delay_with(1, 0.0), Duration::from_millis(1600));
        // Jitter on top of the cap would reach 72 s; the cap comes last.
        assert_eq!(policy.delay_with(10, 0.999), Duration::from_secs(60));
        assert_eq!(policy.delay_with(u32::MAX, 0.0), Duration::from_secs(48));
        for failures in 0..40 {
            assert!(policy.delay(failures) <= policy.max);
        }
    }

    #[tokio::test]
    async fn an_unreachable_teammate_gets_the_entry_when_back_in_order_and_once() {
        let script = Arc::new(Script::default());
        script.down.store(true, Ordering::SeqCst);
        let store = Arc::new(MemoryOutboxStore::default());
        let outbox = Outbox::new(store.clone(), script.clone(), fast());
        let mut outcomes = outbox.subscribe();
        let owner = MemberId::generate();

        let (first, second) = (ask(owner), ask(owner));
        assert!(outbox.push(owner, first.clone()).unwrap());
        assert!(outbox.push(owner, second.clone()).unwrap());
        assert!(
            !outbox.push(owner, first.clone()).unwrap(),
            "same key is a repeat"
        );
        assert_eq!(store.load().unwrap().len(), 2);

        tokio::time::sleep(Duration::from_millis(80)).await;
        let tried = lock(&script.attempts).clone();
        assert!(tried.len() >= 2, "retried while down: {tried:?}");
        assert!(
            tried.iter().all(|k| *k == first.key()),
            "the first holds the line"
        );
        assert!(outbox.pending(Some(owner))[0].attempts >= 1);

        script.down.store(false, Ordering::SeqCst);
        outbox.nudge(owner);
        assert_eq!(next(&mut outcomes).await.entry.key(), first.key());
        assert_eq!(next(&mut outcomes).await.entry.key(), second.key());
        assert!(!outbox.has_pending(owner));
        assert!(store.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refusal_is_final_and_reported() {
        let script = Arc::new(Script::default());
        let outbox = Outbox::new(
            Arc::new(MemoryOutboxStore::default()),
            script.clone(),
            fast(),
        );
        let mut outcomes = outbox.subscribe();
        let owner = MemberId::generate();
        let payload = ask(owner);
        lock(&script.refuse).push(payload.key());

        outbox.push(owner, payload.clone()).unwrap();
        let outcome = next(&mut outcomes).await;
        assert_eq!(outcome.result.unwrap_err().code, ErrorCode::AgentNotShared);
        assert_eq!(lock(&script.attempts).len(), 1);
        assert!(!outbox.contains(payload.key()));
    }

    #[tokio::test]
    async fn a_canceled_entry_is_never_sent() {
        let script = Arc::new(Script::default());
        script.down.store(true, Ordering::SeqCst);
        let outbox = Outbox::new(
            Arc::new(MemoryOutboxStore::default()),
            script.clone(),
            fast(),
        );
        let owner = MemberId::generate();
        let (first, second) = (ask(owner), ask(owner));
        outbox.push(owner, first.clone()).unwrap();
        outbox.push(owner, second.clone()).unwrap();

        match outbox.cancel(second.key()).unwrap() {
            Canceled::Removed(entry) => assert_eq!(entry.payload, second),
            other => panic!("expected removal, got {other:?}"),
        }
        assert_eq!(outbox.cancel(second.key()).unwrap(), Canceled::NotQueued);

        let mut outcomes = outbox.subscribe();
        script.down.store(false, Ordering::SeqCst);
        outbox.nudge(owner);
        assert_eq!(next(&mut outcomes).await.entry.key(), first.key());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!lock(&script.attempts).contains(&second.key()));
    }

    #[tokio::test]
    async fn entries_kept_by_the_store_are_sent_after_a_restart() {
        let store = Arc::new(MemoryOutboxStore::default());
        let owner = MemberId::generate();
        let payload = ask(owner);
        store
            .put(&OutboxEntry {
                member: owner,
                payload: payload.clone(),
                queued_at: now(),
                attempts: 3,
                last_error: None,
            })
            .unwrap();

        let outbox = Outbox::new(store.clone(), Arc::new(Script::default()), fast());
        let mut outcomes = outbox.subscribe();
        outbox.start().unwrap();
        let outcome = next(&mut outcomes).await;
        assert_eq!(outcome.entry.payload, payload);
        assert!(outcome.result.is_ok());
        assert!(store.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_entry_past_its_age_limit_gives_up_as_peer_offline() {
        let script = Arc::new(Script::default());
        script.down.store(true, Ordering::SeqCst);
        let policy = RetryPolicy {
            max_age: Some(Duration::from_millis(30)),
            ..fast()
        };
        let outbox = Outbox::new(Arc::new(MemoryOutboxStore::default()), script, policy);
        let mut outcomes = outbox.subscribe();
        let owner = MemberId::generate();
        outbox.push(owner, ask(owner)).unwrap();
        let outcome = next(&mut outcomes).await;
        assert_eq!(outcome.result.unwrap_err().code, ErrorCode::PeerOffline);
        assert!(!outbox.has_pending(owner));
    }

    #[test]
    fn entries_serialize_for_the_store() {
        let owner = MemberId::generate();
        let entry = OutboxEntry {
            member: owner,
            payload: ask(owner),
            queued_at: now(),
            attempts: 1,
            last_error: Some(ApiError::new(ErrorCode::PeerOffline, "refused")),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["payload"]["kind"], "ask");
        let back: OutboxEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back, entry);
        let key = serde_json::to_value(entry.key()).unwrap();
        assert_eq!(key["kind"], "question");
    }
}
