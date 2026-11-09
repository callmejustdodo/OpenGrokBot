//! Two formationd instances in one process, each with its own data directory and
//! the fake harness, reach each other through formationd's own wiring: the real
//! `PeerApi` and `PeerClient` on `127.0.0.1`, with a fixed WhoIs and directory in
//! place of Tailscale. Every connection comes from `127.0.0.1`, and each side is
//! only ever called by the other, so each side's caller identity is the other
//! side's WhoIs.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use async_trait::async_trait;
use formation_core::ask::mask::MASK;
use formation_core::ports::{ForkResult, TurnResult};
use formation_core::team::TailnetSelf;
use formation_core::testkit::ScriptedAgent;
use formation_proto::agent::Agent;
use formation_proto::ask::QuestionStatus;
use formation_proto::common::{MemberId, SessionId, Timestamp};
use formation_proto::harness::{SessionInfo, TurnOutcome};
use formation_proto::peer::SharedAgentView;
use formation_proto::snapshot::Snapshot;
use formation_proto::team::{Invitation, Team, WhoIs};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::time::{Duration, Instant, sleep};

use super::*;

const WAIT: Duration = Duration::from_secs(10);
const OWNER_EMAIL: &str = "dohyeon@example.com";

/// Whoever calls this side's peer server.
struct OtherSide(WhoIs);

#[async_trait]
impl CallerIdentity for OtherSide {
    async fn identify(&self, _addr: SocketAddr) -> formation_core::ports::Result<WhoIs> {
        Ok(self.0.clone())
    }
}

/// This side on the "tailnet": reachable as `localhost`.
struct Myself {
    whois: WhoIs,
    github_login: String,
}

impl LocalIdentity for Myself {
    fn tailnet_self(&self) -> formation_core::ports::Result<TailnetSelf> {
        Ok(TailnetSelf {
            whois: self.whois.clone(),
            host: "localhost".into(),
        })
    }

    fn github_login(&self) -> formation_core::ports::Result<String> {
        Ok(self.github_login.clone())
    }
}

/// The roster from this side's team store; each member's peer server is found by
/// their node, as `TailnetDirectory` does, but on loopback ports.
struct Loopback {
    team: Arc<dyn TeamRepo + Send + Sync>,
    ports: HashMap<String, u16>,
}

impl PeerDirectory for Loopback {
    fn me(&self) -> Option<MemberId> {
        self.team.me().ok().flatten()
    }

    fn members(&self) -> Vec<Member> {
        self.team.members().unwrap_or_default()
    }

    fn address(&self, member: MemberId) -> Option<String> {
        let member = self.members().into_iter().find(|m| m.id == member)?;
        let port = self.ports.get(&member.tailscale_node)?;
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, *port)).to_string())
    }
}

struct Side {
    directory: PathBuf,
    service: Service,
    api: Option<PeerApi>,
    tasks: Vec<JoinHandle<()>>,
}

