//! The owner's side of the ask pipeline (tech spec, "Ask pipeline"; backend
//! architecture, "Request handling"): route a person-level question, answer from
//! a tool-less fork of the agent's session through the [`Harness`] port, post-
//! process the answer, continue a thread with follow-ups, and refresh.
//!
//! The access check (membership, chain, rate) happens before a question gets
//! here, in `Policy::check_ask`. The pipeline still re-checks sharing on every
//! step, because the pool it is given holds only shared agents: a question whose
//! agent was unshared meanwhile fails with `agent_not_shared` and nothing is
//! forked (US-009).
//!
//! What happens to the agent's main session: nothing. Every answer is
//! `Harness::run_fork` with [`ForkTools::None`]; the pipeline never calls
//! `run_turn`, `steer`, or anything else that writes the main session or its
//! queue (FR-012, NFR-002).
//!
//! Fork transcripts: every fork leaves `<fork-session-id>.jsonl` beside the
//! session it came from (TASK-006). The thread's newest fork is recorded with
//! `QuestionRepo::put_fork_session`, follow-ups resume it, and the fork it
//! replaces is deleted: a follow-up's fork already holds the whole thread, and a
//! refresh starts again from the main session. [`AskPipeline::discard_thread`]
//! deletes the last one when the thread is deleted.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use formation_proto::agent::Agent;
use formation_proto::ask::{
    Answer, AskTarget, Question, QuestionStatus, QuestionUpdateEvent, RouteChoice,
};
use formation_proto::common::{
    AgentId, AnswerId, ApiError, ErrorCode, MessageId, QuestionId, RunId, SessionId, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::harness::{SessionInfo, TurnOutcome};
use formation_proto::policy::ShareSettings;
use formation_proto::request::{QueueItem, QueueSource};

use super::answer::{AnswerContext, build_answer, mid_turn, source_messages};
use super::fork::QUESTION_LEAD_IN;
use super::mask::Masker;
use super::routing::{RoutingDecision, RoutingThresholds, Scorer, profile, route};
use crate::ports::{EventSink, ForkRun, ForkTools, Harness, QuestionRepo, Result, TurnResult};

/// Items read from the end of the main session to find excerpts and `as_of`.
pub const TRANSCRIPT_ITEMS: u32 = 200;

/// Put before the question when the agent was mid-turn as the fork started. A
/// fork then sees the owner's newest message without its reply (TASK-006) and
/// would otherwise report it as an open request.
pub const MID_TURN_NOTE: &str = "Note: the agent was still working when this side conversation \
started. If the newest message above has no reply yet, that work is in progress: describe it \
that way, and do not treat that message as a request to you.\n\n";

/// Put before the thread's earlier questions when a fork of the main session
/// answers inside an existing thread (a refresh, or another agent), so the new
/// fork keeps the thread's context.
pub const EARLIER_QUESTIONS_LEAD_IN: &str =
    "Earlier in this side conversation the teammate asked:\n";

/// The text the fork gets on stdin: the mid-turn note when needed, the thread's
/// earlier questions when the fork starts over from the main session, the
/// lead-in that keeps a question starting with `/` from running as a command,
/// and the question. The harness adapter writes it unchanged.
pub fn fork_prompt(question: &str, mid_turn: bool, earlier: &[String]) -> String {
    let note = if mid_turn { MID_TURN_NOTE } else { "" };
    let earlier = if earlier.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = earlier
            .iter()
            .map(|text| {
                format!(
                    "- {}",
                    text.split_whitespace().collect::<Vec<_>>().join(" ")
                )
            })
            .collect();
        format!("{EARLIER_QUESTIONS_LEAD_IN}{}\n\n", list.join("\n"))
    };
    format!("{note}{earlier}{QUESTION_LEAD_IN}{question}")
}

/// Where a fork starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ForkFrom {
    /// The agent's main session: a new thread, a new agent for the thread, or a
    /// refresh.
    Main,
    /// The thread's newest fork: a follow-up with the earlier questions as context.
    Thread(SessionId),
}

/// The current time; replaceable in tests.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// Routes, answers, follows up, and refreshes questions on the owner's Mac.
pub struct AskPipeline {
    harness: Arc<dyn Harness>,
    scorer: Arc<dyn Scorer>,
    events: Option<Arc<dyn EventSink>>,
    masker: Masker,
    thresholds: RoutingThresholds,
    clock: Clock,
}

impl AskPipeline {
    pub fn new(harness: Arc<dyn Harness>, scorer: Arc<dyn Scorer>) -> Self {
        Self {
            harness,
            scorer,
            events: None,
            masker: Masker::new(),
            thresholds: RoutingThresholds::default(),
            clock: Arc::new(|| SystemTime::now().into()),
        }
    }

    /// Publishes `question.update` for every state the pipeline stores.
    pub fn with_events(mut self, events: Arc<dyn EventSink>) -> Self {
        self.events = Some(events);
        self
    }

    /// Masks the owner's email addresses as well (NFR-003).
    pub fn with_masker(mut self, masker: Masker) -> Self {
        self.masker = masker;
        self
    }

