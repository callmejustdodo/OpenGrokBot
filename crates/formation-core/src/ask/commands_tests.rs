//! Tests for [`super::commands::AskCommands`]: questions asked of a teammate
//! through `FakePeers`, this Mac's copies in `MemoryCollabStore`, sharing, and the
//! router.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::broadcast;

use formation_proto::agent::{HarnessKind, HarnessStatus};
use formation_proto::ask::{
    Ask, AskTarget, Question, QuestionStatus, QuestionUpdateEvent, RouteChoice, RoutingCandidate,
};
use formation_proto::common::{ApiError, ErrorCode, MemberId, QuestionId, RunId, Timestamp};
use formation_proto::events::Event;
use formation_proto::peer::{PeerEvent, PeerQuestionAction, SharedAgentView};
use formation_proto::policy::{Caller, ShareUpdate, Visibility};
use formation_proto::team::{Member, MemberRole};

use super::commands::{AskCommands, ShareHook};
use crate::harness::detect::{Detect, DetectPaths};
use crate::policy::sharing::ShareChange;
use crate::policy::sharing::test_support::agent;
use crate::ports::{
    AgentRepo, CommandCall, CommandHandler, Peers, QuestionRepo, ShareRepo, Store, TeamRepo,
};
use crate::requests::{RequestClient, Requests};
use crate::service::Router;
use crate::testkit::samples::{member, member_caller, shared_agent};
use crate::testkit::{
    AskReply, FakeHarness, FakePeers, FakePolicy, MemoryCollabStore, MemoryStore, PeersCall,
    RecordingEventSink,
};

struct Mac {
    me: Member,
    teammate: Member,
    api: SharedAgentView,
    collab: Arc<MemoryCollabStore>,
    peers: Arc<FakePeers>,
    harness: Arc<FakeHarness>,
    agents: Arc<MemoryStore>,
    events: Arc<RecordingEventSink>,
    commands: AskCommands,
    updates: broadcast::Receiver<PeerEvent>,
}

impl Mac {
    /// Feeds the teammate's published question updates to `apply_update`, as
    /// formationd does from `Peers::subscribe`.
    fn pump(&mut self) {
        while let Ok(event) = self.updates.try_recv() {
            if let PeerEvent::QuestionUpdate { question } = event {
                self.commands
                    .apply_update(question.target.owner(), *question)
                    .unwrap();
            }
        }
    }

    fn copy(&self, id: QuestionId) -> Option<Question> {
        self.collab.question(id).unwrap()
    }

    fn ask_api(&self, text: &str) -> Ask {
        Ask {
            target: AskTarget::Agent {
                member_id: self.teammate.id,
                agent_id: self.api.agent_id,
            },
            text: text.into(),
            thread_id: None,
            from_run: None,
        }
    }

    fn ask_person(&self, text: &str) -> Ask {
        Ask {
            target: AskTarget::Person {
                member_id: self.teammate.id,
            },
            text: text.into(),
            thread_id: None,
            from_run: None,
        }
    }

    fn question_events(&self) -> Vec<QuestionUpdateEvent> {
        self.events
            .events()
            .into_iter()
            .filter_map(|(_, event)| match event {
                Event::QuestionUpdate(update) => Some(update),
                _ => None,
            })
            .collect()
    }

    fn peer_calls(&self) -> usize {
        self.peers.calls().len()
    }
}

fn harness() -> Arc<FakeHarness> {
    Arc::new(FakeHarness::new(
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
    ))
}

fn mac() -> Mac {
    let me = member("baekdusan", MemberRole::Member);
    let teammate = member("dohyeon", MemberRole::Creator);
    let collab = Arc::new(MemoryCollabStore::new());
    collab.set_me(me.id).unwrap();
    collab.put_member(&me).unwrap();
    collab.put_member(&teammate).unwrap();
    let api = shared_agent(teammate.id, "api", false);
    let peers = Arc::new(FakePeers::new(me.id));
    peers.add_teammate(teammate.id, vec![api.clone()]);
    let updates = peers.subscribe();
    let harness = harness();
    let agents = Arc::new(MemoryStore::default());
    let events = Arc::new(RecordingEventSink::default());
    let commands = AskCommands::new(
        collab.clone(),
        peers.clone(),
        harness.clone(),
        agents.clone(),
        events.clone(),
    );
    Mac {
        me,
        teammate,
        api,
        collab,
        peers,
        harness,
        agents,
        events,
        commands,
        updates,
    }
}

