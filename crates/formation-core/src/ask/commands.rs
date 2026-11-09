//! The asker's side of questions and the owner's sharing settings, on the local
//! socket: `ask`, `question_*`, `share_update`, `share_preview`, and
//! `teammate_agents` (FR-009, FR-010, FR-013, FR-016).
//!
//! Questions go to the owner's Mac through the [`Peers`] port, and this Mac keeps
//! a copy of each one it asked ([`QuestionRepo`](crate::ports::QuestionRepo), D-029) that follows the owner's
//! updates. The design follows [`RequestClient`](crate::requests::RequestClient):
//!
//! - `ask` stores the question as `received` before sending it, so the app shows
//!   it at once. `peer_offline` means the [`Peers`] outbox holds it: the copy
//!   becomes `waiting_offline` and the command succeeds, because the question
//!   will still be delivered (FR-016). Any other refusal marks the copy `failed`
//!   and returns the error.
//! - The owner's Mac publishes every change to the question on its events
//!   connection. formationd feeds those to [`AskCommands::apply_update`], which
//!   keeps the newest copy.
//! - `question_cancel`, `question_refresh`, and `question_route_choose` act on the
//!   owner's Mac and store the copy it returns.
//!
//! `question_handoff` and `question_answer` are the owner's commands for a
//! question sent to them in person (SC-010). They change a question that lives on
//! this Mac and must reach the asker through the peer server's
//! [`PeerEventHub`](crate::peer::api::PeerEventHub), so they belong with the
//! owner's [`AskEndpoints`](crate::peer::api::AskEndpoints) adapter, which runs
//! the [`AskPipeline`] and publishes to the asker. Until that adapter serves them,
//! this handler answers them with an `internal` error naming the gap.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use formation_proto::ask::{
    Ask, AskTarget, Question, QuestionAnswer, QuestionCancel, QuestionDelete, QuestionHandoff,
    QuestionRefresh, QuestionRouteChoose, QuestionStatus, QuestionUpdateEvent,
};
use formation_proto::common::{
    ApiError, ChainId, Command, ErrorCode, MemberId, QuestionId, RunId, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::peer::{PeerAsk, PeerQuestionAction, SharedAgentView, TeammateAgents};
use formation_proto::policy::{
    Caller, Chain, MAX_CHAIN_DEPTH, SharePreview, ShareUpdate, ShareUpdateEvent,
};
use formation_proto::request::QueueSource;

use super::pipeline::AskPipeline;
use crate::policy::sharing::{self, ShareChange};
use crate::ports::{
    CollabStore, CommandCall, CommandHandler, EventSink, Harness, Peers, Result, Store,
};

/// Called after `share_update` stored a change, so formationd can act on what the
/// change stops (FR-009, US-009). formationd should:
///
/// - on every change, call [`PeerEventHub::agents_changed`](crate::peer::api::PeerEventHub::agents_changed)
///   so connected teammates get this Mac's new list of shared agents;
/// - on [`ShareChange::unshared`], cancel the agent's questions that are not
///   answered yet on this Mac (the [`AskPipeline`] already refuses to answer from
///   an unshared agent, so this only settles them sooner);
/// - on [`ShareChange::requests_stopped`], cancel the agent's teammate requests
///   that have not started (`received` or `queued`) with
///   [`Requests::cancel`](crate::requests::Requests::cancel)`(None, id)`. A
///   running request continues unless the owner stops it (FR-009).
pub type ShareHook = Arc<dyn Fn(&ShareChange) + Send + Sync>;

/// Serves the asker's question commands, sharing, and the teammate agent list.
/// Every method is for this Mac's owner only.
pub struct AskCommands {
    collab: Arc<dyn CollabStore>,
    peers: Arc<dyn Peers>,
    harness: Arc<dyn Harness>,
    agents: Arc<dyn Store>,
    events: Arc<dyn EventSink>,
    owner_pipeline: Option<Arc<AskPipeline>>,
    share_hook: Option<ShareHook>,
    /// Held around each read-compare-write of a local copy, because owner updates
    /// ([`AskCommands::apply_update`]) and command results race for the same row.
    /// Never held across an `await`.
    copies: Mutex<()>,
}

impl AskCommands {
    pub fn new(
        collab: Arc<dyn CollabStore>,
        peers: Arc<dyn Peers>,
        harness: Arc<dyn Harness>,
        agents: Arc<dyn Store>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            collab,
            peers,
            harness,
            agents,
            events,
            owner_pipeline: None,
            share_hook: None,
            copies: Mutex::new(()),
        }
    }

    /// The pipeline that answers teammates' questions on this Mac. Deleting a
    /// question asked of this Mac's agents needs it, to remove the thread's fork
    /// transcript (FR-013); without it such a delete is refused rather than
    /// leaving the transcript behind.
    pub fn with_owner_pipeline(mut self, pipeline: Arc<AskPipeline>) -> Self {
        self.owner_pipeline = Some(pipeline);
        self
    }

    /// See [`ShareHook`].
    pub fn with_share_hook(mut self, hook: ShareHook) -> Self {
        self.share_hook = Some(hook);
        self
    }

    /// Stores the question and sends it to the owner's Mac.
    ///
    /// - A follow-up names any question of the thread in `thread_id`; it must be a
    ///   question this Mac asked of the same teammate.
    /// - With `from_run`, one of this Mac's agents asks: the question continues the
    ///   chain of the request that run serves, one level deeper, or starts a chain
    ///   at depth 1 for the owner's own work. Deeper than [`MAX_CHAIN_DEPTH`] is
    ///   `rate_limited` (FR-019), checked here and again by the owner's Mac.
    /// - `peer_offline` returns the question `waiting_offline`; the outbox sends it
    ///   when the teammate's Mac is back. Any other refusal marks it `failed` and
    ///   returns the error.
    pub async fn ask(&self, command: Ask) -> Result<Question> {
        require_text("text", &command.text)?;
        let me = self.me()?;
        let owner = command.target.owner();
        if owner == me {
            return Err(error(
                ErrorCode::InvalidParams,
                "ask your own agents in their conversation instead",
            ));
        }
        self.require_teammate(owner)?;
        let thread_id = match command.thread_id {
            None => None,
            Some(id) => {
                let earlier = self
                    .collab
                    .question(id)?
                    .filter(|q| q.asker == me && q.target.owner() == owner)
                    .ok_or_else(|| {
                        error(
                            ErrorCode::NotFound,
                            "no question of yours starts this thread",
                        )
                    })?;
                Some(earlier.thread_id)
            }
        };
        let (chain, via_agent) = self.chain(command.from_run).await?;
        check_chain_depth(chain.depth)?;

        let id = QuestionId::generate();
        let at = now();
        let question = Question {
            id,
            thread_id: thread_id.unwrap_or(id),
            asker: me,
            target: command.target,
            text: command.text.clone(),
            status: QuestionStatus::Received,
            candidates: Vec::new(),
            agent_id: match command.target {
                AskTarget::Agent { agent_id, .. } => Some(agent_id),
                AskTarget::Person { .. } => None,
            },
            answer: None,
            error: None,
            created_at: at,
            updated_at: at,
        };
        self.collab.put_question(&question)?;
        self.publish(&question);

        let body = PeerAsk {
            question_id: id,
            thread_id,
            target: command.target,
            text: command.text,
            via_agent,
            chain,
        };
        match self.peers.ask(owner, body).await {
            // The owner's Mac publishes the question from now on; its first update
            // may already be stored.
            Ok(_) => self.local(id),
            Err(refusal) if refusal.code == ErrorCode::PeerOffline => {
                self.settle_unsent(id, QuestionStatus::WaitingOffline, None)
            }
            Err(refusal) => {
                self.settle_unsent(id, QuestionStatus::Failed, Some(refusal.clone()))?;
                Err(refusal)
            }
        }
    }

    /// Cancels on the owner's Mac, or removes the question from the outbox when it
    /// was never delivered. Canceling a canceled question returns it unchanged.
    pub async fn cancel(&self, id: QuestionId) -> Result<Question> {
        let local = self.asked(id)?;
        if local.status == QuestionStatus::Canceled {
            return Ok(local);
        }
        self.act(&local, PeerQuestionAction::Cancel).await
    }

    /// Asks the owner's Mac for a fresh answer (FR-013, US-016).
    pub async fn refresh(&self, id: QuestionId) -> Result<Question> {
        let local = self.asked(id)?;
        self.act(&local, PeerQuestionAction::Refresh).await
    }

    /// The asker's choice after an `unsure` routing or an answer (SC-015).
    pub async fn route_choose(&self, command: QuestionRouteChoose) -> Result<Question> {
        let local = self.asked(command.question_id)?;
        self.act(
            &local,
            PeerQuestionAction::Route {
                choice: command.choice,
            },
        )
        .await
    }

    /// Deletes this Mac's copy (FR-013): the question, or the whole thread when it
    /// is the thread's first question, since follow-ups mean nothing without it.
    ///
    /// - A question this Mac asked that is still open is canceled on the owner's
    ///   Mac first, best effort, so deleting it does not leave an agent answering.
    ///   An unreachable owner does not stop the delete.
    /// - A question asked of this Mac's agents is deleted with its thread's fork
    ///   transcript ([`AskPipeline::discard_thread`]). One still being answered is
    ///   a `conflict`, because the pipeline would store it again.
    ///
    /// Only this Mac's copy is deleted; whether an owner's delete also reaches the
    /// asker is still to be confirmed (backend architecture, "Storage and data
    /// ownership").
    pub async fn delete(&self, id: QuestionId) -> Result<()> {
        let me = self.me()?;
        let local = self.collab.question(id)?.ok_or_else(question_not_found)?;
        let owner = local.target.owner();
        let asked_of_me = owner == me && local.asker != me;
        if !asked_of_me && local.asker != me {
            return Err(question_not_found());
        }
        let whole_thread = local.id == local.thread_id;
        let doomed: Vec<Question> = if whole_thread {
            self.collab
                .questions()?
                .into_iter()
                .filter(|q| q.thread_id == local.thread_id)
                .collect()
        } else {
            vec![local.clone()]
        };

        if asked_of_me {
            let pipeline = self.owner_pipeline.as_ref().ok_or_else(|| {
                error(
                    ErrorCode::Internal,
                    "deleting a question asked of your agents is not available yet: \
                     formationd has not connected the ask pipeline",
                )
            })?;
            if doomed.iter().any(|q| being_answered(q.status)) {
                return Err(error(
                    ErrorCode::Conflict,
                    "the question is still being answered; try again when it is done",
                ));
            }
            if whole_thread {
                pipeline
                    .discard_thread(self.collab.as_ref(), local.thread_id)
                    .await?;
            }
        } else {
            for question in doomed.iter().filter(|q| is_open(q.status)) {
                // Best effort: the copy goes either way, and an update that
                // arrives later finds no copy and is dropped.
                let _ = self
                    .peers
                    .question_action(owner, question.id, PeerQuestionAction::Cancel)
                    .await;
            }
        }

        for question in doomed {
            match self.collab.delete_question(question.id) {
                Ok(()) => self.publish_removed(question.id),
                // Deleted meanwhile by a concurrent call.
                Err(failure) if failure.code == ErrorCode::NotFound => {}
                Err(failure) => return Err(failure),
            }
        }
        Ok(())
    }

    /// Applies a `PeerEvent::QuestionUpdate` from `from`, keeping the newest copy.
    /// Updates for questions this Mac did not ask, or asked of someone else, are
    /// ignored.
    ///
    /// [`Peers::subscribe`] does not name the sender, but the peer client forwards
    /// a question update only from the question's owner, so formationd passes
    /// `question.target.owner()` as `from`.
    pub fn apply_update(&self, from: MemberId, question: Question) -> Result<()> {
        let me = self.me()?;
        let _copies = self.lock_copies()?;
        match self.collab.question(question.id)? {
            Some(local) if local.asker == me && local.target.owner() == from && from != me => {
                self.accept(from, local, question).map(drop)
            }
            _ => Ok(()),
        }
    }

    /// `share_update`: stores the change, publishes `share.update` when something
    /// changed, and calls the [`ShareHook`]. Returns the change so a caller outside
    /// the router can act on it as the hook would.
    pub fn share_update(&self, update: &ShareUpdate) -> Result<ShareChange> {
        let change = sharing::update_share(self.agents.as_ref(), self.collab.as_ref(), update)?;
        if change.changed() {
            self.events.publish(Event::ShareUpdate(ShareUpdateEvent {
                share: change.after.clone(),
            }));
            if let Some(hook) = &self.share_hook {
                hook(&change);
            }
        }
        Ok(change)
    }

    /// `teammate_agents`: one teammate's shared agents, or every teammate's.
    ///
    /// - For one teammate, their Mac's answer, errors included (`peer_offline`
    ///   when it cannot be reached). Naming this Mac's own member returns this
    ///   Mac's shared agents as teammates see them.
    /// - For everyone, the agents of each teammate whose Mac answers, asked at
    ///   once; a teammate who cannot be reached or refuses is left out, so one
    ///   offline Mac does not hide the others. The agents an offline teammate
    ///   shared last come from presence (`peer.presence`), not from here.
    pub async fn teammate_agents(&self, member: Option<MemberId>) -> Result<Vec<SharedAgentView>> {
        let me = self.me()?;
        match member {
            Some(member) if member == me => Ok(sharing::visible_agents(
                me,
                &self.agents.agents()?,
                &self.collab.shares()?,
            )),
            Some(member) => {
                self.require_teammate(member)?;
                self.peers.agents(member).await
            }
            None => {
                let teammates: Vec<MemberId> = self
                    .collab
                    .members()?
                    .into_iter()
                    .map(|m| m.id)
                    .filter(|id| *id != me)
                    .collect();
                let answers = futures_util::future::join_all(
                    teammates.iter().map(|id| self.peers.agents(*id)),
                )
                .await;
                Ok(answers
                    .into_iter()
                    .flat_map(|a| a.unwrap_or_default())
                    .collect())
            }
        }
    }

    fn me(&self) -> Result<MemberId> {
        self.collab
            .me()?
            .ok_or_else(|| error(ErrorCode::NotMember, "this Mac has not joined a team"))
    }

    /// `not_found` unless `member` is on the team now.
    fn require_teammate(&self, member: MemberId) -> Result<()> {
        if self.collab.members()?.iter().any(|m| m.id == member) {
            Ok(())
        } else {
            Err(error(ErrorCode::NotFound, "no such teammate"))
        }
    }

    /// A question this Mac asked of a teammate. Questions asked of this Mac's
    /// agents are acted on through the owner's commands instead.
    fn asked(&self, id: QuestionId) -> Result<Question> {
        let me = self.me()?;
        let local = self.collab.question(id)?.ok_or_else(question_not_found)?;
        if local.asker != me {
            return Err(question_not_found());
        }
        if local.target.owner() == me {
            return Err(error(
                ErrorCode::InvalidParams,
                "this question was asked of your own agent",
            ));
        }
        Ok(local)
    }

    async fn act(&self, local: &Question, action: PeerQuestionAction) -> Result<Question> {
        let owner = local.target.owner();
        let remote = self.peers.question_action(owner, local.id, action).await?;
        let _copies = self.lock_copies()?;
        let current = self.local(local.id)?;
        self.accept(owner, current, remote)
    }

    fn local(&self, id: QuestionId) -> Result<Question> {
        self.collab.question(id)?.ok_or_else(question_not_found)
    }

    /// Settles a question the owner's Mac did not take, unless an owner update
    /// arrived meanwhile (the outbox may deliver it at once).
    fn settle_unsent(
        &self,
        id: QuestionId,
        status: QuestionStatus,
        failure: Option<ApiError>,
    ) -> Result<Question> {
        let _copies = self.lock_copies()?;
        let mut question = self.local(id)?;
        if question.status != QuestionStatus::Received {
            return Ok(question);
        }
        question.status = status;
        question.error = failure;
        question.updated_at = now().max(question.updated_at);
        self.collab.put_question(&question)?;
        self.publish(&question);
        Ok(question)
    }

    /// The chain and acting agent for a question asked from `from_run`, as for
    /// work requests.
    async fn chain(&self, from_run: Option<RunId>) -> Result<(Chain, Option<String>)> {
        let Some(run_id) = from_run else {
            return Ok((new_chain(0), None));
        };
        let run = self
            .harness
            .run(run_id)
            .await
            .ok_or_else(|| error(ErrorCode::NotFound, "no agent run has this ID"))?;
        let via_agent = self.agents.agent(run.agent_id)?.map(|agent| agent.slug);
        let chain = match run.item.source {
            QueueSource::Request { request_id, .. } => {
                let parent = self
                    .collab
                    .request(request_id)?
                    .ok_or_else(|| error(ErrorCode::NotFound, "no such request"))?;
                Chain {
                    id: parent.chain.id,
                    depth: parent.chain.depth.saturating_add(1),
                }
            }
            QueueSource::Owner | QueueSource::Schedule { .. } => new_chain(1),
        };
        Ok((chain, via_agent))
    }

    /// Stores the owner's copy unless this Mac already has a newer one. Call with
    /// the copies lock held.
    fn accept(&self, from: MemberId, local: Question, remote: Question) -> Result<Question> {
        if remote.id != local.id
            || remote.thread_id != local.thread_id
            || remote.asker != local.asker
            || remote.target.owner() != from
            || local.target.owner() != from
        {
            return Err(error(
                ErrorCode::Conflict,
                "the owner's copy does not match this question",
            ));
        }
        if !supersedes(&remote, &local) {
            return Ok(local);
        }
        self.collab.put_question(&remote)?;
        self.publish(&remote);
        Ok(remote)
    }

    fn publish(&self, question: &Question) {
        self.events
            .publish(Event::QuestionUpdate(QuestionUpdateEvent::Upsert {
                question: Box::new(question.clone()),
            }));
    }

    fn publish_removed(&self, question_id: QuestionId) {
        self.events
            .publish(Event::QuestionUpdate(QuestionUpdateEvent::Removed {
                question_id,
            }));
    }

    fn lock_copies(&self) -> Result<MutexGuard<'_, ()>> {
        self.copies
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "the question copies lock is poisoned"))
    }
}