    pub fn with_thresholds(mut self, thresholds: RoutingThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Takes a received question to its next resting state and stores it:
    /// `answered`, `unsure` (with candidates), or `failed` (with the error).
    ///
    /// - A follow-up (`thread_id` differs from `id`) is answered by the thread's
    ///   agent, in the thread's fork.
    /// - A question to an agent is answered by that agent.
    /// - A question to a person is answered by the agent already chosen in
    ///   `agent_id`, or routed among `pool`.
    ///
    /// `pool` is the owner's shared agents with their settings
    /// ([`crate::policy::sharing::shared_agents`]). Errors are stored on the
    /// question; the result is an error only when storing fails.
    pub async fn handle<R>(
        &self,
        repo: &R,
        pool: &[(Agent, ShareSettings)],
        question: &mut Question,
    ) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        if question.thread_id != question.id {
            return self.follow_up(repo, pool, question).await;
        }
        let agent_id = match (question.target, question.agent_id) {
            (AskTarget::Agent { agent_id, .. }, _) => agent_id,
            (AskTarget::Person { .. }, Some(agent_id)) => agent_id,
            (AskTarget::Person { .. }, None) => match self.route(pool, &question.text).await {
                Ok(RoutingDecision::Confident { agent_id, .. }) => agent_id,
                Ok(RoutingDecision::Unsure { candidates }) => {
                    question.status = QuestionStatus::Unsure;
                    question.candidates = candidates;
                    question.agent_id = None;
                    question.error = None;
                    return self.store(repo, question);
                }
                Err(error) => return self.fail(repo, question, error),
            },
        };
        self.answer(repo, pool, question, agent_id, ForkFrom::Main)
            .await
    }

