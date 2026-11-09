//! The owner's side of `POST /v1/ask` and `POST /v1/questions/{id}/action`
//! (tech spec, "Peer protocol" and "Ask pipeline"): [`OwnerAsks`] implements the
//! peer server's [`AskEndpoints`] on top of [`AskPipeline`].
//!
//! A question is accepted at once and answered in the background: a fork takes
//! several seconds (TASK-006 measured 4-8 s), longer than the asker's request
//! timeout, so `POST /v1/ask` answers `202 Accepted` and every state the
//! question then reaches is published to the asker on the [`PeerEventHub`]. The
//! same goes for a refresh or "ask another agent", which fork again.
//!
//! The question lives on this Mac (D-029); the asker's Mac keeps a copy and
//! applies the newest by `updated_at`. Every write goes through one gate
//! ([`Gate`]) that publishes it, and that refuses to overwrite a question the
//! asker canceled meanwhile, so a background answer that finishes after a cancel
//! is dropped instead of reviving the question. A fork already running is not
//! stopped; its transcript is recorded as the thread's fork and deleted with the
//! thread like any other.
//!
//! Masking (NFR-003): the pipeline masks answers, excerpts, and error messages
//! before they are stored; [`OwnerAsks::new`] gives it the owner's email
//! addresses so those are masked too.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;

use formation_proto::agent::Agent;
use formation_proto::ask::{Question, QuestionStatus, RouteChoice};
use formation_proto::common::{ApiError, ErrorCode, MemberId, QuestionId, SessionId};
use formation_proto::peer::{PeerAsk, PeerAskAccepted, PeerEvent, PeerQuestionAction};
use formation_proto::policy::{Caller, ShareSettings};

use super::mask::Masker;
use super::pipeline::{AskPipeline, Clock};
use crate::peer::api::{AskEndpoints, PeerEventHub};
use crate::peer::outbox::lock;
use crate::policy::sharing::shared_agents;
use crate::ports::{CollabStore, QuestionRepo, Result, Store};

/// How long a final question (answered, canceled, failed) is still sent again
/// when the asker's events connection opens. A Mac offline longer than this
/// missed the update, but its copy keeps the state it last saw and the owner
/// still has the question.
pub const RESEND_FINAL_FOR: chrono::Duration = chrono::Duration::days(7);

/// Answers teammates' questions to this Mac's agents. Cheap to clone: every
/// field is shared, so a background answer holds a clone.
#[derive(Clone)]
pub struct OwnerAsks {
    store: Arc<dyn Store>,
    collab: Arc<dyn CollabStore>,
    hub: PeerEventHub,
    pipeline: Arc<AskPipeline>,
    masker: Masker,
    clock: Clock,
    /// Questions with a background answer running. A second refresh or choice
    /// for one of them is refused instead of forking twice.
    busy: Arc<Mutex<HashSet<QuestionId>>>,
    /// Serializes the check and the write of [`Gate::put_question`] with a
    /// cancel, so neither overwrites the other.
    writes: Arc<Mutex<()>>,
}

impl OwnerAsks {
    /// `pipeline` is configured by the caller (harness, scorer, events, clock);
    /// its masker is replaced by one that also masks `owner_emails`: the owner's
    /// Claude Code account and any other address of theirs a transcript may
    /// carry (NFR-003).
    pub fn new<I, S>(
        store: Arc<dyn Store>,
        collab: Arc<dyn CollabStore>,
        hub: PeerEventHub,
        pipeline: AskPipeline,
        owner_emails: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let masker = Masker::with_owner_emails(owner_emails);
        Self {
            store,
            collab,
            hub,
            pipeline: Arc::new(pipeline.with_masker(masker.clone())),
            masker,
            clock: Arc::new(|| SystemTime::now().into()),
            busy: Arc::default(),
            writes: Arc::default(),
        }
    }

