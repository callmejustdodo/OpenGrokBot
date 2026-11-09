//! The team on the peer server (issue #179): [`TeamJoinEndpoint`] serves
//! `POST /v1/team/join` from [`TeamService::handle_join`], and [`forward_roster`]
//! carries [`TeamService::subscribe_roster`] to the peer server's
//! [`PeerEventHub`] and to the peer client.
//!
//! formationd wires both the same way on every Mac: `handle_join` answers
//! `not_found` on a Mac that did not create the team, and [`forward_roster`]
//! decides per change whether this Mac pushes the roster.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::peer::{PeerEvent, PeerJoin, PeerJoinAccepted};
use formation_proto::team::WhoIs;

use super::service::TeamService;
use crate::peer::api::{JoinEndpoint, PeerEventHub};
use crate::ports::{Result, TeamRepo};

/// [`JoinEndpoint`] over the team on this Mac.
pub struct TeamJoinEndpoint<S: TeamRepo + Send + Sync + ?Sized> {
    team: Arc<TeamService<S>>,
}

impl<S: TeamRepo + Send + Sync + ?Sized> TeamJoinEndpoint<S> {
    pub fn new(team: Arc<TeamService<S>>) -> Self {
        Self { team }
    }
}

#[async_trait]
impl<S: TeamRepo + Send + Sync + ?Sized + 'static> JoinEndpoint for TeamJoinEndpoint<S> {
    /// `handle_join` is synchronous: it reads and writes the store (SQLite in
    /// formationd) under the team lock, so it runs on the blocking pool rather
    /// than stalling the peer server's runtime threads.
    async fn join(&self, whois: WhoIs, join: PeerJoin) -> Result<PeerJoinAccepted> {
        let team = self.team.clone();
        tokio::task::spawn_blocking(move || team.handle_join(Some(&whois), &join))
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("the join failed: {e}")))?
    }
}

/// Forwards every roster change made or applied on this Mac until the team
/// service is dropped.
///
/// - On the creator's Mac, each change goes to every member as
///   [`RosterChange::to_peer_event`](super::RosterChange::to_peer_event) through
///   [`PeerEventHub::publish_all`]; a roster push makes each events connection
///   check its member first, so a removed member's connection closes instead of
///   receiving it.
/// - On a member's Mac (the roster received on `team_join`, or one applied
///   from the creator's push or `GET /v1/team`), nothing is pushed, since teammates take the roster only
///   from the creator's Mac. [`PeerEventHub::recheck_members`] still closes the
///   connections of members who were removed, including every connection when
///   this Mac itself was removed.
///
/// After each change, `on_change` runs: formationd passes
/// [`PeerClient::sync_members`](crate::peer::client::PeerClient::sync_members),
/// so links to new teammates open and links to removed ones close. Joining a
/// team counts as a change, so a newly joined Mac connects without waiting for
/// the creator's push. Changes made before this call are not replayed:
/// formationd also runs `sync_members` once at startup for the roster it
/// already holds.
///
/// When the subscription falls behind, the missed changes are replaced by the
/// current roster: the push carries the whole roster with its version, so the
/// newest one is all members need.
pub fn forward_roster<S, F>(
    team: Arc<TeamService<S>>,
    hub: PeerEventHub,
    on_change: F,
) -> JoinHandle<()>
where
    S: TeamRepo + Send + Sync + ?Sized + 'static,
    F: Fn() + Send + 'static,
{
    // Subscribe now, not in the task, so no change made after this call is missed.
    let mut changes = team.subscribe_roster();
    let weak = Arc::downgrade(&team);
    drop(team);
    tokio::spawn(async move {
        loop {
            let change = match changes.recv().await {
                Ok(change) => Some(change),
                Err(RecvError::Lagged(_)) => None,
                Err(RecvError::Closed) => return,
            };
            // Hold the service only while handling a change, so dropping it
            // closes the channel and ends this task.
            let Some(team) = weak.upgrade() else { return };
            let creator = {
                let team = team.clone();
                tokio::task::spawn_blocking(move || team.is_creator())
                    .await
                    .ok()
                    .and_then(|result| result.ok())
                    .unwrap_or(false)
            };
            if creator {
                let event = match change {
                    Some(change) => Some(change.to_peer_event()),
                    None => current_roster(team.clone()).await,
                };
                match event {
                    Some(event) => hub.publish_all(event),
                    // The roster could not be read: closing removed members'
                    // connections is still owed.
                    None => hub.recheck_members(),
                }
            } else {
                hub.recheck_members();
            }
            drop(team);
            on_change();
        }
    })
}

