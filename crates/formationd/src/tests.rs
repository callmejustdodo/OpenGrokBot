use super::*;
use formation_core::service::connection;
use formation_proto::common::{AgentId, ApprovalId, RequestId, RunId};
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
    )
    .unwrap();
    assert!(directory.join("formation.db").is_file());
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
    ] {
        let mut request = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}}),
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
    }
    drop(client);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(service);
    std::fs::remove_dir_all(directory).unwrap();
}