    /// The clock for the times this type sets (`created_at`, `updated_at` of a
    /// cancel, the resend window). The pipeline keeps its own clock.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    fn gate(&self) -> Gate<'_> {
        Gate {
            collab: self.collab.as_ref(),
            hub: &self.hub,
            writes: &self.writes,
        }
    }

    /// The asker's question, or `not_found` for anyone else, so a member cannot
    /// learn which question IDs exist.
    fn own_question(&self, member: MemberId, id: QuestionId) -> Result<Question> {
        self.collab
            .question(id)?
            .filter(|question| question.asker == member)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such question"))
    }

    /// Marks the question busy, or `conflict` when an answer is already running.
    fn claim(&self, id: QuestionId) -> Result<Busy> {
        if !lock(&self.busy).insert(id) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "the question is still being answered",
            ));
        }
        Ok(Busy {
            set: self.busy.clone(),
            id,
        })
    }

    /// The agents the pipeline may route to and answer from, read on every run so
    /// an agent unshared meanwhile is no longer in it (US-009).
    fn pool(&self) -> Result<Vec<(Agent, ShareSettings)>> {
        Ok(shared_agents(&self.store.agents()?, &self.collab.shares()?))
    }

    /// Stores and publishes the question as accepted for more work, then runs
    /// `job` in the background. Returns the stored question.
    fn start(&self, busy: Busy, mut question: Question, job: Job) -> Result<Question> {
        question.status = QuestionStatus::Received;
        question.error = None;
        question.updated_at = (self.clock)();
        self.gate().put_question(&question)?;
        let accepted = question.clone();
        let this = self.clone();
        tokio::spawn(async move { this.run(busy, question, job).await });
        Ok(accepted)
    }

    async fn run(self, busy: Busy, mut question: Question, job: Job) {
        let gate = self.gate();
        let result = match self.pool() {
            Ok(pool) => match job {
                Job::Handle => self.pipeline.handle(&gate, &pool, &mut question).await,
                Job::Refresh => self.pipeline.refresh(&gate, &pool, &mut question).await,
                Job::Choose(choice) => {
                    self.pipeline
                        .choose(&gate, &pool, &mut question, choice)
                        .await
                }
            },
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            // The pipeline stores its own failures; this is a store error or a
            // cancel. Best effort, so the asker does not wait on a question stuck
            // in `answering`; the gate refuses it after a cancel.
            question.status = QuestionStatus::Failed;
            question.error = Some(ApiError {
                message: self.masker.mask(&error.message),
                ..error
            });
            question.updated_at = (self.clock)();
            let _ = gate.put_question(&question);
        }
        drop(busy);
    }

    fn cancel(&self, member: MemberId, id: QuestionId) -> Result<Question> {
        let _writes = lock(&self.writes);
        let mut question = self.own_question(member, id)?;
        match question.status {
            QuestionStatus::Canceled => return Ok(question),
            QuestionStatus::Answered | QuestionStatus::Failed => {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "the question is already finished",
                ));
            }
            _ => {}
        }
        question.status = QuestionStatus::Canceled;
        question.error = None;
        question.updated_at = (self.clock)();
        self.collab.put_question(&question)?;
        publish(&self.hub, &question);
        Ok(question)
    }
}

/// What a background run does with the question.
enum Job {
    Handle,
    Refresh,
    Choose(RouteChoice),
}

/// A question's busy mark, cleared when the run ends, however it ends.
struct Busy {
    set: Arc<Mutex<HashSet<QuestionId>>>,
    id: QuestionId,
}

impl Drop for Busy {
    fn drop(&mut self) {
        lock(&self.set).remove(&self.id);
    }
}

/// Whether the asker may refresh or choose an agent: the pipeline is done with
/// the question and it is waiting on the asker.
fn settled(status: QuestionStatus) -> bool {
    matches!(
        status,
        QuestionStatus::Answered | QuestionStatus::Failed | QuestionStatus::Unsure
    )
}

fn is_final(status: QuestionStatus) -> bool {
    matches!(
        status,
        QuestionStatus::Answered | QuestionStatus::Canceled | QuestionStatus::Failed
    )
}