#[async_trait]
impl CommandHandler for AskCommands {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            Ask::METHOD,
            QuestionCancel::METHOD,
            QuestionRefresh::METHOD,
            QuestionRouteChoose::METHOD,
            QuestionHandoff::METHOD,
            QuestionAnswer::METHOD,
            QuestionDelete::METHOD,
            ShareUpdate::METHOD,
            SharePreview::METHOD,
            TeammateAgents::METHOD,
        ]
    }

    /// Teammates reach questions through the peer endpoints, never through commands.
    async fn handle(&self, call: CommandCall) -> Result<serde_json::Value> {
        if call.caller != Caller::Owner {
            return Err(error(
                ErrorCode::Denied,
                "question and sharing commands are for this Mac's owner",
            ));
        }
        let method = call.method.as_str();
        if method == Ask::METHOD {
            respond(self.ask(params(&call)?).await)
        } else if method == QuestionCancel::METHOD {
            let QuestionCancel { question_id } = params(&call)?;
            respond(self.cancel(question_id).await)
        } else if method == QuestionRefresh::METHOD {
            let QuestionRefresh { question_id } = params(&call)?;
            respond(self.refresh(question_id).await)
        } else if method == QuestionRouteChoose::METHOD {
            respond(self.route_choose(params(&call)?).await)
        } else if method == QuestionHandoff::METHOD || method == QuestionAnswer::METHOD {
            // Decode anyway, so a malformed call still reads as `invalid_params`.
            if method == QuestionHandoff::METHOD {
                params::<QuestionHandoff>(&call)?;
            } else {
                params::<QuestionAnswer>(&call)?;
            }
            Err(error(
                ErrorCode::Internal,
                format!(
                    "{method} is not available yet: questions sent to you in person are \
                     served with the peer ask endpoints, which formationd has not connected"
                ),
            ))
        } else if method == QuestionDelete::METHOD {
            let QuestionDelete { question_id } = params(&call)?;
            respond(self.delete(question_id).await)
        } else if method == ShareUpdate::METHOD {
            let update: ShareUpdate = params(&call)?;
            respond(self.share_update(&update).map(|change| change.after))
        } else if method == SharePreview::METHOD {
            let SharePreview { agent_id } = params(&call)?;
            let me = self.me()?;
            respond(sharing::share_preview(
                me,
                self.agents.as_ref(),
                self.collab.as_ref(),
                agent_id,
            ))
        } else if method == TeammateAgents::METHOD {
            let TeammateAgents { member_id } = params(&call)?;
            respond(self.teammate_agents(member_id).await)
        } else {
            Err(error(
                ErrorCode::InvalidParams,
                format!("{method} is not a question or sharing command"),
            ))
        }
    }
}

