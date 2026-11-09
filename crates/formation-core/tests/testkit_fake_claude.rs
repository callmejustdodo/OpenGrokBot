#![cfg(feature = "testkit")]

use formation_core::harness::claude::protocol::{self, Event, Launch, LaunchOptions};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

fn spawn(name: &str, delay: u64, resume: bool) -> Child {
    Command::new(env!("CARGO_BIN_EXE_fake-claude"))
        .args(protocol::launch_args(
            if resume {
                Launch::Resume {
                    session_id: "test-session",
                }
            } else {
                Launch::New {
                    session_id: "test-session",
                }
            },
            LaunchOptions {
                settings: Some("ignored settings"),
                ..Default::default()
            },
        ))
        .env(
            "FAKE_CLAUDE_TRANSCRIPT",
            format!(
                "{}/testkit/fixtures/claude/{name}.stdout.jsonl",
                env!("CARGO_MANIFEST_DIR")
            ),
        )
        .env("FAKE_CLAUDE_LINE_DELAY_MS", delay.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn send(child: &mut Child, line: String) {
    writeln!(child.stdin.as_mut().unwrap(), "{line}").unwrap();
    child.stdin.as_mut().unwrap().flush().unwrap();
}

fn finish(mut child: Child) -> Vec<Value> {
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            assert!(!matches!(
                protocol::decode_stdout_line(line).unwrap(),
                Event::Unknown(_)
            ));
            let value: Value = serde_json::from_str(line).unwrap();
            if let Some(id) = value.get("session_id") {
                assert_eq!(id, "test-session");
            }
            value
        })
        .collect()
}

#[test]
fn replays_one_turn_then_exits_on_eof() {
    let mut child = spawn("1-one-turn", 0, false);
    send(&mut child, protocol::encode_user_message("one", None));
    let lines = finish(child);
    assert_eq!(lines.len(), 33);
    assert_eq!(lines.last().unwrap()["type"], "result");
    assert_eq!(lines.last().unwrap()["result"], "ONE-DONE");
}

#[test]
fn queues_two_turns_and_ignores_exhausted_messages() {
    let mut child = spawn("2-queue-later", 2, true);
    for _ in 0..3 {
        send(&mut child, protocol::encode_user_message("queued", None));
    }
    let lines = finish(child);
    let results: Vec<_> = lines.iter().filter(|v| v["type"] == "result").collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["result"], "FIRST-DONE");
    assert_eq!(results[1]["result"], "SECOND-DONE");
}

#[test]
fn interrupt_acknowledges_then_ends_only_the_active_turn() {
    let mut child = spawn("2-queue-later", 30, false);
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            tx.send(line.unwrap()).unwrap();
        }
    });
    send(&mut child, protocol::encode_user_message("first", None));
    let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        protocol::decode_stdout_line(&first).unwrap(),
        Event::System(_)
    ));
    send(&mut child, protocol::encode_interrupt("stop-test"));
    let mut seen_response = false;
    loop {
        let line = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match protocol::decode_stdout_line(&line).unwrap() {
            Event::ControlResponse(response) => {
                assert_eq!(response.response.request_id, "stop-test");
                assert_eq!(response.response.subtype, "success");
                seen_response = true;
            }
            Event::Result(result) => {
                assert!(seen_response);
                assert_eq!(result.subtype, "error_during_execution");
                assert!(result.is_error);
                assert_eq!(result.result, None);
                assert_eq!(result.session_id, "test-session");
                break;
            }
            _ => {
                assert!(!seen_response, "result must follow acknowledgement");
            }
        }
    }
    send(&mut child, protocol::encode_user_message("second", None));
    drop(child.stdin.take());
    let remaining: Vec<_> = rx
        .iter()
        .map(|s| protocol::decode_stdout_line(&s).unwrap())
        .collect();
    assert!(
        matches!(remaining.last(), Some(Event::Result(r)) if r.result.as_deref() == Some("SECOND-DONE"))
    );
    assert!(child.wait().unwrap().success());
}

#[test]
fn idle_interrupt_only_acknowledges_and_empty_eof_succeeds() {
    let mut child = spawn("1-one-turn", 0, false);
    send(&mut child, protocol::encode_interrupt("idle"));
    let lines = finish(child);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["response"]["request_id"], "idle");
    assert!(finish(spawn("1-one-turn", 0, false)).is_empty());
}

#[test]
fn requires_a_session_id() {
    let output = Command::new(env!("CARGO_BIN_EXE_fake-claude"))
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn drops_lines_after_last_result() {
    let path = std::env::temp_dir().join(format!(
        "formation-fake-claude-{}.jsonl",
        std::process::id()
    ));
    let fixture = std::fs::read_to_string(format!(
        "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    std::fs::write(&path, format!("{fixture}\n{{\"type\":\"system\",\"subtype\":\"status\",\"status\":\"trailing\",\"session_id\":\"old\"}}\n").replace("\n\n", "\n")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_fake-claude"))
        .args(["--session-id", "test-session"])
        .env("FAKE_CLAUDE_TRANSCRIPT", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    send(&mut child, protocol::encode_user_message("first", None));
    send(
        &mut child,
        protocol::encode_user_message("no complete turn", None),
    );
    let lines = finish(child);
    std::fs::remove_file(path).unwrap();
    assert_eq!(lines.len(), 33);
    assert_eq!(lines.last().unwrap()["type"], "result");
}