impl Side {
    /// Wires one instance and binds its peer server on `127.0.0.1:port`.
    async fn start(
        login: &str,
        port: u16,
        caller: WhoIs,
        ports: HashMap<String, u16>,
        owner_emails: &[String],
    ) -> Self {
        let directory = std::env::temp_dir().join(format!("formationd-loopback-{login}-{port}"));
        let _ = std::fs::remove_dir_all(&directory);
        let tailnet = Tailnet {
            local: Arc::new(Myself {
                whois: whois(login),
                github_login: login.into(),
            }),
            callers: Arc::new(OtherSide(caller)),
            directory: Box::new(move |team| Arc::new(Loopback { team, ports })),
            peer_port: port,
        };
        let mut service = wiring(
            &directory,
            "harness",
            DetectPaths {
                claude_config: directory.join("claude"),
                codex_sessions: directory.join("codex"),
                hermes: vec![],
                openclaw: vec![],
            },
            tailnet,
            owner_emails,
        )
        .unwrap();
        let tasks = start_peers(&service).unwrap();
        let server = service.peer_server.take().expect("the real peer server");
        let api = PeerApi::bind(
            PeerApiConfig::new(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
            server.deps(),
        )
        .await
        .unwrap();
        Self {
            directory,
            service,
            api: Some(api),
            tasks,
        }
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> T {
        let result = self
            .service
            .router
            .call(method, params)
            .await
            .unwrap_or_else(|error| panic!("{method}: {error:?}"));
        serde_json::from_value(result).unwrap()
    }

    async fn snapshot(&self) -> Snapshot {
        self.service
            .snapshots
            .capture(&self.service.bus)
            .await
            .unwrap()
            .0
    }

    /// Polls the snapshot until `found` returns something.
    async fn wait_for<T>(&self, what: &str, found: impl Fn(&Snapshot) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let snapshot = self.snapshot().await;
            if let Some(value) = found(&snapshot) {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn stop(mut self) {
        if let Some(api) = self.api.take() {
            api.shutdown().await.unwrap();
        }
        for task in &self.tasks {
            task.abort();
        }
        if let Some(client) = &self.service.peer_client {
            client.shutdown();
        }
        drop(self.service);
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn whois(login: &str) -> WhoIs {
    WhoIs {
        login: format!("{login}@github"),
        node: format!("n{login}"),
    }
}

/// Two free loopback ports. They are released before the peer servers bind them,
/// because each port must be known to the wiring (invitation links carry it).
fn free_ports() -> (u16, u16) {
    let first = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let second = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    (
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port(),
    )
}

/// The agent's main session and one fork that answers, naming the owner's email.
fn scripted(agent: &Agent, directory: &std::path::Path) -> ScriptedAgent {
    let at = Timestamp::from(std::time::SystemTime::now());
    let session = |id: &str| SessionInfo {
        agent_id: agent.id,
        session_id: SessionId(id.into()),
        transcript_path: directory.join(format!("{id}.jsonl")),
        cwd: agent.folder_path.clone(),
        started_at: at,
        last_activity_at: Some(at),
    };
    let mut scripted = ScriptedAgent::new(session("main"), []);
    scripted.forks.push_back(ForkResult {
        session: session("fork-1"),
        result: TurnResult {
            outcome: TurnOutcome::Completed,
            result: Some(format!(
                "Yes, the login API merged this morning. Ask {OWNER_EMAIL} for details."
            )),
        },
    });
    scripted
}

#[tokio::test(flavor = "multi_thread")]
async fn two_daemons_join_share_ask_and_request_over_loopback() {
    let (creator_port, member_port) = free_ports();
    let ports = HashMap::from([
        (whois("dohyeon").node, creator_port),
        (whois("minsu").node, member_port),
    ]);
    let creator = Side::start(
        "dohyeon",
        creator_port,
        whois("minsu"),
        ports.clone(),
        &[OWNER_EMAIL.to_owned()],
    )
    .await;
    let member = Side::start("minsu", member_port, whois("dohyeon"), ports, &[]).await;

    // (a) The creator invites; the member joins with the link; both list both.
    let team: Team = creator
        .call("team_create", json!({"display_name": "Dohyeon"}))
        .await;
    let creator_id = team.me;
    let invitation: Invitation = creator.call("team_invite", json!({})).await;
    let joined: Team = member
        .call(
            "team_join",
            json!({"code": invitation.link, "display_name": "Minsu"}),
        )
        .await;
    let member_id = joined.me;
    assert_ne!(member_id, creator_id);
    for (side, name) in [(&creator, "creator"), (&member, "member")] {
        side.wait_for(&format!("the {name}'s roster to list both"), |s| {
            let ids: Vec<_> = s.team.as_ref()?.members.iter().map(|m| m.id).collect();
            (ids.contains(&creator_id) && ids.contains(&member_id)).then_some(())
        })
        .await;
    }

    // (b) The creator shares an agent; the member's `teammate_agents` lists it.
    let agent: Agent = creator
        .call(
            "agent_create",
            json!({
                "name": "API",
                "role": "API developer",
                "harness": "claude_code",
                "avatar": {"shape": "circle", "color": "blue"},
                "folder": null,
                "repositories": [],
                "instructions": null,
            }),
        )
        .await;
    creator
        .service
        .fake_harness
        .as_ref()
        .expect("the fake harness")
        .add_agent(scripted(&agent, &creator.directory));
    let _: Value = creator
        .call(
            "share_update",
            json!({
                "agent_id": agent.id,
                "shared": true,
                "visibility": "answer",
                "accepts_requests": true,
            }),
        )
        .await;
    let deadline = Instant::now() + WAIT;
    loop {
        let agents: Vec<SharedAgentView> = member
            .call("teammate_agents", json!({"member_id": creator_id}))
            .await;
        if agents
            .iter()
            .any(|view| view.agent_id == agent.id && view.owner == creator_id)
        {
            break;
        }
        assert!(Instant::now() < deadline, "the shared agent never showed");
        sleep(Duration::from_millis(50)).await;
    }

    // (c) The member asks that agent; the answer comes back, the owner's email masked.
    let asked: formation_proto::ask::Question = member
        .call(
            "ask",
            json!({
                "target": {"kind": "agent", "member_id": creator_id, "agent_id": agent.id},
                "text": "Did the login API merge?",
                "thread_id": null,
                "from_run": null,
            }),
        )
        .await;
    let answer = member
        .wait_for("the answer", |s| {
            s.questions
                .iter()
                .find(|q| q.id == asked.id && q.status == QuestionStatus::Answered)
                .and_then(|q| q.answer.clone())
        })
        .await;
    assert!(answer.text.contains("merged this morning"), "{answer:?}");
    assert!(!answer.text.contains(OWNER_EMAIL), "{answer:?}");
    assert!(answer.text.contains(MASK), "{answer:?}");

    // (d) The member's work request reaches the creator, and its status comes back:
    // turning requests off on the creator's Mac cancels it, and the update reaches
    // the member's own copy over the events connection.
    let request: formation_proto::request::WorkRequest = member
        .call(
            "request_create",
            json!({
                "owner": creator_id,
                "agent_id": agent.id,
                "prompt": "Add a token expiry check.",
                "idempotency_key": "loopback-1",
                "from_question": null,
                "from_run": null,
            }),
        )
        .await;
    assert_eq!(request.owner, creator_id);
    assert_eq!(request.requester, member_id);
    creator
        .wait_for("the request on the creator's Mac", |s| {
            s.requests
                .iter()
                .any(|r| r.id == request.id && r.requester == member_id)
                .then_some(())
        })
        .await;
    let _: Value = creator
        .call(
            "share_update",
            json!({
                "agent_id": agent.id,
                "shared": null,
                "visibility": null,
                "accepts_requests": false,
            }),
        )
        .await;
    member
        .wait_for("the canceled status on the member's Mac", |s| {
            s.requests
                .iter()
                .any(|r| r.id == request.id && r.status == RequestStatus::Canceled)
                .then_some(())
        })
        .await;

    member.stop().await;
    creator.stop().await;
}