fn not_settled(question: &Question) -> ApiError {
    ApiError::new(
        ErrorCode::Conflict,
        format!(
            "the question cannot be changed while it is {:?}",
            question.status
        ),
    )
}

fn publish(hub: &PeerEventHub, question: &Question) {
    hub.publish_to(
        question.asker,
        PeerEvent::QuestionUpdate {
            question: Box::new(question.clone()),
        },
    );
}

/// The [`QuestionRepo`] the pipeline writes through: publishes every stored
/// state to the asker, and refuses to overwrite a canceled question with
/// `conflict`, which also stops the pipeline before it forks when the cancel
/// came first.
struct Gate<'a> {
    collab: &'a dyn CollabStore,
    hub: &'a PeerEventHub,
    writes: &'a Mutex<()>,
}

impl QuestionRepo for Gate<'_> {
    fn question(&self, id: QuestionId) -> Result<Option<Question>> {
        self.collab.question(id)
    }

    fn questions(&self) -> Result<Vec<Question>> {
        self.collab.questions()
    }

    fn put_question(&self, question: &Question) -> Result<()> {
        let _writes = lock(self.writes);
        let canceled = self
            .collab
            .question(question.id)?
            .is_some_and(|stored| stored.status == QuestionStatus::Canceled);
        if canceled && question.status != QuestionStatus::Canceled {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "the asker canceled the question",
            ));
        }
        self.collab.put_question(question)?;
        publish(self.hub, question);
        Ok(())
    }

    fn delete_question(&self, id: QuestionId) -> Result<()> {
        self.collab.delete_question(id)
    }

    fn fork_session(&self, thread_id: QuestionId) -> Result<Option<SessionId>> {
        self.collab.fork_session(thread_id)
    }

    fn put_fork_session(&self, thread_id: QuestionId, session_id: &SessionId) -> Result<()> {
        self.collab.put_fork_session(thread_id, session_id)
    }
}

