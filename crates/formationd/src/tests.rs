use super::*;
use formation_core::service::connection;
use formation_proto::common::AgentId;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn fake_daemon_serves_snapshot_and_agents() {
    let directory = std::env::temp_dir().join(format!("formationd-{}", AgentId::generate()));
    let service = wiring(&directory, "harness").unwrap();
    assert!(directory.join("formation.db").is_file());
    let (client, server) = UnixStream::pair().unwrap();
    let task = tokio::spawn(connection(
        server,
        service.router.clone(),
        service.snapshots.clone(),
        service.bus.clone(),
    ));
    let mut client = BufReader::new(client);
    for (id, method) in [(1, "snapshot"), (2, "agents_list")] {
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
            assert_eq!(snapshot["team"], Value::Null);
            for field in [
                "agents",
                "teammate_agents",
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