    /// The asker's choice after an `unsure` routing or an answer (SC-015): another
    /// agent answers in a new fork of its own session, or the question goes to
    /// the owner.
    pub async fn choose<R>(
        &self,
        repo: &R,
        pool: &[(Agent, ShareSettings)],
        question: &mut Question,
        choice: RouteChoice,
    ) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        match choice {
            RouteChoice::Agent { agent_id } => {
                question.candidates.clear();
                self.answer(repo, pool, question, agent_id, ForkFrom::Main)
                    .await
            }
            RouteChoice::Owner => {
                question.status = QuestionStatus::SentToOwner;
                question.agent_id = None;
                question.error = None;
                self.store(repo, question)
            }
        }
    }

    /// The owner answers a question sent to them in person (US-010). The answer
    /// names no agent and has no excerpts; its text is masked like an agent's
    /// (NFR-003), and it is stored and published like any answer.
    pub fn answer_in_person<R>(&self, repo: &R, question: &mut Question, text: &str) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        let at = (self.clock)();
        question.status = QuestionStatus::Answered;
        question.agent_id = None;
        question.candidates.clear();
        question.error = None;
        question.answer = Some(Answer {
            id: AnswerId::generate(),
            agent_id: None,
            text: self.masker.mask(text),
            excerpts: Vec::new(),
            as_of: at,
            possibly_stale: false,
            answered_at: at,
        });
        self.store(repo, question)
    }

    /// Answers the question again from the agent's latest session content, in a
    /// new fork of the main session that replaces the thread's fork (FR-013,
    /// US-016). The thread's earlier questions are restated to the new fork, so
    /// later follow-ups keep their context. A question without an agent is
    /// routed again.
    pub async fn refresh<R>(
        &self,
        repo: &R,
        pool: &[(Agent, ShareSettings)],
        question: &mut Question,
    ) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        let agent_id = match question.agent_id {
            Some(agent_id) => Some(agent_id),
            None if question.thread_id != question.id => {
                thread_head(repo, question.thread_id)?.and_then(|head| head.agent_id)
            }
            None => None,
        };
        match agent_id {
            Some(agent_id) => {
                self.answer(repo, pool, question, agent_id, ForkFrom::Main)
                    .await
            }
            None => {
                question.status = QuestionStatus::Received;
                question.candidates.clear();
                self.handle(repo, pool, question).await
            }
        }
    }

    /// Deletes the thread's fork transcript when the thread is deleted (FR-013).
    /// Call it before the thread's questions leave the repository: the agent
    /// that made the fork is found from them. Returns whether a file was removed.
    pub async fn discard_thread<R>(&self, repo: &R, thread_id: QuestionId) -> Result<bool>
    where
        R: QuestionRepo + ?Sized,
    {
        let Some(fork) = repo.fork_session(thread_id)? else {
            return Ok(false);
        };
        let Some(agent_id) = thread_head(repo, thread_id)?.and_then(|head| head.agent_id) else {
            return Ok(false);
        };
        let Some(main) = self.harness.session_info(agent_id).await? else {
            return Ok(false);
        };
        Ok(
            remove_fork_transcript(&main.transcript_path, &fork, &[&main.session_id])
                .unwrap_or(false),
        )
    }

    async fn follow_up<R>(
        &self,
        repo: &R,
        pool: &[(Agent, ShareSettings)],
        question: &mut Question,
    ) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        let Some(agent_id) = thread_head(repo, question.thread_id)?.and_then(|head| head.agent_id)
        else {
            let error = ApiError::new(ErrorCode::NotFound, "no answered thread to follow up");
            return self.fail(repo, question, error);
        };
        let from = match repo.fork_session(question.thread_id)? {
            Some(fork) => ForkFrom::Thread(fork),
            // The thread's fork is gone: start again from the main session.
            None => ForkFrom::Main,
        };
        self.answer(repo, pool, question, agent_id, from).await
    }

    async fn route(&self, pool: &[(Agent, ShareSettings)], text: &str) -> Result<RoutingDecision> {
        let mut profiles = Vec::with_capacity(pool.len());
        for (agent, _) in pool.iter().filter(|(_, share)| share.shared) {
            profiles.push(profile(self.harness.as_ref(), agent).await);
        }
        route(
            self.scorer.as_ref(),
            text,
            &profiles,
            self.thresholds,
            &self.masker,
        )
        .await
    }

    async fn answer<R>(
        &self,
        repo: &R,
        pool: &[(Agent, ShareSettings)],
        question: &mut Question,
        agent_id: AgentId,
        from: ForkFrom,
    ) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        // Fail closed even if a caller's pool holds a private agent.
        let Some((_, share)) = pool.iter().find(|(agent, share)| {
            agent.id == agent_id && share.agent_id == agent_id && share.shared
        }) else {
            let error = ApiError::new(ErrorCode::AgentNotShared, "the agent is not shared");
            return self.fail(repo, question, error);
        };
        // The thread's newest answer, read before this question is stored again:
        // its agent owns the thread's fork, and its `as_of` is the fallback for a
        // follow-up that cites nothing new.
        let head = thread_head(repo, question.thread_id)?;
        question.agent_id = Some(agent_id);
        question.status = QuestionStatus::Answering;
        question.error = None;
        self.store(repo, question)?;

        // A thread's fork holds the main session only up to when it was forked,
        // which is before the thread's newest answer: newer main-session messages
        // are neither citable nor a date for an answer that cites nothing.
        let (previous, earlier) = match &from {
            ForkFrom::Thread(_) => (
                head.as_ref()
                    .and_then(|head| head.answer.as_ref())
                    .map(|answer| (answer.as_of, answer.answered_at)),
                Vec::new(),
            ),
            ForkFrom::Main => (None, earlier_questions(repo, question)?),
        };
        match self
            .run_answer(question, agent_id, share, from, previous, &earlier)
            .await
        {
            Ok((answer, fork, main)) => {
                let previous = repo.fork_session(question.thread_id)?;
                repo.put_fork_session(question.thread_id, &fork.session_id)?;
                if let Some(previous) = previous.filter(|p| *p != fork.session_id) {
                    self.remove_replaced_fork(
                        &previous,
                        head.and_then(|h| h.agent_id),
                        &main,
                        &fork,
                    )
                    .await;
                }
                question.status = QuestionStatus::Answered;
                question.answer = Some(answer);
                question.candidates.clear();
                self.store(repo, question)
            }
            Err(error) => self.fail(repo, question, error),
        }
    }

    /// Deletes the fork a new answer replaced. It lies beside the sessions of the
    /// agent that made it: this agent's, or the previous agent's when the asker
    /// chose another one. Best effort: a fork left behind only costs disk space.
    async fn remove_replaced_fork(
        &self,
        previous: &SessionId,
        previous_agent: Option<AgentId>,
        main: &SessionInfo,
        fork: &SessionInfo,
    ) {
        let mut keep = vec![main.session_id.clone(), fork.session_id.clone()];
        let beside = match previous_agent {
            Some(other) if other != main.agent_id => match self.harness.session_info(other).await {
                Ok(Some(other_main)) => {
                    keep.push(other_main.session_id.clone());
                    other_main.transcript_path
                }
                _ => return,
            },
            _ => fork.transcript_path.clone(),
        };
        let keep: Vec<&SessionId> = keep.iter().collect();
        let _ = remove_fork_transcript(&beside, previous, &keep);
    }

    /// Forks, waits for the answer, and post-processes it. `previous` is the
    /// thread's newest answer's `as_of` and `answered_at` when the fork resumes
    /// the thread. Returns the answer, the fork's session, and the main session.
    async fn run_answer(
        &self,
        question: &Question,
        agent_id: AgentId,
        share: &ShareSettings,
        from: ForkFrom,
        previous: Option<(Timestamp, Timestamp)>,
        earlier: &[String],
    ) -> Result<(Answer, SessionInfo, SessionInfo)> {
        let main = self.harness.session_info(agent_id).await?.ok_or_else(|| {
            ApiError::new(
                ErrorCode::HarnessUnavailable,
                "the agent has no session to answer from",
            )
        })?;
        let items = self
            .harness
            .conversation(agent_id, None, None, TRANSCRIPT_ITEMS)
            .await
            .map(|page| page.items)
            .unwrap_or_default();
        let started = (self.clock)();
        let seen_until = previous.map_or(started, |(_, answered_at)| answered_at.min(started));
        let sources = source_messages(&items, seen_until);
        // An answer that cites nothing is dated by the thread's previous answer,
        // else the newest message the fork saw, else the session's own last
        // activity; never by the time of answering.
        let fallback_as_of = previous
            .map(|(as_of, _)| as_of)
            .or(sources.last().map(|message| message.at))
            .or(main.last_activity_at)
            .or(Some(main.started_at));
        let (from_session, note) = match from {
            ForkFrom::Main => (main.session_id.clone(), mid_turn(&items)),
            ForkFrom::Thread(fork) => (fork, false),
        };
        let item = QueueItem {
            id: MessageId::generate(),
            agent_id,
            // A tool-less fork asks no permission, so nothing is attributed to
            // this source; the asker is recorded on the question.
            source: QueueSource::Owner,
            text: fork_prompt(&question.text, note, earlier),
            queued_at: started,
        };
        let fork = self
            .harness
            .run_fork(
                agent_id,
                ForkRun {
                    run_id: RunId::generate(),
                    from: from_session.clone(),
                    item,
                    tools: ForkTools::None,
                },
            )
            .await?;
        if fork.session.session_id == main.session_id || fork.session.session_id == from_session {
            return Err(ApiError::new(
                ErrorCode::Internal,
                "the harness answered in the session it should have forked",
            ));
        }
        let text = match completed_text(fork.result) {
            Ok(text) => text,
            Err(error) => {
                // No answer refers to this fork, so nothing would delete it later.
                let _ = remove_fork_transcript(
                    &fork.session.transcript_path,
                    &fork.session.session_id,
                    &[&main.session_id, &from_session],
                );
                return Err(error);
            }
        };
        let answer = build_answer(
            &text,
            &AnswerContext {
                agent_id,
                sources: &sources,
                visibility: share.visibility,
                masker: &self.masker,
                answered_at: (self.clock)(),
                fallback_as_of,
            },
        );
        Ok((answer, fork.session, main))
    }

    /// Stores the question as failed. The error reaches the asker, and a harness
    /// error may carry CLI output, so its message is masked like an answer.
    fn fail<R>(&self, repo: &R, question: &mut Question, mut error: ApiError) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        error.message = self.masker.mask(&error.message);
        question.status = QuestionStatus::Failed;
        question.error = Some(error);
        self.store(repo, question)
    }

    fn store<R>(&self, repo: &R, question: &mut Question) -> Result<()>
    where
        R: QuestionRepo + ?Sized,
    {
        question.updated_at = (self.clock)();
        repo.put_question(question)?;
        if let Some(events) = &self.events {
            events.publish(Event::QuestionUpdate(QuestionUpdateEvent::Upsert {
                question: Box::new(question.clone()),
            }));
        }
        Ok(())
    }
}

