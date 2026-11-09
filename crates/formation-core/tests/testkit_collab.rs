//! Owner: B. Scenarios across B's fakes (TASK-042): A's request executor driven
//! through `FakeRequestQueue` and `FakePermissionGate`, C-style approval
//! simulation, and peer authentication with `stub-whois`. `cargo test --workspace`
//! runs it: the crate's dev-dependency on itself turns on `testkit`.
#![cfg(feature = "testkit")]

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::SystemTime;

use formation_core::ports::{
    ApprovalRepo, CollabStore, PermissionGate, Policy, RequestQueue, RunProgress,
};
use formation_core::testkit::collab::samples::{
    bash, member, permission_request, whois_of, work_request,
};
use formation_core::testkit::{
    FakePermissionGate, FakePolicy, FakeRequestQueue, MemoryCollabStore, StubWhoIs, WhoIsResolver,
};
use formation_proto::common::{AgentId, ApiError, ChainId, ErrorCode, MemberId, RunId, Timestamp};
use formation_proto::policy::{ApprovalChoice, ApprovalStatus, Caller, Chain, PermissionVerdict};
use formation_proto::request::{QueueSource, RequestStatus, WorkRequest};
use formation_proto::team::MemberRole;

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// What A's executor (TASK-024) does with one queued work request: take it, run
/// the turn, send its one tool call through the gate, and report the outcome.
async fn execute_next(queue: &dyn RequestQueue, gate: &dyn PermissionGate, agent_id: AgentId) {
    let item = queue.next(agent_id).await.expect("next item");
    queue
        .report(item.id, RunProgress::Started)
        .await
        .expect("started");
    let (caller, request_id) = match item.source {
        QueueSource::Request {
            request_id,
            requester,
        } => (
            Caller::Member {
                member_id: requester,
                via_agent: None,
                chain: Chain {
                    id: ChainId::generate(),
                    depth: 0,
                },
            },
            Some(request_id),
        ),
        _ => (Caller::Owner, None),
    };
    let prompt = permission_request(
        agent_id,
        RunId::generate(),
        caller,
        request_id,
        bash("npm test"),
    );
    let progress = match gate.decide(prompt).await {
        Ok(PermissionVerdict::Allow { .. }) => RunProgress::Completed {
            result: Some("npm test passed".into()),
        },
        Ok(PermissionVerdict::Deny { message }) => RunProgress::Failed {
            error: ApiError::new(ErrorCode::Denied, message),
        },
        Ok(PermissionVerdict::Defer) => unreachable!("the permission tool never gets defer"),
        Err(error) => RunProgress::Failed { error },
    };
    queue.report(item.id, progress).await.expect("finished");
}

#[test]
fn a_request_runs_after_allow_once_and_stops_on_deny_or_expiry() {
    let queue = Arc::new(FakeRequestQueue::new());
    let gate = Arc::new(FakePermissionGate::new());
    let (minsu, dohyeon, backend) = (
        MemberId::generate(),
        MemberId::generate(),
        AgentId::generate(),
    );
    let requests: Vec<WorkRequest> = ["allow", "deny", "expire"]
        .into_iter()
        .map(|key| work_request(minsu, dohyeon, backend, "Add token expiry tests", key))
        .collect();
    let mut updates = queue.subscribe_requests();

    block_on(async {
        for request in &requests {
            queue.enqueue_request(request.clone()).await.unwrap();
        }
        let executor = tokio::spawn({
            let (queue, gate) = (queue.clone(), gate.clone());
            async move {
                for _ in 0..3 {
                    execute_next(queue.as_ref(), gate.as_ref(), backend).await;
                }
            }
        });

        // The owner answers each approval in turn, as the approval sheet would.
        for (request, answer) in requests.iter().zip(["allow once", "deny", "expire"]) {
            let approval = gate.next_pending().await;
            assert_eq!(approval.request_id, Some(request.id));
            assert_eq!(approval.requester, Some(minsu));
            assert_eq!(approval.summary, "npm test");
            match answer {
                "allow once" => gate.decide_approval(approval.id, ApprovalChoice::AllowOnce),
                "deny" => gate.decide_approval(approval.id, ApprovalChoice::Deny),
                _ => gate.expire(approval.id),
            }
            .unwrap();
        }
        executor.await.unwrap();

        let finals: Vec<_> = requests
            .iter()
            .map(|request| queue.request(request.id).unwrap())
            .collect();
        assert_eq!(finals[0].status, RequestStatus::Completed);
        assert_eq!(finals[0].result.as_deref(), Some("npm test passed"));
        assert_eq!(finals[1].status, RequestStatus::Denied);
        assert_eq!(finals[2].status, RequestStatus::Expired);
        assert_eq!(
            finals[2].error.as_ref().map(|error| error.code),
            Some(ErrorCode::ApprovalExpired)
        );

        // The requester saw each request queued, then running, then final.
        let mut seen = vec![];
        while let Ok(update) = updates.try_recv() {
            seen.push((update.idempotency_key, update.status));
        }
        let timeline = |key: &str| -> Vec<RequestStatus> {
            seen.iter()
                .filter(|(k, _)| k == key)
                .map(|(_, status)| *status)
                .collect()
        };
        use RequestStatus::*;
        assert_eq!(timeline("allow"), [Queued, Running, Completed]);
        assert_eq!(timeline("deny"), [Queued, Running, Denied]);
        assert_eq!(timeline("expire"), [Queued, Running, Expired]);
    });
    assert!(queue.in_flight().is_empty());
    assert_eq!(gate.approvals().len(), 3);
}