/// The roster push for the roster as stored now, or `None` when it cannot be read.
async fn current_roster<S>(team: Arc<TeamService<S>>) -> Option<PeerEvent>
where
    S: TeamRepo + Send + Sync + ?Sized + 'static,
{
    let roster = tokio::task::spawn_blocking(move || team.roster())
        .await
        .ok()?
        .ok()?;
    Some(PeerEvent::Roster {
        members: roster.members,
        version: roster.version,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::sync::broadcast;

    use formation_proto::common::MemberId;
    use formation_proto::team::{MemberRole, Team};

    use super::*;
    use crate::peer::api::HubMessage;
    use crate::team::{LocalIdentity, TailnetSelf, TeamConfig, system_clock};
    use crate::testkit::RecordingEventSink;
    use crate::testkit::collab::{FakePeers, MemoryCollabStore};

    const WAIT: Duration = Duration::from_secs(5);

    struct Stub {
        whois: WhoIs,
        github_login: String,
    }

    impl LocalIdentity for Stub {
        fn tailnet_self(&self) -> Result<TailnetSelf> {
            Ok(TailnetSelf {
                whois: self.whois.clone(),
                host: "dohyeon-mbp.tail0000.ts.net".into(),
            })
        }

        fn github_login(&self) -> Result<String> {
            Ok(self.github_login.clone())
        }
    }

    fn whois(name: &str) -> WhoIs {
        WhoIs {
            login: format!("{name}@example.com"),
            node: format!("n{}", name.to_uppercase()),
        }
    }

    fn service(name: &str) -> Arc<TeamService<MemoryCollabStore>> {
        service_with(
            name,
            Arc::new(MemoryCollabStore::new()),
            Arc::new(FakePeers::new(MemberId::generate())),
        )
    }

    fn service_with(
        name: &str,
        store: Arc<MemoryCollabStore>,
        peers: Arc<FakePeers>,
    ) -> Arc<TeamService<MemoryCollabStore>> {
        Arc::new(TeamService::new(
            store,
            Arc::new(Stub {
                whois: whois(name),
                github_login: name.into(),
            }),
            peers,
            Arc::new(RecordingEventSink::default()),
            system_clock(),
            TeamConfig::default(),
        ))
    }

    /// Dohyeon's Mac with a team.
    fn creator() -> (Arc<TeamService<MemoryCollabStore>>, Team) {
        let team = service("dohyeon");
        let created = team.create(Some("Dohyeon")).unwrap();
        (team, created)
    }

    fn peer_join(code: &str, github: &str) -> PeerJoin {
        PeerJoin {
            code: code.into(),
            github_login: github.into(),
            display_name: github.into(),
        }
    }

    async fn next_message(rx: &mut broadcast::Receiver<HubMessage>) -> HubMessage {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("a hub message")
            .expect("hub open")
    }

    async fn wait_for(count: &AtomicUsize, at_least: usize) {
        tokio::time::timeout(WAIT, async {
            while count.load(Ordering::SeqCst) < at_least {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the callback ran");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_join_through_the_endpoint_records_the_callers_tailscale_identity() {
        let (team, created) = creator();
        let code = team.invite().unwrap().code;
        let endpoint: Arc<dyn JoinEndpoint> = Arc::new(TeamJoinEndpoint::new(team.clone()));

        let accepted = endpoint
            .join(whois("minsu"), peer_join(&code, "minsu"))
            .await
            .unwrap();

        assert_eq!(accepted.version, 2);
        assert!(accepted.team_id.is_some());
        let minsu = accepted
            .members
            .iter()
            .find(|m| m.id == accepted.member_id)
            .expect("the new member is listed");
        assert_eq!(minsu.github_login, "minsu");
        assert_eq!(minsu.tailscale_login, whois("minsu").login);
        assert_eq!(minsu.tailscale_node, whois("minsu").node);
        assert_eq!(minsu.role, MemberRole::Member);
        assert!(accepted.members.iter().any(|m| m.id == created.me));

        // The code is single-use; the creator's errors come through unchanged.
        let again = endpoint
            .join(whois("jiwoo"), peer_join(&code, "jiwoo"))
            .await
            .unwrap_err();
        assert_eq!(again.code, ErrorCode::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mac_that_did_not_create_the_team_refuses_joins() {
        let team = service("minsu");
        let endpoint = TeamJoinEndpoint::new(team);
        let refused = endpoint
            .join(whois("jiwoo"), peer_join("7F3K-Q9PX", "jiwoo"))
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_creators_roster_changes_reach_every_member_and_the_client() {
        let (team, _) = creator();
        let hub = PeerEventHub::new();
        let mut events = hub.subscribe();
        let synced = Arc::new(AtomicUsize::new(0));
        let counter = synced.clone();
        let _task = forward_roster(team.clone(), hub.clone(), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let code = team.invite().unwrap().code;
        let accepted = TeamJoinEndpoint::new(team.clone())
            .join(whois("minsu"), peer_join(&code, "minsu"))
            .await
            .unwrap();
        match next_message(&mut events).await {
            HubMessage::Event {
                audience: crate::peer::api::Audience::Members,
                event: PeerEvent::Roster { members, version },
            } => {
                assert_eq!(version, 2);
                assert!(members.iter().any(|m| m.id == accepted.member_id));
            }
            other => panic!("expected a roster push, got {other:?}"),
        }
        wait_for(&synced, 1).await;

        team.remove(accepted.member_id).unwrap();
        match next_message(&mut events).await {
            HubMessage::Event {
                event: PeerEvent::Roster { members, version },
                ..
            } => {
                assert_eq!(version, 3);
                assert!(!members.iter().any(|m| m.id == accepted.member_id));
            }
            other => panic!("expected a roster push, got {other:?}"),
        }
        wait_for(&synced, 2).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_members_mac_rechecks_connections_without_pushing_the_roster() {
        // Dohyeon creates the team and Minsu joins; Minsu's Mac then applies the
        // creator's roster that removes Jiwoo.
        let (creator, created) = creator();
        let code = creator.invite().unwrap().code;
        let minsu_id = creator
            .handle_join(Some(&whois("minsu")), &peer_join(&code, "minsu"))
            .unwrap()
            .member_id;
        let code = creator.invite().unwrap().code;
        let jiwoo_id = creator
            .handle_join(Some(&whois("jiwoo")), &peer_join(&code, "jiwoo"))
            .unwrap()
            .member_id;

        let store = Arc::new(MemoryCollabStore::new());
        for member in creator.roster().unwrap().members {
            store.put_member(&member).unwrap();
        }
        store.set_me(minsu_id).unwrap();
        store
            .put_team(Some(&crate::ports::TeamRecord {
                id: formation_proto::team::TeamId::generate(),
                creator_host: "dohyeon-mbp.tail0000.ts.net".into(),
                creator_port: 7413,
                roster_version: creator.roster().unwrap().version,
                open_invitation: None,
            }))
            .unwrap();
        let minsu = Arc::new(TeamService::new(
            store,
            Arc::new(Stub {
                whois: whois("minsu"),
                github_login: "minsu".into(),
            }),
            Arc::new(FakePeers::new(minsu_id)),
            Arc::new(RecordingEventSink::default()),
            system_clock(),
            TeamConfig::default(),
        ));

        let hub = PeerEventHub::new();
        let mut events = hub.subscribe();
        let synced = Arc::new(Mutex::new(0usize));
        let counter = synced.clone();
        let _task = forward_roster(minsu.clone(), hub, move || {
            *counter.lock().unwrap() += 1;
        });

        creator.remove(jiwoo_id).unwrap();
        let change = minsu
            .apply_roster(created.me, creator.roster().unwrap())
            .unwrap()
            .expect("a newer roster");
        assert_eq!(change.removed, vec![jiwoo_id]);

        match next_message(&mut events).await {
            HubMessage::Recheck => {}
            other => panic!("expected a member recheck, got {other:?}"),
        }
        tokio::time::timeout(WAIT, async {
            while *synced.lock().unwrap() < 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the callback ran");
        assert!(
            events.try_recv().is_err(),
            "a member's Mac pushes no roster"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn joining_a_team_runs_the_callback_so_the_new_member_connects() {
        // Minsu has no link to anyone before the join, so the creator's roster push
        // cannot reach him: the join itself has to start the peer client.
        let (creator, _) = creator();
        let invitation = creator.invite().unwrap();
        let link = crate::team::InvitationLink::parse(&invitation.link).unwrap();
        let minsu_id = MemberId::generate();
        let peers = Arc::new(FakePeers::new(minsu_id));
        peers.set_roster(creator.roster().unwrap().members);
        peers.expect_join(&link.address(), &invitation.code, whois("minsu"));
        let minsu = service_with("minsu", Arc::new(MemoryCollabStore::new()), peers);

        let hub = PeerEventHub::new();
        let mut events = hub.subscribe();
        let synced = Arc::new(AtomicUsize::new(0));
        let counter = synced.clone();
        let _task = forward_roster(minsu.clone(), hub, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let team = minsu
            .join(&formation_proto::team::TeamJoin {
                code: invitation.link.clone(),
                display_name: None,
            })
            .await
            .unwrap();
        assert_eq!(team.me, minsu_id);

        wait_for(&synced, 1).await;
        match next_message(&mut events).await {
            HubMessage::Recheck => {}
            other => panic!("expected a member recheck, got {other:?}"),
        }
    }

    /// On a current-thread runtime the forwarding task cannot run until the test
    /// awaits, so every change below is made before it reads the first one.
    #[tokio::test]
    async fn a_lagging_subscription_pushes_the_current_roster_first() {
        let (team, _) = creator();
        let hub = PeerEventHub::new();
        let mut events = hub.subscribe();
        let _task = forward_roster(team.clone(), hub, || {});

        // More joins than the roster channel holds (64).
        for i in 0..70 {
            let code = team.invite().unwrap().code;
            let name = format!("member{i}");
            team.handle_join(Some(&whois(&name)), &peer_join(&code, &name))
                .unwrap();
        }
        let latest = team.roster().unwrap();
        assert_eq!(latest.version, 71);

        match next_message(&mut events).await {
            HubMessage::Event {
                audience: crate::peer::api::Audience::Members,
                event: PeerEvent::Roster { members, version },
            } => {
                assert_eq!(version, latest.version);
                assert_eq!(members.len(), latest.members.len());
            }
            other => panic!("expected the current roster, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_a_member_closes_their_events_connection() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

        use crate::peer::api::{BlockingIdentity, PeerApi, PeerApiConfig, PeerApiDeps};
        use crate::policy::TeamPolicy;
        use crate::testkit::MemoryStore;

        let store = Arc::new(MemoryCollabStore::new());
        let team = service_with(
            "dohyeon",
            store.clone(),
            Arc::new(FakePeers::new(MemberId::generate())),
        );
        team.create(Some("Dohyeon")).unwrap();
        let code = team.invite().unwrap().code;
        let minsu = team
            .handle_join(Some(&whois("minsu")), &peer_join(&code, "minsu"))
            .unwrap()
            .member_id;

        // Every loopback connection is Minsu's Mac.
        let hub = PeerEventHub::new();
        let api = PeerApi::bind(
            PeerApiConfig::new("127.0.0.1:0".parse().unwrap()),
            PeerApiDeps {
                identity: Arc::new(BlockingIdentity::new(|_| Ok(whois("minsu")))),
                policy: Arc::new(TeamPolicy::new(
                    store.clone(),
                    Arc::new(MemoryStore::default()),
                )),
                team: store.clone(),
                join: None,
                ask: None,
                requests: None,
                hub: hub.clone(),
            },
        )
        .await
        .unwrap();
        let _task = forward_roster(team.clone(), hub.clone(), || {});

        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/v1/events", api.local_addr()))
                .await
                .expect("Minsu connects as a member");
        let hello = tokio::time::timeout(WAIT, socket.next())
            .await
            .expect("a hello")
            .expect("open")
            .expect("a message");
        assert!(matches!(hello, Message::Text(_)), "got {hello:?}");

        team.remove(minsu).unwrap();
        let closed = tokio::time::timeout(WAIT, async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Close(frame))) => return frame,
                    Some(Ok(Message::Text(text))) => {
                        panic!("a removed member received {text}")
                    }
                    Some(Ok(_)) => {}
                    other => panic!("expected a close frame, got {other:?}"),
                }
            }
        })
        .await
        .expect("the connection closed");
        assert_eq!(closed.expect("a close frame").code, CloseCode::Policy);
        api.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forwarding_ends_when_the_team_service_is_dropped() {
        let (team, _) = creator();
        let task = forward_roster(team.clone(), PeerEventHub::new(), || {});
        drop(team);
        tokio::time::timeout(WAIT, task)
            .await
            .expect("the task ended")
            .expect("the task did not panic");
    }
}
