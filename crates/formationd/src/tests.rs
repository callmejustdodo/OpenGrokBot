use super::*;
use formation_core::service::connection;
use formation_proto::common::AgentId;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn fake_daemon_serves_snapshot_and_agents() {
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
            assert_eq!(snapshot["harnesses"][0]["version"], "fake");
        } else {
            assert_eq!(response["result"], json!([]));
        }
    }
    drop(client);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(service);
    std::fs::remove_dir_all(directory).unwrap();
}