#[test]
fn a_fake_service_simulates_approvals_from_events() {
    let gate = Arc::new(FakePermissionGate::new());
    let store = Arc::new(MemoryCollabStore::new());
    let agent = AgentId::generate();
    let mut events = gate.subscribe();

    block_on(async {
        // Two of the owner's own runs ask at once; reads pass without a card.
        let prompts = ["npm test", "rm -rf node_modules"].map(|command| {
            permission_request(agent, RunId::generate(), Caller::Owner, None, bash(command))
        });
        let waiting: Vec<_> = prompts
            .into_iter()
            .map(|prompt| {
                let gate = gate.clone();
                tokio::spawn(async move { gate.decide(prompt).await })
            })
            .collect();

        // A fake service mirrors approval events (two raised, two decided) into its
        // store and decides by command, as C's fake-formationd would script it.
        for _ in 0..4 {
            let approval = events.recv().await.unwrap().approval;
            store.put_approval(&approval).unwrap();
            if approval.status == ApprovalStatus::Pending {
                let choice = if approval.summary.starts_with("rm") {
                    ApprovalChoice::Deny
                } else {
                    ApprovalChoice::KeepAllowingSimilar
                };
                gate.decide_approval(approval.id, choice).unwrap();
                let twice = gate.decide_approval(approval.id, choice).unwrap_err();
                assert_eq!(twice.code, ErrorCode::Conflict);
            }
        }

        let verdicts: Vec<_> = join_all(waiting).await;
        assert!(matches!(verdicts[0], PermissionVerdict::Allow { .. }));
        assert!(matches!(verdicts[1], PermissionVerdict::Deny { .. }));
    });
    assert!(
        store
            .approvals(Some(ApprovalStatus::Pending))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .approvals(Some(ApprovalStatus::Allowed))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.approvals(Some(ApprovalStatus::Denied)).unwrap().len(),
        1
    );
}

async fn join_all(
    handles: Vec<tokio::task::JoinHandle<formation_core::ports::Result<PermissionVerdict>>>,
) -> Vec<PermissionVerdict> {
    let mut verdicts = vec![];
    for handle in handles {
        verdicts.push(handle.await.unwrap().unwrap());
    }
    verdicts
}

#[test]
fn a_peer_connection_is_authenticated_through_whois_and_the_roster() {
    let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
    let policy = Arc::new(FakePolicy::new());
    let minsu = member("minsu", MemberRole::Member);
    store.put_member(&minsu).unwrap();
    policy.add_member(&minsu.tailscale_login, minsu.id);

    let minsu_ip = IpAddr::V4(Ipv4Addr::new(100, 79, 59, 24));
    let ci_ip = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9));
    let whois = Arc::new(
        StubWhoIs::new()
            .with_user(minsu_ip, &minsu.tailscale_login, &minsu.tailscale_node)
            .with_tagged(ci_ip, "ci", &["tag:ci"]),
    );
    let resolver: Arc<dyn WhoIsResolver> = whois.clone();
    let port: Arc<dyn Policy> = policy.clone();

    // What the peer server does on each connection: WhoIs, then the roster.
    let authenticate = |addr: SocketAddr| {
        let (resolver, port, store) = (resolver.clone(), port.clone(), store.clone());
        async move {
            let whois = resolver.whois(addr).await?;
            let member = port.authenticate(&whois).await?;
            let stored = store.member_by_tailscale(&whois.login)?;
            assert_eq!(stored.map(|m| m.id), Some(member));
            Ok::<_, ApiError>(member)
        }
    };
    block_on(async {
        let from_minsu = SocketAddr::new(minsu_ip, 52011);
        assert_eq!(authenticate(from_minsu).await.unwrap(), minsu.id);
        assert_eq!(whois_of(&minsu).login, minsu.tailscale_login);

        let tagged = authenticate(SocketAddr::new(ci_ip, 1)).await.unwrap_err();
        assert_eq!(tagged.code, ErrorCode::NotMember);
        let outsider = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 194)), 7413);
        assert_eq!(
            authenticate(outsider).await.unwrap_err().code,
            ErrorCode::NotMember
        );

        // Removal ends access at once.
        policy.remove_member(minsu.id);
        store
            .remove_member(minsu.id, Timestamp::from(SystemTime::now()))
            .unwrap();
        assert_eq!(
            authenticate(from_minsu).await.unwrap_err().code,
            ErrorCode::NotMember
        );
    });
    assert_eq!(whois.calls().len(), 4);
}