/// The thread's newest answered question. Its agent made the thread's recorded
/// fork. `QuestionRepo` has no query by thread, so this reads every question;
/// a member's question count stays small.
fn thread_head<R>(repo: &R, thread_id: QuestionId) -> Result<Option<Question>>
where
    R: QuestionRepo + ?Sized,
{
    Ok(repo
        .questions()?
        .into_iter()
        .filter(|q| q.thread_id == thread_id && q.status == QuestionStatus::Answered)
        .filter(|q| q.agent_id.is_some())
        .filter_map(|q| Some((q.answer.as_ref()?.answered_at, q)))
        .max_by_key(|(answered_at, _)| *answered_at)
        .map(|(_, q)| q))
}

/// The texts of the thread's questions asked before this one, oldest first.
fn earlier_questions<R>(repo: &R, question: &Question) -> Result<Vec<String>>
where
    R: QuestionRepo + ?Sized,
{
    let mut earlier: Vec<Question> = repo
        .questions()?
        .into_iter()
        .filter(|q| q.thread_id == question.thread_id && q.id != question.id)
        .filter(|q| (q.created_at, q.id) < (question.created_at, question.id))
        .collect();
    earlier.sort_by_key(|q| (q.created_at, q.id));
    Ok(earlier.into_iter().map(|q| q.text).collect())
}

/// The fork's reply when its turn completed with text.
fn completed_text(result: TurnResult) -> Result<String> {
    match (result.outcome, result.result) {
        (TurnOutcome::Completed, Some(text)) if !text.trim().is_empty() => Ok(text),
        (outcome, _) => Err(ApiError::new(
            ErrorCode::Internal,
            format!("the side conversation ended without an answer ({outcome:?})"),
        )),
    }
}