fn call(method: &str, params: Value) -> CommandCall {
    CommandCall {
        caller: Caller::Owner,
        method: method.into(),
        params,
    }
}

#[tokio::test]
async fn a_question_is_kept_here_and_follows_the_owners_answer() {
    let mut mac = mac();
    let asked = mac
        .commands
        .ask(mac.ask_api("Is login done?"))
        .await
        .unwrap();
    assert_eq!(asked.status, QuestionStatus::Received);
    assert_eq!(asked.asker, mac.me.id);
    assert_eq!(asked.thread_id, asked.id);
    assert_eq!(asked.agent_id, Some(mac.api.agent_id));
    assert_eq!(mac.peers.questions(mac.teammate.id)[0].id, asked.id);

    mac.pump();
    mac.peers
        .answer(asked.id, mac.api.agent_id, "Merged this morning.")
        .unwrap();
    mac.pump();
    let copy = mac.copy(asked.id).unwrap();
    assert_eq!(copy.status, QuestionStatus::Answered);
    assert_eq!(copy.answer.unwrap().text, "Merged this morning.");
    match mac.question_events().last() {
        Some(QuestionUpdateEvent::Upsert { question }) => {
            assert_eq!(question.status, QuestionStatus::Answered)
        }
        other => panic!("expected an upsert, got {other:?}"),
    }
}

#[tokio::test]
async fn an_offline_teammate_gets_the_question_when_back() {
    let mut mac = mac();
    mac.peers.set_online(mac.teammate.id, false).unwrap();
    let asked = mac.commands.ask(mac.ask_api("Status?")).await.unwrap();
    assert_eq!(asked.status, QuestionStatus::WaitingOffline);
    assert_eq!(asked.error, None);
    assert_eq!(mac.peers.outbox().len(), 1);
    assert_eq!(mac.copy(asked.id).unwrap(), asked);

    let delivered = mac.peers.set_online(mac.teammate.id, true).unwrap();
    assert!(delivered[0].1.is_ok());
    mac.pump();
    assert_eq!(mac.copy(asked.id).unwrap().status, QuestionStatus::Received);
}

#[tokio::test]
async fn a_refused_question_is_kept_as_failed() {
    let mac = mac();
    let refusal = ApiError::new(ErrorCode::AgentNotShared, "not shared");
    mac.peers
        .fail_next(mac.teammate.id, refusal.clone())
        .unwrap();
    let err = mac.commands.ask(mac.ask_api("Status?")).await.unwrap_err();
    assert_eq!(err, refusal);
    let copy = &mac.collab.questions().unwrap()[0];
    assert_eq!(copy.status, QuestionStatus::Failed);
    assert_eq!(copy.error.as_ref(), Some(&refusal));
}

