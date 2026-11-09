use super::*;
use formation_core::service::connection;
use formation_proto::common::{AgentId, ApprovalId, MemberId, QuestionId, RequestId, RunId};
use formation_proto::policy::{
    ApprovalChoice, ApprovalDecide, PermissionDecide, PermissionPrompt, PermissionSource, ToolCall,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn fake_daemon_serves_snapshot_and_agents() {
    check_wiring("", false).await;
    check_wiring("collab", false).await;
    check_wiring("harness", false).await;
    check_wiring("harness,peers", true).await;
    check_wiring("harness,requests", false).await;
}

/// A Tailscale with no CLI and no LocalAPI, so tests never reach this Mac's own.
fn no_tailnet() -> Arc<Tailscale> {
    Arc::new(Tailscale::with_discovery(Via::LocalApi, None, None, || {
        None
    }))
}

async fn check_wiring(fakes: &str, fake_peers: bool) {
    let directory = std::env::temp_dir().join(format!("formationd-{}", AgentId::generate()));
    let service = wiring(
        &directory,
        fakes,
        DetectPaths {
            claude_config: directory.join("claude"),
            codex_sessions: directory.join("codex"),
            hermes: vec![],
            openclaw: vec![],
        },
        no_tailnet(),
    )
    .unwrap();
    assert!(directory.join("formation.db").is_file());
    // The real peer client and server are the default; fake peers replace both.
    assert_eq!(service.peer_client.is_some(), !fake_peers);
    assert_eq!(service.peer_server.is_some(), !fake_peers);
    let peer_tasks = start_peers(&service).unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let task = tokio::spawn(connection(
        server,
        service.router.clone(),
        service.snapshots.clone(),
        service.bus.clone(),
    ));
    let mut client = BufReader::new(client);
    for (id, method) in [
        (1, "snapshot"),
        (2, "agents_list"),
        (3, "agents_detect"),
        (4, "schedule_list"),
        (9, "history_list"),
    ] {
        let params = if method == "history_list" {
            json!({"agent_id": null, "member_id": null, "before": null, "limit": 50})
        } else {
            json!({})
        };
        let mut request = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap();
        request.push(b'\n');
        client.get_mut().write_all(&request).await.unwrap();
        let mut line = String::new();
        timeout(Duration::from_secs(5), client.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert!(response.get("error").is_none(), "{response}");
        if method == "snapshot" {
            let snapshot = &response["result"];
            if fake_peers {
                assert_eq!(snapshot["team"]["members"].as_array().unwrap().len(), 2);
                assert_eq!(snapshot["teammate_agents"][0]["slug"], "fake-agent");
            } else {
                assert_eq!(snapshot["team"], Value::Null);
                assert_eq!(snapshot["teammate_agents"], json!([]));
            }
            for field in [
                "agents",
                "questions",
                "requests",
                "approvals",
                "rules",
                "shares",
                "inbox",
            ] {
                assert_eq!(snapshot[field], json!([]), "{field}");
            }
            assert_eq!(snapshot["harnesses"][0]["harness"], "claude_code");
            assert_eq!(snapshot["harnesses"][1]["harness"], "codex");
            assert_eq!(snapshot["harnesses"].as_array().unwrap().len(), 2);
            if fakes.contains("harness") {
                assert_eq!(snapshot["harnesses"][0]["version"], "fake");
            }
        } else {
            assert_eq!(response["result"], json!([]));
        }
    }
    let prompt = PermissionPrompt {
        agent_id: AgentId::generate(),
        run_id: RunId::generate(),
        session_id: None,
        source: PermissionSource::PermissionTool,
        call: ToolCall {
            tool_use_id: Some("tool".into()),
            tool_name: "Bash".into(),
            input: json!({"command": "true"}),
        },
    };
    let decide = ApprovalDecide {
        approval_id: ApprovalId::generate(),
        choice: ApprovalChoice::AllowOnce,
        rule: None,
    };
    for (id, method, params) in [
        (5, "permission_decide", json!(PermissionDecide { prompt })),
        (6, "approval_decide", json!(decide)),
        (7, "team_invite", json!({})),
        (
            8,
            "request_get",
            json!({"request_id": RequestId::generate()}),
        ),
        (10, "history_delete", json!({"ids": []})),
        (
            11,
            "ask",
            json!({
                "target": {"kind": "person", "member_id": MemberId::generate()},
                "text": "what changed?",
                "thread_id": null,
                "from_run": null,
            }),
        ),
        (
            12,
            "share_update",
            json!({
                "agent_id": AgentId::generate(),
                "shared": true,
                "visibility": null,
                "accepts_requests": null,
            }),
        ),
        (13, "teammate_agents", json!({"member_id": null})),
        (
            14,
            "question_handoff",
            json!({"question_id": QuestionId::generate(), "agent_id": AgentId::generate()}),
        ),
        (
            15,
            "question_answer",
            json!({"question_id": QuestionId::generate(), "text": "it was me"}),
        ),
    ] {
        let mut request = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap();
        request.push(b'\n');
        client.get_mut().write_all(&request).await.unwrap();
        let mut line = String::new();
        timeout(Duration::from_secs(5), client.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert_ne!(response["error"]["code"], -32601, "{method}: {response}");
        if method == "team_invite" {
            // No team is stored: the team service refuses before asking Tailscale.
            assert!(response["error"]["message"].is_string(), "{response}");
        }
        if method == "permission_decide" {
            // An unknown run is denied, not left to Claude Code.
            assert_eq!(response["result"]["behavior"], "deny", "{response}");
        }
        if method == "question_handoff" || method == "question_answer" {
            // Served by the owner's asks: an unknown question is not found.
            assert_eq!(response["error"]["data"]["code"], "not_found", "{response}");
        }
        if method == "teammate_agents" {
            if fake_peers {
                assert_eq!(response["result"][0]["slug"], "fake-agent", "{response}");
            } else {
                // No team: refused by the ask commands, not an unknown method.
                assert!(response["error"]["message"].is_string(), "{response}");
            }
        }
    }
    drop(client);
    for task in peer_tasks {
        task.abort();
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(service);
    std::fs::remove_dir_all(directory).unwrap();
}

/// On a member's Mac, the creator's roster push reaches the team service, which
/// announces the change that makes `forward_roster` sync the peer client.
#[tokio::test(flavor = "multi_thread")]
async fn a_roster_push_from_the_creator_updates_a_members_team() {
    let directory = std::env::temp_dir().join(format!("formationd-{}", AgentId::generate()));
    let service = wiring(
        &directory,
        "harness,peers",
        DetectPaths {
            claude_config: directory.join("claude"),
            codex_sessions: directory.join("codex"),
            hermes: vec![],
            openclaw: vec![],
        },
        no_tailnet(),
    )
    .unwrap();

    let creator = samples::member("dohyeon", MemberRole::Creator);
    let me = samples::member("minsu", MemberRole::Member);
    let newcomer = samples::member("jiwoo", MemberRole::Member);
    let store = Arc::new(MemoryCollabStore::new());
    store.put_member(&creator).unwrap();
    store.put_member(&me).unwrap();
    store.set_me(me.id).unwrap();
    store
        .put_team(Some(&formation_core::ports::TeamRecord {
            id: formation_proto::team::TeamId::generate(),
            creator_host: "dohyeon-mbp.tail0000.ts.net".into(),
            creator_port: PEER_PORT,
            roster_version: 1,
            open_invitation: None,
        }))
        .unwrap();
    let peers = Arc::new(FakePeers::new(me.id));
    let team: Arc<TeamService<dyn CollabStore>> = Arc::new(TeamService::new(
        store,
        Arc::new(TailnetIdentity),
        peers.clone(),
        Arc::new(EventBus::new(16)),
        system_clock(),
        TeamConfig::default(),
    ));
    let mut changes = team.subscribe_roster();
    let bridge = forward_peer_events(
        peers.subscribe(),
        service.ask_commands.clone(),
        service.request_client.clone(),
        team.clone(),
    );

    // The fake pushes version 1 (already held, ignored), then version 2.
    peers.set_roster(vec![creator.clone(), me.clone()]);
    peers.set_roster(vec![creator, me, newcomer.clone()]);

    let change = timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("a roster change")
        .unwrap();
    assert_eq!(change.version, 2);
    let members = team.team().unwrap().expect("still in the team").members;
    assert!(members.iter().any(|m| m.id == newcomer.id), "{members:?}");

    bridge.abort();
    drop(service);
    std::fs::remove_dir_all(directory).unwrap();
}