/// Deletes the transcript of fork `session`, which lies beside `beside` (any
/// transcript in the same project folder). Refuses session IDs in `keep`, and IDs
/// that are not plain `[A-Za-z0-9-]` names, so it can never delete the main
/// session or a path outside the folder. `Ok(false)` when there was no file.
pub fn remove_fork_transcript(
    beside: &Path,
    session: &SessionId,
    keep: &[&SessionId],
) -> io::Result<bool> {
    let id = session.0.as_str();
    let plain = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !plain || keep.contains(&session) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to delete transcript {id:?}"),
        ));
    }
    let Some(folder) = beside.parent() else {
        return Ok(false);
    };
    match std::fs::remove_file(folder.join(format!("{id}.jsonl"))) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    use formation_proto::agent::{HarnessKind, HarnessStatus};
    use formation_proto::ask::ExcerptRole;
    use formation_proto::common::{ItemId, MemberId};
    use formation_proto::harness::{Author, ConversationItem, ConversationPageResult};
    use formation_proto::policy::Visibility;

    use super::*;
    use crate::ask::mask::MASK;
    use crate::ask::routing::test_support::ScriptedScorer;
    use crate::ask::routing::{AgentScore, RECENT_ITEMS};
    use crate::ask::test_support::TempDir;
    use crate::policy::sharing::test_support::agent;
    use crate::ports::ForkResult;
    use crate::testkit::{
        FakeHarness, HarnessCall, RecordingEventSink, ScriptedAgent, ScriptedPage,
    };

    const MAIN: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const WEB_MAIN: &str = "bbbbbbbb-0000-4000-8000-000000000002";

    /// 10:00 UTC plus `minute` minutes.
    fn at(minute: i64) -> Timestamp {
        let total = 10 * 60 + minute;
        format!("2026-11-09T{:02}:{:02}:00Z", total / 60, total % 60)
            .parse()
            .unwrap()
    }

    /// `QuestionRepo` in memory, until the testkit's in-memory `CollabStore`
    /// (TASK-042) lands.
    #[derive(Default)]
    struct Questions {
        questions: Mutex<HashMap<QuestionId, Question>>,
        forks: Mutex<HashMap<QuestionId, SessionId>>,
    }

    impl QuestionRepo for Questions {
        fn question(&self, id: QuestionId) -> Result<Option<Question>> {
            Ok(self.questions.lock().unwrap().get(&id).cloned())
        }
        fn questions(&self) -> Result<Vec<Question>> {
            Ok(self.questions.lock().unwrap().values().cloned().collect())
        }
        fn put_question(&self, question: &Question) -> Result<()> {
            self.questions
                .lock()
                .unwrap()
                .insert(question.id, question.clone());
            Ok(())
        }
        fn delete_question(&self, id: QuestionId) -> Result<()> {
            self.questions.lock().unwrap().remove(&id);
            Ok(())
        }
        fn fork_session(&self, thread_id: QuestionId) -> Result<Option<SessionId>> {
            Ok(self.forks.lock().unwrap().get(&thread_id).cloned())
        }
        fn put_fork_session(&self, thread_id: QuestionId, session_id: &SessionId) -> Result<()> {
            self.forks
                .lock()
                .unwrap()
                .insert(thread_id, session_id.clone());
            Ok(())
        }
    }

    /// One owner's Mac: agents, their transcripts on disk, a fake harness with
    /// scripted forks, and a clock that moves a minute per reading.
    struct Owner {
        dir: TempDir,
        api: Agent,
        web: Agent,
        harness: Arc<FakeHarness>,
        events: Arc<RecordingEventSink>,
        repo: Questions,
        minute: Arc<AtomicI64>,
    }

    fn main_items() -> Vec<ConversationItem> {
        vec![
            ConversationItem::UserMessage {
                id: ItemId("u1".into()),
                at: at(0),
                author: Author::Owner,
                text:
                    "Clean up the login API endpoints. Use sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx."
                        .into(),
            },
            ConversationItem::AssistantText {
                id: ItemId("a1".into()),
                at: at(31),
                text: "Endpoints cleaned up. Next I am adding a token expiry check.".into(),
            },
        ]
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

    fn fork(dir: &Path, agent: &Agent, id: &str, text: &str) -> ForkResult {
        ForkResult {
            session: session(dir, agent, id),
            result: TurnResult {
                outcome: TurnOutcome::Completed,
                result: Some(text.into()),
            },
        }
    }

    fn page(limit: u32, items: Vec<ConversationItem>) -> ScriptedPage {
        ScriptedPage {
            session_id: None,
            before: None,
            limit,
            page: ConversationPageResult {
                items,
                has_more: false,
                stream_seq: 0,
            },
        }
    }

    impl Owner {
        /// `forks` are the API agent's scripted fork answers: (fork session ID, text).
        fn new(forks: &[(&str, &str)], items: Vec<ConversationItem>) -> Self {
            Self::with_web(forks, &[], items)
        }

        /// Also scripts the web agent's forks. Its transcripts live in `web/`.
        fn with_web(
            forks: &[(&str, &str)],
            web_forks: &[(&str, &str)],
            items: Vec<ConversationItem>,
        ) -> Self {
            let dir = TempDir::new("ask-pipeline");
            let api = agent("api", "API developer");
            let web = agent("web", "Frontend developer");
            fs::write(
                dir.path().join(format!("{MAIN}.jsonl")),
                "main transcript\n",
            )
            .unwrap();
            let mut scripted = ScriptedAgent::new(session(dir.path(), &api, MAIN), []);
            scripted.forks = forks
                .iter()
                .map(|(id, text)| fork(dir.path(), &api, id, text))
                .collect::<VecDeque<_>>();
            scripted.pages = vec![
                page(TRANSCRIPT_ITEMS, items.clone()),
                page(RECENT_ITEMS, items),
            ];
            let web_dir = dir.path().join("web");
            fs::create_dir_all(&web_dir).unwrap();
            fs::write(
                web_dir.join(format!("{WEB_MAIN}.jsonl")),
                "web transcript\n",
            )
            .unwrap();
            let mut web_scripted = ScriptedAgent::new(session(&web_dir, &web, WEB_MAIN), []);
            web_scripted.forks = web_forks
                .iter()
                .map(|(id, text)| fork(&web_dir, &web, id, text))
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
                [scripted, web_scripted],
            ));
            Self {
                dir,
                api,
                web,
                harness,
                events: Arc::new(RecordingEventSink::default()),
                repo: Questions::default(),
                minute: Arc::new(AtomicI64::new(40)),
            }
        }

        fn pipeline(&self, scorer: ScriptedScorer) -> AskPipeline {
            let minute = self.minute.clone();
            AskPipeline::new(self.harness.clone(), Arc::new(scorer))
                .with_events(self.events.clone())
                .with_masker(Masker::with_owner_emails(["dohyeon@example.com"]))
                .with_clock(Arc::new(move || at(minute.fetch_add(1, Ordering::SeqCst))))
        }

        fn pool(&self, visibility: Visibility) -> Vec<(Agent, ShareSettings)> {
            vec![(
                self.api.clone(),
                ShareSettings {
                    shared: true,
                    visibility,
                    ..ShareSettings::private(self.api.id)
                },
            )]
        }

        fn shared_both(&self) -> Vec<(Agent, ShareSettings)> {
            let mut pool = self.pool(Visibility::AnswerExcerpts);
            pool.push((
                self.web.clone(),
                ShareSettings {
                    shared: true,
                    ..ShareSettings::private(self.web.id)
                },
            ));
            pool
        }

        fn transcript(&self, id: &str) -> PathBuf {
            self.dir.path().join(format!("{id}.jsonl"))
        }

        fn forks(&self) -> Vec<(SessionId, String)> {
            self.harness
                .calls()
                .into_iter()
                .map(|call| match call {
                    HarnessCall::RunFork {
                        from, item, tools, ..
                    } => {
                        assert_eq!(tools, ForkTools::None);
                        (from, item.text)
                    }
                    other => panic!("the main session was touched: {other:?}"),
                })
                .collect()
        }
    }

    fn question(target: AskTarget, text: &str) -> Question {
        let id = QuestionId::generate();
        Question {
            id,
            thread_id: id,
            asker: MemberId::generate(),
            target,
            text: text.into(),
            status: QuestionStatus::Received,
            candidates: vec![],
            agent_id: None,
            answer: None,
            error: None,
            created_at: at(39),
            updated_at: at(39),
        }
    }

    fn follow_up(thread: &Question, text: &str) -> Question {
        Question {
            thread_id: thread.id,
            ..question(thread.target, text)
        }
    }

    fn to_agent(agent: &Agent) -> AskTarget {
        AskTarget::Agent {
            member_id: MemberId::generate(),
            agent_id: agent.id,
        }
    }

    fn to_person() -> AskTarget {
        AskTarget::Person {
            member_id: MemberId::generate(),
        }
    }

    const ANSWER: &str = "The login API cleanup is done; a token expiry check is next. Signed in \
        as dohyeon@example.com.\n\nMessages used:\n- \"Endpoints cleaned up\"";

    #[tokio::test]
    async fn an_agent_answers_from_a_tool_less_fork() {
        let owner = Owner::new(&[("f1", ANSWER)], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "/usage how far is the login work?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();

        assert_eq!(q.status, QuestionStatus::Answered, "{:?}", q.error);
        assert_eq!(q.agent_id, Some(owner.api.id));
        let answer = q.answer.as_ref().unwrap();
        assert_eq!(answer.agent_id, Some(owner.api.id));
        assert!(answer.text.starts_with("The login API cleanup is done"));
        assert!(answer.text.contains(MASK) && !answer.text.contains("dohyeon@"));
        assert_eq!(answer.excerpts.len(), 1);
        assert_eq!(answer.excerpts[0].role, ExcerptRole::Assistant);
        assert_eq!(answer.as_of, at(31));
        assert!(!answer.possibly_stale);

        // One fork of the main session, without tools, the question on stdin
        // after the lead-in; nothing else reached the harness.
        let forks = owner.forks();
        assert_eq!(forks.len(), 1);
        assert_eq!(forks[0].0, SessionId(MAIN.into()));
        assert_eq!(
            forks[0].1,
            format!("{QUESTION_LEAD_IN}/usage how far is the login work?")
        );
        assert_eq!(
            owner.repo.fork_session(q.thread_id).unwrap(),
            Some(SessionId("f1".into()))
        );
        assert!(owner.transcript(MAIN).exists());

        // The asker sees answering, then answered.
        let statuses: Vec<QuestionStatus> = owner
            .events
            .events()
            .into_iter()
            .map(|(_, event)| match event {
                Event::QuestionUpdate(QuestionUpdateEvent::Upsert { question }) => question.status,
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(
            statuses,
            [QuestionStatus::Answering, QuestionStatus::Answered]
        );
        assert_eq!(owner.repo.question(q.id).unwrap().unwrap(), q);
    }

    #[tokio::test]
    async fn answer_only_agents_send_no_excerpts() {
        let owner = Owner::new(&[("f1", ANSWER)], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::Answer), &mut q)
            .await
            .unwrap();
        let answer = q.answer.unwrap();
        assert!(answer.excerpts.is_empty());
        assert!(!answer.text.contains("Endpoints cleaned up"));
        assert_eq!(answer.as_of, at(31));
    }

    #[tokio::test]
    async fn private_agents_are_never_forked() {
        let owner = Owner::new(&[("f1", ANSWER)], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        // `web` is not in the pool: it is private.
        let mut q = question(to_agent(&owner.web), "How far?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        assert_eq!(q.status, QuestionStatus::Failed);
        assert_eq!(q.error.unwrap().code, ErrorCode::AgentNotShared);
        assert!(owner.harness.calls().is_empty());

        // Nothing shared: a person-level question has no agent to route to.
        let mut q = question(to_person(), "How far?");
        pipeline.handle(&owner.repo, &[], &mut q).await.unwrap();
        assert_eq!(q.error.unwrap().code, ErrorCode::AgentNotShared);
        assert!(owner.harness.calls().is_empty());
    }

    #[tokio::test]
    async fn person_questions_are_routed_among_shared_agents_only() {
        let owner = Owner::new(&[("f1", ANSWER)], main_items());
        let scorer = ScriptedScorer::new(Ok(vec![AgentScore {
            agent_id: owner.api.id,
            score: 0.9,
            reason: "Owns the login API".into(),
        }]));
        let pipeline = owner.pipeline(scorer);
        let mut q = question(to_person(), "How far is the login work?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        assert_eq!(q.status, QuestionStatus::Answered);
        assert_eq!(q.agent_id, Some(owner.api.id));
        assert_eq!(owner.forks().len(), 1);
    }

    #[tokio::test]
    async fn unsure_routing_lets_the_asker_choose() {
        let owner = Owner::new(&[("f1", ANSWER)], main_items());
        let scorer = ScriptedScorer::new(Ok(vec![AgentScore {
            agent_id: owner.api.id,
            score: 0.4,
            reason: "Maybe the API".into(),
        }]));
        let pipeline = owner.pipeline(scorer);
        let pool = owner.pool(Visibility::AnswerExcerpts);
        let mut q = question(to_person(), "Who broke the build?");
        pipeline.handle(&owner.repo, &pool, &mut q).await.unwrap();
        assert_eq!(q.status, QuestionStatus::Unsure);
        assert_eq!(q.candidates.len(), 1);
        assert_eq!(q.candidates[0].agent_id, owner.api.id);
        assert!(owner.forks().is_empty());

        pipeline
            .choose(
                &owner.repo,
                &pool,
                &mut q,
                RouteChoice::Agent {
                    agent_id: owner.api.id,
                },
            )
            .await
            .unwrap();
        assert_eq!(q.status, QuestionStatus::Answered);
        assert!(q.candidates.is_empty());

        let mut other = question(to_person(), "Who broke the build?");
        pipeline
            .choose(&owner.repo, &pool, &mut other, RouteChoice::Owner)
            .await
            .unwrap();
        assert_eq!(other.status, QuestionStatus::SentToOwner);
        assert_eq!(owner.forks().len(), 1);
    }

    #[tokio::test]
    async fn follow_ups_continue_the_thread_fork_and_refresh_starts_over() {
        let answers = [
            ("f1", ANSWER),
            (
                "f2",
                "It is adding the expiry check.\n\nMessages used:\n- \"token expiry check\"",
            ),
            ("f3", "Refreshed: the cleanup is done."),
        ];
        let owner = Owner::new(&answers, main_items());
        for (id, _) in &answers {
            fs::write(owner.transcript(id), "fork transcript\n").unwrap();
        }
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let pool = owner.pool(Visibility::AnswerExcerpts);

        let mut first = question(to_agent(&owner.api), "How far is the login work?");
        pipeline
            .handle(&owner.repo, &pool, &mut first)
            .await
            .unwrap();

        let mut next = follow_up(&first, "And the expiry check?");
        pipeline
            .handle(&owner.repo, &pool, &mut next)
            .await
            .unwrap();
        assert_eq!(next.status, QuestionStatus::Answered, "{:?}", next.error);
        assert_eq!(next.agent_id, Some(owner.api.id));
        assert_eq!(next.answer.as_ref().unwrap().as_of, at(31));

        pipeline
            .refresh(&owner.repo, &pool, &mut next)
            .await
            .unwrap();
        assert_eq!(next.status, QuestionStatus::Answered);
        assert_eq!(
            next.answer.as_ref().unwrap().text,
            "Refreshed: the cleanup is done."
        );

        let forks = owner.forks();
        let from: Vec<&str> = forks.iter().map(|(from, _)| from.0.as_str()).collect();
        // The follow-up resumed the thread's fork; the refresh went back to the
        // main session.
        assert_eq!(from, [MAIN, "f1", MAIN]);
        // The refresh restates the thread's earlier question to the new fork.
        assert_eq!(
            forks[2].1,
            format!(
                "{EARLIER_QUESTIONS_LEAD_IN}- How far is the login work?\n\n{QUESTION_LEAD_IN}And the \
                 expiry check?"
            )
        );
        assert_eq!(
            forks[1].1,
            format!("{QUESTION_LEAD_IN}And the expiry check?")
        );
        assert_eq!(
            owner.repo.fork_session(first.id).unwrap(),
            Some(SessionId("f3".into()))
        );
        // Replaced forks are deleted; the newest and the main session stay.
        assert!(!owner.transcript("f1").exists());
        assert!(!owner.transcript("f2").exists());
        assert!(owner.transcript("f3").exists());
        assert!(owner.transcript(MAIN).exists());

        assert!(
            pipeline
                .discard_thread(&owner.repo, first.id)
                .await
                .unwrap()
        );
        assert!(!owner.transcript("f3").exists());
        assert!(owner.transcript(MAIN).exists());
    }

    #[tokio::test]
    async fn a_follow_up_is_dated_by_what_its_fork_saw() {
        let mut items = main_items();
        items.push(ConversationItem::AssistantText {
            id: ItemId("a2".into()),
            at: at(50),
            text: "Started on the billing page.".into(),
        });
        let owner = Owner::new(&[("f1", ANSWER), ("f2", "Not established.")], items);
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let pool = owner.pool(Visibility::AnswerExcerpts);
        let mut first = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(&owner.repo, &pool, &mut first)
            .await
            .unwrap();
        assert_eq!(first.answer.as_ref().unwrap().as_of, at(31));

        // The main session moved on after the thread's fork was taken; a follow-up
        // in that fork that cites nothing keeps the thread's date.
        owner.minute.store(60, Ordering::SeqCst);
        let mut next = follow_up(&first, "Anything else?");
        pipeline
            .handle(&owner.repo, &pool, &mut next)
            .await
            .unwrap();
        assert_eq!(next.status, QuestionStatus::Answered, "{:?}", next.error);
        assert_eq!(next.answer.as_ref().unwrap().as_of, at(31));
    }

    #[tokio::test]
    async fn a_private_agent_in_the_pool_is_still_refused() {
        let owner = Owner::new(&[("f1", ANSWER)], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let pool = vec![(owner.api.clone(), ShareSettings::private(owner.api.id))];
        let mut q = question(to_agent(&owner.api), "How far?");
        pipeline.handle(&owner.repo, &pool, &mut q).await.unwrap();
        assert_eq!(q.status, QuestionStatus::Failed);
        assert_eq!(q.error.unwrap().code, ErrorCode::AgentNotShared);
        let mut q = question(to_person(), "How far?");
        pipeline.handle(&owner.repo, &pool, &mut q).await.unwrap();
        assert_eq!(q.error.unwrap().code, ErrorCode::AgentNotShared);
        assert!(owner.forks().is_empty());
    }

    #[tokio::test]
    async fn a_fork_without_an_answer_leaves_no_transcript() {
        let owner = Owner::new(&[("f1", "  ")], main_items());
        fs::write(owner.transcript("f1"), "fork transcript\n").unwrap();
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        assert_eq!(q.status, QuestionStatus::Failed);
        assert!(!owner.transcript("f1").exists());
        assert!(owner.transcript(MAIN).exists());
    }

    #[tokio::test]
    async fn an_unshared_thread_agent_stops_follow_ups() {
        let owner = Owner::new(&[("f1", ANSWER), ("f2", ANSWER)], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut first = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(
                &owner.repo,
                &owner.pool(Visibility::AnswerExcerpts),
                &mut first,
            )
            .await
            .unwrap();
        let mut next = follow_up(&first, "And now?");
        pipeline.handle(&owner.repo, &[], &mut next).await.unwrap();
        assert_eq!(next.status, QuestionStatus::Failed);
        assert_eq!(next.error.unwrap().code, ErrorCode::AgentNotShared);
        assert_eq!(owner.forks().len(), 1);
    }

    #[tokio::test]
    async fn choosing_another_agent_moves_the_thread_to_its_fork() {
        let owner = Owner::with_web(
            &[("f1", ANSWER)],
            &[("w1", "The web side is done."), ("w2", "Still done.")],
            main_items(),
        );
        fs::write(owner.transcript("f1"), "fork transcript\n").unwrap();
        for id in ["w1", "w2"] {
            fs::write(
                owner.dir.path().join("web").join(format!("{id}.jsonl")),
                "fork transcript\n",
            )
            .unwrap();
        }
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let pool = owner.shared_both();

        let mut first = question(to_agent(&owner.api), "How far is the login work?");
        pipeline
            .handle(&owner.repo, &pool, &mut first)
            .await
            .unwrap();
        // "Ask another agent" after the answer.
        pipeline
            .choose(
                &owner.repo,
                &pool,
                &mut first,
                RouteChoice::Agent {
                    agent_id: owner.web.id,
                },
            )
            .await
            .unwrap();
        assert_eq!(first.agent_id, Some(owner.web.id));
        assert_eq!(first.answer.as_ref().unwrap().text, "The web side is done.");
        // The API agent's fork is deleted from its own folder.
        assert!(!owner.transcript("f1").exists());
        assert!(owner.transcript(MAIN).exists());

        // A follow-up goes to the agent that answered last, in its fork.
        let mut next = follow_up(&first, "Anything left?");
        pipeline
            .handle(&owner.repo, &pool, &mut next)
            .await
            .unwrap();
        assert_eq!(next.agent_id, Some(owner.web.id));
        let calls = owner.harness.calls();
        let HarnessCall::RunFork { agent_id, from, .. } = &calls[2] else {
            panic!("expected a fork: {calls:?}");
        };
        assert_eq!((*agent_id, from.0.as_str()), (owner.web.id, "w1"));
        let web = owner.dir.path().join("web");
        assert!(!web.join("w1.jsonl").exists());
        assert!(web.join("w2.jsonl").exists());
        assert!(web.join(format!("{WEB_MAIN}.jsonl")).exists());
    }

    #[tokio::test]
    async fn without_readable_messages_the_session_dates_the_answer() {
        let owner = Owner::new(&[("f1", "Not established.")], vec![]);
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        let answer = q.answer.unwrap();
        // The session's last activity, not the time of answering.
        assert_eq!(answer.as_of, at(31));
        assert!(answer.answered_at > at(40));
    }

    #[tokio::test]
    async fn a_fork_taken_mid_turn_carries_a_note() {
        let mut items = main_items();
        items.push(ConversationItem::UserMessage {
            id: ItemId("u2".into()),
            at: at(35),
            author: Author::Owner,
            text: "Now add a refresh token.".into(),
        });
        let owner = Owner::new(&[("f1", ANSWER)], items);
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "What is it doing?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        let forks = owner.forks();
        assert_eq!(
            forks[0].1,
            format!("{MID_TURN_NOTE}{QUESTION_LEAD_IN}What is it doing?")
        );
        assert!(!forks[0].1.starts_with('/'));
    }

    #[tokio::test]
    async fn failed_or_unforked_answers_fail_the_question() {
        // No scripted fork: the harness errors.
        let owner = Owner::new(&[], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        assert_eq!(q.status, QuestionStatus::Failed);
        assert!(q.answer.is_none());

        // A harness that answers in the main session itself is refused.
        let owner = Owner::new(&[(MAIN, ANSWER)], main_items());
        let pipeline = owner.pipeline(ScriptedScorer::new(Ok(vec![])));
        let mut q = question(to_agent(&owner.api), "How far?");
        pipeline
            .handle(&owner.repo, &owner.pool(Visibility::AnswerExcerpts), &mut q)
            .await
            .unwrap();
        assert_eq!(q.status, QuestionStatus::Failed);
        assert_eq!(q.error.unwrap().code, ErrorCode::Internal);
        assert_eq!(owner.repo.fork_session(q.thread_id).unwrap(), None);
        assert!(owner.transcript(MAIN).exists());

        assert!(
            completed_text(TurnResult {
                outcome: TurnOutcome::Interrupted,
                result: Some("partial".into()),
            })
            .is_err()
        );
    }

    #[test]
    fn fork_transcripts_are_deleted_only_by_plain_id() {
        let dir = TempDir::new("ask-forks");
        let main = SessionId(MAIN.into());
        let beside = dir.path().join(format!("{MAIN}.jsonl"));
        fs::write(&beside, "main").unwrap();
        fs::write(dir.path().join("f1.jsonl"), "fork").unwrap();
        for bad in ["", "../x", "a/b", MAIN] {
            assert!(
                remove_fork_transcript(&beside, &SessionId(bad.into()), &[&main]).is_err(),
                "{bad:?}"
            );
        }
        assert!(beside.exists());
        assert!(remove_fork_transcript(&beside, &SessionId("f1".into()), &[&main]).unwrap());
        assert!(!remove_fork_transcript(&beside, &SessionId("f1".into()), &[&main]).unwrap());
    }
}