#[async_trait]
impl AskEndpoints for OwnerAsks {
    async fn ask(&self, caller: Caller, ask: PeerAsk) -> Result<PeerAskAccepted> {
        let Caller::Member { member_id, .. } = caller else {
            return Err(ApiError::new(
                ErrorCode::NotMember,
                "peer questions come from members",
            ));
        };
        let accepted = PeerAskAccepted {
            question_id: ask.question_id,
        };
        if ask.text.trim().is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidParams,
                "the question is empty",
            ));
        }
        let thread_id = ask.thread_id.unwrap_or(ask.question_id);
        if thread_id != ask.question_id {
            // A follow-up resumes the thread's fork, which holds the earlier
            // questions and answers: only the member who started the thread may
            // continue it.
            self.own_question(member_id, thread_id)
                .map_err(|_| ApiError::new(ErrorCode::NotFound, "no such thread"))?;
        }
        let at = (self.clock)();
        let question = Question {
            id: ask.question_id,
            thread_id,
            asker: member_id,
            target: ask.target,
            text: ask.text,
            status: QuestionStatus::Received,
            candidates: Vec::new(),
            agent_id: None,
            answer: None,
            error: None,
            created_at: at,
            updated_at: at,
        };
        let busy = {
            let _writes = lock(&self.writes);
            if let Some(stored) = self.collab.question(question.id)? {
                // An outbox replay of a question already accepted: its answer is
                // under way or published.
                return if stored.asker == member_id {
                    Ok(accepted)
                } else {
                    Err(ApiError::new(
                        ErrorCode::Conflict,
                        "the question ID is taken",
                    ))
                };
            }
            let busy = self.claim(question.id)?;
            self.collab.put_question(&question)?;
            busy
        };
        publish(&self.hub, &question);
        let this = self.clone();
        tokio::spawn(async move { this.run(busy, question, Job::Handle).await });
        Ok(accepted)
    }

    async fn question_action(
        &self,
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    ) -> Result<Question> {
        let choice = match action {
            PeerQuestionAction::Cancel => return self.cancel(member, question_id),
            PeerQuestionAction::Refresh => None,
            PeerQuestionAction::Route { choice } => Some(choice),
        };
        let question = self.own_question(member, question_id)?;
        if !settled(question.status) {
            return Err(not_settled(&question));
        }
        let busy = self.claim(question_id)?;
        match choice {
            None => self.start(busy, question, Job::Refresh),
            Some(choice @ RouteChoice::Agent { .. }) => {
                self.start(busy, question, Job::Choose(choice))
            }
            // Sending the question to the owner forks nothing: answer in place.
            Some(RouteChoice::Owner) => {
                let mut question = question;
                let pool = self.pool()?;
                self.pipeline
                    .choose(&self.gate(), &pool, &mut question, RouteChoice::Owner)
                    .await?;
                drop(busy);
                Ok(question)
            }
        }
    }

    async fn questions_to_resend(&self, member: MemberId) -> Result<Vec<Question>> {
        let now = (self.clock)();
        let mut questions: Vec<Question> = self
            .collab
            .questions()?
            .into_iter()
            .filter(|q| q.asker == member)
            .filter(|q| !is_final(q.status) || now - q.updated_at <= RESEND_FINAL_FOR)
            .collect();
        questions.sort_by_key(|q| (q.updated_at, q.id));
        Ok(questions)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    use tokio::sync::Semaphore;
    use tokio::sync::broadcast::Receiver;

    use formation_proto::agent::{HarnessKind, HarnessStatus};
    use formation_proto::ask::AskTarget;
    use formation_proto::common::{ChainId, Timestamp};
    use formation_proto::harness::{SessionInfo, TurnOutcome};
    use formation_proto::policy::{Chain, Visibility};

    use super::*;
    use crate::ask::mask::MASK;
    use crate::ask::routing::test_support::ScriptedScorer;
    use crate::ask::routing::{AgentScore, RoutingProfile, Scorer};
    use crate::ask::test_support::TempDir;
    use crate::peer::api::events::HubMessage;
    use crate::policy::sharing::test_support::agent;
    use crate::ports::{AgentRepo, ForkResult, ShareRepo, TurnResult};
    use crate::testkit::{FakeHarness, HarnessCall, MemoryCollabStore, MemoryStore, ScriptedAgent};

    const MAIN: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const OWNER_EMAIL: &str = "dohyeon@example.com";

    /// 10:00 UTC plus `minute` minutes.
    fn at(minute: i64) -> Timestamp {
        "2026-11-09T10:00:00Z".parse::<Timestamp>().unwrap() + chrono::Duration::minutes(minute)
    }

    fn session(dir: &Path, agent: &Agent, id: &str) -> SessionInfo {
        SessionInfo {
            agent_id: agent.id,
            session_id: SessionId(id.into()),
            transcript_path: dir.join(format!("{id}.jsonl")),
            cwd: agent.folder_path.clone(),
            started_at: at(0),
            last_activity_at: Some(at(31)),
        }
    }

    /// Waits for a permit before scoring, so a test can act while a question is
    /// being routed.
    struct GatedScorer {
        gate: Arc<Semaphore>,
        inner: ScriptedScorer,
    }

    #[async_trait]
    impl Scorer for GatedScorer {
        async fn score(
            &self,
            question: &str,
            agents: &[RoutingProfile],
        ) -> Result<Vec<AgentScore>> {
            let _permit = self.gate.acquire().await;
            self.inner.score(question, agents).await
        }
    }

    /// The owner's Mac: one shared agent with scripted fork answers, in-memory
    /// stores, and the hub a test listens on.
    struct Owner {
        _dir: TempDir,
        api: Agent,
        me: MemberId,
        harness: Arc<FakeHarness>,
        collab: Arc<MemoryCollabStore>,
        hub: PeerEventHub,
        asks: OwnerAsks,
    }

    impl Owner {
        fn new(answers: &[(&str, &str)], scorer: Arc<dyn Scorer>) -> Self {
            let dir = TempDir::new("ask-endpoints");
            let api = agent("api", "API developer");
            let mut scripted = ScriptedAgent::new(session(dir.path(), &api, MAIN), []);
            scripted.forks = answers
                .iter()
                .map(|(id, text)| ForkResult {
                    session: session(dir.path(), &api, id),
                    result: TurnResult {
                        outcome: TurnOutcome::Completed,
                        result: Some((*text).into()),
                    },
                })
                .collect::<VecDeque<_>>();
            let harness = Arc::new(FakeHarness::new(
                HarnessKind::ClaudeCode,
                HarnessStatus {
                    harness: HarnessKind::ClaudeCode,
                    installed: true,
                    signed_in: Some(true),
                    version: Some("2.1.319".into()),
                    path: None,
                    problem: None,
                },
                [scripted],
            ));
            let store = Arc::new(MemoryStore::default());
            store.insert_agent(&api).unwrap();
            let collab = Arc::new(MemoryCollabStore::new());
            collab
                .put_share(&ShareSettings {
                    shared: true,
                    visibility: Visibility::AnswerExcerpts,
                    ..ShareSettings::private(api.id)
                })
                .unwrap();
            let hub = PeerEventHub::new();
            let minute = Arc::new(AtomicI64::new(40));
            let clock: Clock = Arc::new(move || at(minute.fetch_add(1, Ordering::SeqCst)));
            let pipeline = AskPipeline::new(harness.clone(), scorer).with_clock(clock.clone());
            let asks = OwnerAsks::new(store, collab.clone(), hub.clone(), pipeline, [OWNER_EMAIL])
                .with_clock(clock);
            Self {
                _dir: dir,
                api,
                me: MemberId::generate(),
                harness,
                collab,
                hub,
                asks,
            }
        }

        fn scripted(answers: &[(&str, &str)], scores: Vec<AgentScore>) -> Self {
            Self::new(answers, Arc::new(ScriptedScorer::new(Ok(scores))))
        }

        fn to_agent(&self) -> AskTarget {
            AskTarget::Agent {
                member_id: self.me,
                agent_id: self.api.id,
            }
        }

        fn to_person(&self) -> AskTarget {
            AskTarget::Person { member_id: self.me }
        }

        fn forks(&self) -> usize {
            self.harness
                .calls()
                .iter()
                .filter(|call| matches!(call, HarnessCall::RunFork { .. }))
                .count()
        }

        fn stored(&self, id: QuestionId) -> Question {
            self.collab.question(id).unwrap().unwrap()
        }

        /// Until no background answer runs.
        async fn idle(&self) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !lock(&self.asks.busy).is_empty() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the background answer finished");
        }
    }

    fn member(member_id: MemberId) -> Caller {
        Caller::Member {
            member_id,
            via_agent: None,
            chain: Chain {
                id: ChainId::generate(),
                depth: 0,
            },
        }
    }

    fn peer_ask(target: AskTarget, text: &str) -> PeerAsk {
        PeerAsk {
            question_id: QuestionId::generate(),
            thread_id: None,
            target,
            text: text.into(),
            via_agent: None,
            chain: Chain {
                id: ChainId::generate(),
                depth: 0,
            },
        }
    }

    /// The next question update published to `member`.
    async fn next_update(rx: &mut Receiver<HubMessage>, member: MemberId) -> Question {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(HubMessage::Event {
                        audience: crate::peer::api::Audience::Member(to),
                        event: PeerEvent::QuestionUpdate { question },
                    }) if to == member => return *question,
                    Ok(_) => {}
                    Err(error) => panic!("hub closed: {error}"),
                }
            }
        })
        .await
        .expect("a question update was published")
    }

    /// Updates published to `member` until the question reaches a status in
    /// `until`.
    async fn statuses_until(
        rx: &mut Receiver<HubMessage>,
        member: MemberId,
        until: &[QuestionStatus],
    ) -> (Vec<QuestionStatus>, Question) {
        let mut seen = Vec::new();
        loop {
            let question = next_update(rx, member).await;
            seen.push(question.status);
            if until.contains(&question.status) {
                return (seen, question);
            }
        }
    }

    fn answer_text() -> String {
        format!("The login API cleanup is done. Signed in as {OWNER_EMAIL}.")
    }

    #[tokio::test]
    async fn a_question_is_accepted_at_once_and_its_masked_answer_published() {
        let answer = answer_text();
        let owner = Owner::scripted(&[("f1", &answer)], vec![]);
        let asker = MemberId::generate();
        let mut rx = owner.hub.subscribe();
        let ask = peer_ask(owner.to_agent(), "How far is the login work?");
        let accepted = owner.asks.ask(member(asker), ask.clone()).await.unwrap();
        assert_eq!(accepted.question_id, ask.question_id);

        let (statuses, question) =
            statuses_until(&mut rx, asker, &[QuestionStatus::Answered]).await;
        assert_eq!(
            statuses,
            [
                QuestionStatus::Received,
                QuestionStatus::Answering,
                QuestionStatus::Answered
            ]
        );
        assert_eq!(question.asker, asker);
        assert_eq!(question.thread_id, ask.question_id);
        let text = &question.answer.as_ref().unwrap().text;
        assert!(text.contains(MASK) && !text.contains(OWNER_EMAIL), "{text}");
        owner.idle().await;
        assert_eq!(owner.stored(ask.question_id), question);
        assert_eq!(owner.forks(), 1);
    }

    #[tokio::test]
    async fn an_outbox_replay_returns_the_question_without_asking_again() {
        let answer = answer_text();
        let owner = Owner::scripted(&[("f1", &answer), ("f2", &answer)], vec![]);
        let asker = MemberId::generate();
        let ask = peer_ask(owner.to_agent(), "How far?");
        owner.asks.ask(member(asker), ask.clone()).await.unwrap();
        // While the first answer runs, and after it finished.
        let again = owner.asks.ask(member(asker), ask.clone()).await.unwrap();
        assert_eq!(again.question_id, ask.question_id);
        owner.idle().await;
        let again = owner.asks.ask(member(asker), ask.clone()).await.unwrap();
        assert_eq!(again.question_id, ask.question_id);
        owner.idle().await;
        assert_eq!(owner.forks(), 1);
        assert_eq!(owner.collab.questions().unwrap().len(), 1);

        // Another member cannot take over the ID.
        let error = owner
            .asks
            .ask(member(MemberId::generate()), ask)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn only_the_asker_sees_or_acts_on_a_question() {
        let answer = answer_text();
        let owner = Owner::scripted(&[("f1", &answer)], vec![]);
        let asker = MemberId::generate();
        let other = MemberId::generate();
        let ask = peer_ask(owner.to_agent(), "How far?");
        owner.asks.ask(member(asker), ask.clone()).await.unwrap();
        owner.idle().await;

        for action in [
            PeerQuestionAction::Cancel,
            PeerQuestionAction::Refresh,
            PeerQuestionAction::Route {
                choice: RouteChoice::Owner,
            },
        ] {
            let error = owner
                .asks
                .question_action(other, ask.question_id, action)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound, "{action:?}");
        }
        assert!(
            owner
                .asks
                .questions_to_resend(other)
                .await
                .unwrap()
                .is_empty()
        );
        let resent = owner.asks.questions_to_resend(asker).await.unwrap();
        assert_eq!(resent.len(), 1);
        assert_eq!(resent[0].status, QuestionStatus::Answered);

        // Nor follow up in the asker's thread, whose fork holds the answers.
        let follow_up = PeerAsk {
            thread_id: Some(ask.question_id),
            ..peer_ask(owner.to_agent(), "And the tokens?")
        };
        let error = owner.asks.ask(member(other), follow_up).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(owner.forks(), 1);
    }

    #[tokio::test]
    async fn the_asker_follows_up_in_their_thread() {
        let answer = answer_text();
        let owner = Owner::scripted(
            &[("f1", &answer), ("f2", "The expiry check is next.")],
            vec![],
        );
        let asker = MemberId::generate();
        let mut rx = owner.hub.subscribe();
        let first = peer_ask(owner.to_agent(), "How far?");
        owner.asks.ask(member(asker), first.clone()).await.unwrap();
        statuses_until(&mut rx, asker, &[QuestionStatus::Answered]).await;
        owner.idle().await;

        let next = PeerAsk {
            thread_id: Some(first.question_id),
            ..peer_ask(owner.to_agent(), "And then?")
        };
        owner.asks.ask(member(asker), next.clone()).await.unwrap();
        let (_, question) = statuses_until(&mut rx, asker, &[QuestionStatus::Answered]).await;
        assert_eq!(question.id, next.question_id);
        assert_eq!(question.thread_id, first.question_id);
        assert_eq!(question.answer.unwrap().text, "The expiry check is next.");
        let calls = owner.harness.calls();
        let HarnessCall::RunFork { from, .. } = &calls[1] else {
            panic!("expected a fork: {calls:?}");
        };
        assert_eq!(from.0, "f1");
    }

    #[tokio::test]
    async fn the_asker_chooses_an_agent_refreshes_and_sends_to_the_owner() {
        // No scores: routing is unsure.
        let owner = Owner::scripted(
            &[("f1", "First answer."), ("f2", "Fresh answer.")],
            Vec::new(),
        );
        let asker = MemberId::generate();
        let mut rx = owner.hub.subscribe();
        let ask = peer_ask(owner.to_person(), "Who broke the build?");
        let id = ask.question_id;
        owner.asks.ask(member(asker), ask).await.unwrap();
        let (_, question) = statuses_until(
            &mut rx,
            asker,
            &[QuestionStatus::Unsure, QuestionStatus::Answered],
        )
        .await;
        assert_eq!(question.status, QuestionStatus::Unsure);
        owner.idle().await;

        let choice = RouteChoice::Agent {
            agent_id: owner.api.id,
        };
        let accepted = owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Route { choice })
            .await
            .unwrap();
        assert_eq!(accepted.status, QuestionStatus::Received);
        let (_, question) = statuses_until(&mut rx, asker, &[QuestionStatus::Answered]).await;
        assert_eq!(question.answer.unwrap().text, "First answer.");
        owner.idle().await;

        owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Refresh)
            .await
            .unwrap();
        let (_, question) = statuses_until(&mut rx, asker, &[QuestionStatus::Answered]).await;
        assert_eq!(question.answer.unwrap().text, "Fresh answer.");
        owner.idle().await;
        assert_eq!(owner.forks(), 2);

        let sent = owner
            .asks
            .question_action(
                asker,
                id,
                PeerQuestionAction::Route {
                    choice: RouteChoice::Owner,
                },
            )
            .await
            .unwrap();
        assert_eq!(sent.status, QuestionStatus::SentToOwner);
        assert_eq!(next_update(&mut rx, asker).await, sent);
        assert_eq!(owner.stored(id), sent);
        // Waiting on the owner: nothing more for the asker to change but cancel.
        let error = owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Refresh)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        let canceled = owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Cancel)
            .await
            .unwrap();
        assert_eq!(canceled.status, QuestionStatus::Canceled);
    }

    #[tokio::test]
    async fn a_cancel_during_routing_is_not_undone_by_the_answer() {
        let gate = Arc::new(Semaphore::new(0));
        let scorer = GatedScorer {
            gate: gate.clone(),
            inner: ScriptedScorer::new(Ok(Vec::new())),
        };
        let owner = Owner::new(&[("f1", "An answer.")], Arc::new(scorer));
        let asker = MemberId::generate();
        let mut rx = owner.hub.subscribe();
        let ask = peer_ask(owner.to_person(), "Who broke the build?");
        let id = ask.question_id;
        owner.asks.ask(member(asker), ask).await.unwrap();
        assert_eq!(
            next_update(&mut rx, asker).await.status,
            QuestionStatus::Received
        );

        // Routing is under way: no second answer, but a cancel goes through.
        let error = owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Refresh)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        let canceled = owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Cancel)
            .await
            .unwrap();
        assert_eq!(canceled.status, QuestionStatus::Canceled);
        assert_eq!(
            next_update(&mut rx, asker).await.status,
            QuestionStatus::Canceled
        );
        // Canceling again is harmless.
        owner
            .asks
            .question_action(asker, id, PeerQuestionAction::Cancel)
            .await
            .unwrap();

        gate.add_permits(1);
        owner.idle().await;
        assert_eq!(owner.stored(id).status, QuestionStatus::Canceled);
        assert_eq!(owner.forks(), 0);
        // Only the repeated cancel was published after the cancel.
        let mut later = Vec::new();
        while let Ok(message) = rx.try_recv() {
            if let HubMessage::Event {
                event: PeerEvent::QuestionUpdate { question },
                ..
            } = message
            {
                later.push(question.status);
            }
        }
        assert!(
            later.iter().all(|s| *s == QuestionStatus::Canceled),
            "{later:?}"
        );
    }

    #[tokio::test]
    async fn unshared_agents_fail_the_question_without_a_fork() {
        let owner = Owner::scripted(&[("f1", "An answer.")], vec![]);
        owner
            .collab
            .put_share(&ShareSettings::private(owner.api.id))
            .unwrap();
        let asker = MemberId::generate();
        let mut rx = owner.hub.subscribe();
        owner
            .asks
            .ask(member(asker), peer_ask(owner.to_agent(), "How far?"))
            .await
            .unwrap();
        let (_, question) = statuses_until(&mut rx, asker, &[QuestionStatus::Failed]).await;
        assert_eq!(question.error.unwrap().code, ErrorCode::AgentNotShared);
        owner.idle().await;
        assert_eq!(owner.forks(), 0);
    }

    #[tokio::test]
    async fn resend_covers_open_questions_and_recently_final_ones() {
        let owner = Owner::scripted(&[], vec![]);
        let asker = MemberId::generate();
        let base = Question {
            id: QuestionId::generate(),
            thread_id: QuestionId::generate(),
            asker,
            target: owner.to_agent(),
            text: "How far?".into(),
            status: QuestionStatus::Answered,
            candidates: vec![],
            agent_id: Some(owner.api.id),
            answer: None,
            error: None,
            created_at: at(0),
            updated_at: at(0),
        };
        // The owner's clock reads 10:40 on its first call.
        let old = at(40) - RESEND_FINAL_FOR - chrono::Duration::minutes(1);
        let questions = [
            (QuestionStatus::Answered, old, false),
            (QuestionStatus::Canceled, old, false),
            (QuestionStatus::SentToOwner, old, true),
            (QuestionStatus::Unsure, old, true),
            (QuestionStatus::Failed, at(10), true),
        ];
        let mut expected = Vec::new();
        for (status, updated_at, sent) in questions {
            let id = QuestionId::generate();
            let question = Question {
                id,
                thread_id: id,
                status,
                updated_at,
                ..base.clone()
            };
            owner.collab.put_question(&question).unwrap();
            if sent {
                expected.push(id);
            }
        }
        owner
            .collab
            .put_question(&Question {
                asker: MemberId::generate(),
                ..base.clone()
            })
            .unwrap();
        let mut resent: Vec<QuestionId> = owner
            .asks
            .questions_to_resend(asker)
            .await
            .unwrap()
            .into_iter()
            .map(|q| q.id)
            .collect();
        resent.sort();
        expected.sort();
        assert_eq!(resent, expected);
    }

    #[tokio::test]
    async fn empty_questions_and_owner_callers_are_refused() {
        let owner = Owner::scripted(&[], vec![]);
        let error = owner
            .asks
            .ask(
                member(MemberId::generate()),
                peer_ask(owner.to_agent(), "  "),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidParams);
        let error = owner
            .asks
            .ask(Caller::Owner, peer_ask(owner.to_agent(), "How far?"))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotMember);
        assert!(owner.collab.questions().unwrap().is_empty());
    }
}