#[tokio::test]
async fn questions_go_only_to_teammates_and_continue_only_my_threads() {
    let mac = mac();
    let mine = Ask {
        target: AskTarget::Person {
            member_id: mac.me.id,
        },
        ..mac.ask_api("Status?")
    };
    let err = mac.commands.ask(mine).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);

    let stranger = Ask {
        target: AskTarget::Person {
            member_id: MemberId::generate(),
        },
        ..mac.ask_api("Status?")
    };
    assert_eq!(
        mac.commands.ask(stranger).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    let blank = mac.ask_api("   ");
    assert_eq!(
        mac.commands.ask(blank).await.unwrap_err().code,
        ErrorCode::InvalidParams
    );
    let unknown_thread = Ask {
        thread_id: Some(QuestionId::generate()),
        ..mac.ask_api("And then?")
    };
    assert_eq!(
        mac.commands.ask(unknown_thread).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    let unknown_run = Ask {
        from_run: Some(RunId::generate()),
        ..mac.ask_api("Status?")
    };
    assert_eq!(
        mac.commands.ask(unknown_run).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    // Nothing was sent or stored.
    assert_eq!(mac.peer_calls(), 0);
    assert!(mac.collab.questions().unwrap().is_empty());
}

#[tokio::test]
async fn an_unsure_route_is_chosen_on_the_owners_mac() {
    let mut mac = mac();
    let candidate = RoutingCandidate {
        agent_id: mac.api.agent_id,
        name: "api".into(),
        role: "API developer".into(),
        score: 0.5,
        reason: "Worked on login".into(),
    };
    mac.peers
        .script_ask(mac.teammate.id, AskReply::Unsure(vec![candidate]))
        .unwrap();
    let asked = mac
        .commands
        .ask(mac.ask_person("Who owns login?"))
        .await
        .unwrap();
    assert_eq!(asked.agent_id, None);
    mac.pump();
    assert_eq!(mac.copy(asked.id).unwrap().status, QuestionStatus::Unsure);

    let routed = mac
        .commands
        .handle(call(
            "question_route_choose",
            json!({
                "question_id": asked.id,
                "choice": { "kind": "agent", "agent_id": mac.api.agent_id },
            }),
        ))
        .await
        .unwrap();
    let routed: Question = serde_json::from_value(routed).unwrap();
    assert_eq!(routed.status, QuestionStatus::Answering);
    assert_eq!(routed.agent_id, Some(mac.api.agent_id));
    assert_eq!(mac.copy(asked.id).unwrap(), routed);

    // The owner's own event for the same change finds the copy already current.
    let before = mac.question_events().len();
    mac.pump();
    assert_eq!(mac.question_events().len(), before);

    let to_owner = mac
        .commands
        .route_choose(formation_proto::ask::QuestionRouteChoose {
            question_id: asked.id,
            choice: RouteChoice::Owner,
        })
        .await
        .unwrap();
    assert_eq!(to_owner.status, QuestionStatus::SentToOwner);
}

#[tokio::test]
async fn refresh_and_cancel_act_on_the_owners_mac() {
    let mut mac = mac();
    let asked = mac.commands.ask(mac.ask_api("Status?")).await.unwrap();
    mac.peers
        .answer(asked.id, mac.api.agent_id, "Done.")
        .unwrap();
    mac.pump();
    let refreshed = mac.commands.refresh(asked.id).await.unwrap();
    assert_eq!(refreshed.status, QuestionStatus::Answering);

    let canceled = mac.commands.cancel(asked.id).await.unwrap();
    assert_eq!(canceled.status, QuestionStatus::Canceled);
    let calls = mac.peer_calls();
    // Canceling again returns the copy without asking the owner's Mac.
    assert_eq!(mac.commands.cancel(asked.id).await.unwrap(), canceled);
    assert_eq!(mac.peer_calls(), calls);
    // A canceled copy stays canceled whatever arrives later.
    let mut late = canceled.clone();
    late.status = QuestionStatus::Answered;
    mac.commands.apply_update(mac.teammate.id, late).unwrap();
    assert_eq!(mac.copy(asked.id).unwrap(), canceled);

    let err = mac
        .commands
        .refresh(QuestionId::generate())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn updates_from_the_wrong_member_or_older_copies_are_ignored() {
    let mut mac = mac();
    let asked = mac.commands.ask(mac.ask_api("Status?")).await.unwrap();
    mac.peers
        .answer(asked.id, mac.api.agent_id, "Done.")
        .unwrap();
    mac.pump();
    let answered = mac.copy(asked.id).unwrap();

    let mut stale = answered.clone();
    stale.status = QuestionStatus::Answering;
    stale.updated_at = Timestamp::default();
    mac.commands.apply_update(mac.teammate.id, stale).unwrap();
    assert_eq!(mac.copy(asked.id).unwrap(), answered);

    let mut forged = answered.clone();
    forged.status = QuestionStatus::Failed;
    mac.commands
        .apply_update(MemberId::generate(), forged.clone())
        .unwrap();
    assert_eq!(mac.copy(asked.id).unwrap(), answered);

    // A question this Mac never asked is not stored.
    let mut other = answered.clone();
    other.id = QuestionId::generate();
    other.thread_id = other.id;
    mac.commands
        .apply_update(mac.teammate.id, other.clone())
        .unwrap();
    assert!(mac.copy(other.id).is_none());

    // The owner's copy must be of the same question.
    let mut moved = answered.clone();
    moved.thread_id = QuestionId::generate();
    let err = mac
        .commands
        .apply_update(mac.teammate.id, moved)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn deleting_the_first_question_deletes_the_thread() {
    let mut mac = mac();
    let first = mac.commands.ask(mac.ask_api("Status?")).await.unwrap();
    mac.peers
        .answer(first.id, mac.api.agent_id, "Done.")
        .unwrap();
    mac.pump();
    let follow_up = mac
        .commands
        .ask(Ask {
            thread_id: Some(first.id),
            ..mac.ask_api("Which PR?")
        })
        .await
        .unwrap();
    assert_eq!(follow_up.thread_id, first.id);
    let other = mac
        .commands
        .ask(mac.ask_api("Anything else?"))
        .await
        .unwrap();

    mac.commands
        .handle(call("question_delete", json!({ "question_id": first.id })))
        .await
        .unwrap();
    assert!(mac.copy(first.id).is_none());
    assert!(mac.copy(follow_up.id).is_none());
    assert!(mac.copy(other.id).is_some());
    let removed: Vec<QuestionId> = mac
        .question_events()
        .into_iter()
        .filter_map(|event| match event {
            QuestionUpdateEvent::Removed { question_id } => Some(question_id),
            _ => None,
        })
        .collect();
    assert_eq!(removed.len(), 2);
    assert!(removed.contains(&first.id) && removed.contains(&follow_up.id));
    // The open follow-up was canceled on the owner's Mac first; the answered
    // question was not.
    let cancels: Vec<QuestionId> = mac
        .peers
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            PeersCall::QuestionAction {
                question_id,
                action: PeerQuestionAction::Cancel,
                ..
            } => Some(question_id),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, vec![follow_up.id]);
}

#[tokio::test]
async fn deleting_a_question_asked_of_my_agents_needs_the_pipeline() {
    let mac = mac();
    let id = QuestionId::generate();
    let theirs = Question {
        id,
        thread_id: id,
        asker: mac.teammate.id,
        target: AskTarget::Person {
            member_id: mac.me.id,
        },
        text: "Status?".into(),
        status: QuestionStatus::Answered,
        candidates: vec![],
        agent_id: None,
        answer: None,
        error: None,
        created_at: Timestamp::default(),
        updated_at: Timestamp::default(),
    };
    mac.collab.put_question(&theirs).unwrap();
    let err = mac.commands.delete(id).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(mac.copy(id).is_some());
    // Nor can the asker's commands act on it from here.
    assert_eq!(
        mac.commands.cancel(id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn share_update_publishes_and_reports_what_stopped() {
    let mac = mac();
    let api = agent("api", "API developer");
    mac.agents.insert_agent(&api).unwrap();
    let seen: Arc<Mutex<Vec<ShareChange>>> = Arc::default();
    let hook: ShareHook = {
        let seen = seen.clone();
        Arc::new(move |change: &ShareChange| seen.lock().unwrap().push(change.clone()))
    };
    let commands = AskCommands::new(
        mac.collab.clone(),
        mac.peers.clone(),
        mac.harness.clone(),
        mac.agents.clone(),
        mac.events.clone(),
    )
    .with_share_hook(hook);

    let shared = commands
        .handle(call(
            "share_update",
            json!({ "agent_id": api.id, "shared": true, "accepts_requests": true }),
        ))
        .await
        .unwrap();
    assert_eq!(shared["shared"], true);
    assert_eq!(shared["accepts_requests"], true);
    assert!(mac.collab.share(api.id).unwrap().shared);

    let off = ShareUpdate {
        agent_id: api.id,
        shared: Some(false),
        visibility: None,
        accepts_requests: None,
    };
    let change = commands.share_update(&off).unwrap();
    assert!(change.unshared() && change.requests_stopped());
    // No change: no event and no hook.
    assert!(!commands.share_update(&off).unwrap().changed());

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen[1].unshared());
    let shares: Vec<bool> = mac
        .events
        .events()
        .into_iter()
        .filter_map(|(_, event)| match event {
            Event::ShareUpdate(update) => Some(update.share.shared),
            _ => None,
        })
        .collect();
    assert_eq!(shares, vec![true, false]);

    let preview = commands
        .handle(call("share_preview", json!({ "agent_id": api.id })))
        .await
        .unwrap();
    assert_eq!(preview["view"]["owner"], json!(mac.me.id));
    assert_eq!(preview["visibility"], json!(Visibility::AnswerExcerpts));
    let err = commands
        .handle(call("share_update", json!({ "agent_id": "api" })))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

#[tokio::test]
async fn teammate_agents_lists_who_answers() {
    let mac = mac();
    let away = member("minsu", MemberRole::Member);
    mac.collab.put_member(&away).unwrap();
    mac.peers
        .add_teammate(away.id, vec![shared_agent(away.id, "web", true)]);
    mac.peers.set_online(away.id, false).unwrap();

    let one: Vec<SharedAgentView> = serde_json::from_value(
        mac.commands
            .handle(call(
                "teammate_agents",
                json!({ "member_id": mac.teammate.id }),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(one, vec![mac.api.clone()]);

    // Everyone: the offline teammate is left out instead of failing the list.
    let all = mac.commands.teammate_agents(None).await.unwrap();
    assert_eq!(all, vec![mac.api.clone()]);
    assert_eq!(
        mac.commands
            .teammate_agents(Some(away.id))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PeerOffline
    );
    // Omitted params mean everyone.
    let all = mac
        .commands
        .handle(call("teammate_agents", Value::Null))
        .await
        .unwrap();
    assert_eq!(all.as_array().map(Vec::len), Some(1));

    // My own member: my shared agents as teammates see them.
    let mine = agent("ops", "Operations");
    mac.agents.insert_agent(&mine).unwrap();
    assert!(
        mac.commands
            .teammate_agents(Some(mac.me.id))
            .await
            .unwrap()
            .is_empty()
    );
    mac.commands
        .share_update(&ShareUpdate {
            agent_id: mine.id,
            shared: Some(true),
            visibility: None,
            accepts_requests: None,
        })
        .unwrap();
    let mine_seen = mac.commands.teammate_agents(Some(mac.me.id)).await.unwrap();
    assert_eq!(mine_seen.len(), 1);
    assert_eq!(mine_seen[0].owner, mac.me.id);
    assert!(mac.agents.agent(mine.id).unwrap().is_some());
}

#[tokio::test]
async fn teammate_agents_drops_views_a_teammate_claims_for_someone_else() {
    let mac = mac();
    let third = member("minsu", MemberRole::Member);
    mac.collab.put_member(&third).unwrap();
    mac.peers.add_teammate(third.id, vec![]);
    // The teammate's Mac credits an agent it made up to the third member.
    let forged = shared_agent(third.id, "forged", false);
    mac.peers
        .set_agents(mac.teammate.id, vec![mac.api.clone(), forged])
        .unwrap();

    let one = mac
        .commands
        .teammate_agents(Some(mac.teammate.id))
        .await
        .unwrap();
    assert_eq!(one, vec![mac.api.clone()]);
    let all = mac.commands.teammate_agents(None).await.unwrap();
    assert_eq!(all, vec![mac.api.clone()]);
}

/// The best-effort cancel makes the owner publish a `canceled` update while the
/// delete runs. Whichever lands first, the copy stays deleted and `Removed` is
/// the last event for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_update_during_a_delete_does_not_bring_the_question_back() {
    for _ in 0..50 {
        let mac = mac();
        let commands = Arc::new(AskCommands::new(
            mac.collab.clone(),
            mac.peers.clone(),
            mac.harness.clone(),
            mac.agents.clone(),
            mac.events.clone(),
        ));
        let asked = commands.ask(mac.ask_api("Status?")).await.unwrap();
        let mut canceled = mac.copy(asked.id).unwrap();
        canceled.status = QuestionStatus::Canceled;
        canceled.updated_at = Timestamp::from(std::time::SystemTime::now());

        let update = {
            let commands = commands.clone();
            let from = mac.teammate.id;
            tokio::task::spawn_blocking(move || commands.apply_update(from, canceled))
        };
        commands.delete(asked.id).await.unwrap();
        update.await.unwrap().unwrap();

        assert!(mac.copy(asked.id).is_none());
        let last = mac
            .question_events()
            .into_iter()
            .rfind(|event| match event {
                QuestionUpdateEvent::Upsert { question } => question.id == asked.id,
                QuestionUpdateEvent::Removed { question_id } => *question_id == asked.id,
            });
        assert!(
            matches!(last, Some(QuestionUpdateEvent::Removed { .. })),
            "{last:?}"
        );
    }
}

#[tokio::test]
async fn only_the_owner_calls_and_the_in_person_commands_are_left_out() {
    let methods = mac().commands.methods();
    // Left for the owner-side handler, so it can register them beside this one.
    assert!(!methods.contains(&"question_handoff"));
    assert!(!methods.contains(&"question_answer"));

    let mac = mac();
    let mut teammate_call = call("ask", json!({}));
    teammate_call.caller = member_caller(mac.teammate.id);
    assert_eq!(
        mac.commands.handle(teammate_call).await.unwrap_err().code,
        ErrorCode::Denied
    );

    let err = mac
        .commands
        .handle(call("request_create", json!({})))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

/// Methods of the handlers formationd registers that are too heavy to build
/// here (`Agents`, `Schedules`, the Claude Code and Codex supervisors,
/// `PermissionCommands`, `TeamService`), copied from their `methods()`.
struct OtherHandlers;

#[async_trait::async_trait]
impl CommandHandler for OtherHandlers {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            // Agents
            "agents_list",
            "agent_create",
            "agent_update",
            "agent_files_read",
            "agent_files_write",
            "harness_check",
            // Schedules
            "schedule_list",
            "schedule_create",
            "schedule_update",
            "schedule_delete",
            "schedule_run_list",
            // ClaudeSupervisor and CodexSupervisor, through the dispatcher
            "agent_start",
            "agent_stop",
            "agent_message_send",
            "agent_message_steer",
            "agent_message_edit",
            "agent_message_withdraw",
            "agent_turn_interrupt",
            "agent_request_stop",
            "conversation_page",
            // PermissionCommands
            "permission_decide",
            "approval_decide",
            // TeamService
            "team_create",
            "team_invite",
            "team_join",
            "team_remove",
            // The owner's in-person question commands, for the handler that
            // will serve them beside AskCommands
            "question_handoff",
            "question_answer",
        ]
    }

    async fn handle(&self, _: CommandCall) -> crate::ports::Result<Value> {
        unreachable!("only the method lists are checked")
    }
}

#[test]
fn the_router_takes_these_methods_beside_the_other_handlers() {
    let mac = mac();
    let policy = Arc::new(FakePolicy::new());
    let owner_side = Arc::new(Requests::new(
        mac.collab.clone(),
        policy,
        mac.events.clone(),
    ));
    let requests = RequestClient::new(
        mac.collab.clone(),
        mac.peers.clone(),
        mac.harness.clone(),
        mac.agents.clone(),
        mac.events.clone(),
        owner_side,
    );
    let store: Arc<dyn Store> = mac.agents.clone();
    let detect = Detect::new(
        DetectPaths {
            claude_config: PathBuf::from("/nonexistent/claude"),
            codex_sessions: PathBuf::from("/nonexistent/codex"),
            hermes: vec![],
            openclaw: vec![],
        },
        store,
        mac.events.clone(),
    );
    let methods = mac.commands.methods();
    assert_eq!(methods.len(), 8);
    let router = Router::new(vec![
        Arc::new(mac.commands),
        Arc::new(requests),
        Arc::new(detect),
        Arc::new(OtherHandlers),
    ]);
    assert!(router.is_ok(), "{:?}", router.err());
}