/// Whether the owner's copy is newer than this Mac's. Questions move back and
/// forth (a refresh takes an answered question back to `answering`), so the
/// owner's `updated_at` orders the copies. This Mac's own `received` and
/// `waiting_offline` copies are stamped by this Mac's clock before the owner had
/// the question, so any copy from the owner replaces them. A canceled question
/// stays canceled.
fn supersedes(remote: &Question, local: &Question) -> bool {
    if remote == local || local.status == QuestionStatus::Canceled {
        return false;
    }
    matches!(
        local.status,
        QuestionStatus::Received | QuestionStatus::WaitingOffline
    ) || remote.updated_at >= local.updated_at
}

/// Not answered, canceled, or failed: canceling it still changes something.
fn is_open(status: QuestionStatus) -> bool {
    matches!(
        status,
        QuestionStatus::Received
            | QuestionStatus::Answering
            | QuestionStatus::Unsure
            | QuestionStatus::WaitingOffline
            | QuestionStatus::SentToOwner
    )
}

/// The owner's pipeline is working on it and will store it again.
fn being_answered(status: QuestionStatus) -> bool {
    matches!(status, QuestionStatus::Received | QuestionStatus::Answering)
}

fn check_chain_depth(depth: u8) -> Result<()> {
    if depth > MAX_CHAIN_DEPTH {
        Err(error(
            ErrorCode::RateLimited,
            format!("agent-to-agent chains stop at depth {MAX_CHAIN_DEPTH}"),
        ))
    } else {
        Ok(())
    }
}

fn require_text(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(error(
            ErrorCode::InvalidParams,
            format!("{field} must not be empty"),
        ))
    } else {
        Ok(())
    }
}

fn new_chain(depth: u8) -> Chain {
    Chain {
        id: ChainId::generate(),
        depth,
    }
}

fn now() -> Timestamp {
    SystemTime::now().into()
}

fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}

fn question_not_found() -> ApiError {
    error(ErrorCode::NotFound, "no such question")
}

/// Omitted or `null` params read as `{}`, as on the rest of the local socket.
fn params<C: DeserializeOwned>(call: &CommandCall) -> Result<C> {
    let params = match &call.params {
        serde_json::Value::Null => serde_json::json!({}),
        params => params.clone(),
    };
    serde_json::from_value(params)
        .map_err(|failure| error(ErrorCode::InvalidParams, failure.to_string()))
}

fn respond<T: Serialize>(result: Result<T>) -> Result<serde_json::Value> {
    serde_json::to_value(result?).map_err(|failure| error(ErrorCode::Internal, failure.to_string()))
}
