//! TASK-013: the stream-json codec against the recorded Claude Code fixtures. Runs offline.

use formation_core::harness::claude::protocol::{
    ApiEvent, ContentBlock, ContentDelta, Event, Launch, LaunchOptions, Priority, ResultEvent,
    SystemKind, UserContent, decode_stdout_line, encode_interrupt, encode_user_message,
    launch_args,
};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testkit/fixtures/claude")
}

fn fixture_files(suffix: &str) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = fs::read_dir(fixtures())
        .expect("read fixture folder")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| path.to_string_lossy().ends_with(suffix))
        .collect();
    assert!(!files.is_empty(), "no {suffix} fixtures");
    files
}

fn events(name: &str) -> Vec<Event> {
    fs::read_to_string(fixtures().join(format!("{name}.stdout.jsonl")))
        .expect("read fixture")
        .lines()
        .map(|line| decode_stdout_line(line).expect("valid JSON"))
        .collect()
}

fn results(events: &[Event]) -> Vec<&ResultEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Result(result) => Some(result),
            _ => None,
        })
        .collect()
}

#[test]
fn every_fixture_line_decodes_to_a_typed_event() {
    for path in fixture_files(".stdout.jsonl") {
        let body = fs::read_to_string(&path).expect("read fixture");
        assert!(!body.is_empty(), "{} is empty", path.display());
        for (index, line) in body.lines().enumerate() {
            let event = decode_stdout_line(line)
                .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), index + 1));
            assert!(
                !matches!(event, Event::Unknown(_)),
                "{}:{} is not typed: {line}",
                path.display(),
                index + 1
            );
        }
    }
}

#[test]
fn init_and_result_report_the_same_session() {
    let events = events("1-one-turn");
    let (init_session, cwd, model, mode) = events
        .iter()
        .find_map(|event| match event {
            Event::System(system) => match &system.kind {
                SystemKind::Init {
                    cwd,
                    model,
                    permission_mode,
                } => Some((&system.session_id, cwd, model, permission_mode)),
                _ => None,
            },
            _ => None,
        })
        .expect("an init event");
    assert_eq!(*init_session, results(&events)[0].session_id);
    assert!(!cwd.is_empty());
    assert_eq!(model, "claude-opus-5-5");
    assert_eq!(mode, "default");
}

#[test]
fn a_later_message_is_its_own_turn() {
    assert_eq!(results(&events("2-queue-later")).len(), 2);
}

#[test]
fn stop_is_acknowledged_and_ends_the_turn_without_text() {
    let events = events("4-stop-control-request");
    let response = events
        .iter()
        .find_map(|event| match event {
            Event::ControlResponse(control) => Some(&control.response),
            _ => None,
        })
        .expect("a control_response");
    assert_eq!(response.subtype, "success");
    assert_eq!(response.request_id, "stop-1");
    let data = response.response.as_ref().expect("response data");
    assert_eq!(data.still_queued, Some(vec![]));

    let stopped = results(&events)[0];
    assert_eq!(stopped.subtype, "error_during_execution");
    assert!(stopped.is_error);
    assert_eq!(stopped.result, None);
}

#[test]
fn a_steered_turn_reports_aborted_tools() {
    let events = events("3-steer-now-human");
    assert_eq!(
        results(&events)[0].terminal_reason.as_deref(),
        Some("aborted_tools")
    );
}

#[test]
fn replays_tool_results_and_partial_input_are_typed() {
    let events = events("1-one-turn");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::User(user) if user.is_replay && matches!(user.message.content, UserContent::Text(_))
    )));
    assert!(events.iter().any(|event| match event {
        Event::User(user) => match &user.message.content {
            UserContent::Blocks(blocks) => blocks.iter().any(|block| matches!(
                block,
                ContentBlock::ToolResult {
                    is_error: false,
                    ..
                }
            )),
            UserContent::Text(_) => false,
        },
        _ => false,
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Assistant(assistant) if assistant.message.content.iter().any(
            |block| matches!(block, ContentBlock::ToolUse { name, .. } if name == "Bash")
        )
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::StreamEvent(stream) if matches!(
            stream.event,
            ApiEvent::ContentBlockDelta { delta: ContentDelta::InputJson { .. }, .. }
        )
    )));
}

#[test]
fn unmodelled_lines_keep_their_raw_value() {
    for line in [
        r#"{"type":"something_new","a":1}"#,
        r#"{"type":"system","subtype":"something_new","session_id":"s"}"#,
        r#"{"type":"result","subtype":"success"}"#,
        r#"{"type":"assistant","session_id":"s","parent_tool_use_id":null,"message":{"id":"m","content":[{"type":"new_block"}]}}"#,
        r#"{"no_type":true}"#,
        "[1,2]",
    ] {
        let raw: Value = serde_json::from_str(line).expect("test line is JSON");
        assert_eq!(decode_stdout_line(line).unwrap(), Event::Unknown(raw));
    }
}

#[test]
fn invalid_json_is_an_error() {
    assert!(decode_stdout_line("{not json").is_err());
    assert!(decode_stdout_line("").is_err());
}

#[test]
fn encoders_reproduce_the_recorded_stdin() {
    let mut encoded = Vec::new();
    for path in fixture_files(".stdin.jsonl") {
        let body = fs::read_to_string(&path).expect("read fixture");
        for line in body.lines() {
            let recorded: Value = serde_json::from_str(line).expect("valid JSON");
            // The encoder has no `origin`; one experiment added it by hand.
            if recorded.get("origin").is_some() {
                continue;
            }
            let output = match recorded["type"].as_str() {
                Some("user") => {
                    let priority = match recorded["priority"].as_str() {
                        None => None,
                        Some("now") => Some(Priority::Now),
                        Some("next") => Some(Priority::Next),
                        Some("later") => Some(Priority::Later),
                        Some(other) => panic!("unexpected priority {other}"),
                    };
                    let text = recorded["message"]["content"].as_str().expect("text");
                    encode_user_message(text, priority)
                }
                Some("control_request") => {
                    encode_interrupt(recorded["request_id"].as_str().expect("request_id"))
                }
                other => panic!("unexpected stdin type {other:?}"),
            };
            assert!(!output.contains('\n'), "one line, no newline: {output}");
            let output: Value = serde_json::from_str(&output).expect("encoder writes JSON");
            assert_eq!(output, recorded);
            encoded.push(recorded);
        }
    }
    let priority_of = |wanted: &str| {
        encoded
            .iter()
            .any(|line| line["priority"].as_str() == Some(wanted))
    };
    assert!(priority_of("now"), "fixtures exercise priority now");
    assert!(priority_of("later"), "fixtures exercise priority later");
    assert!(
        encoded.iter().any(|line| line["type"] == "control_request"),
        "fixtures exercise the interrupt"
    );
}

#[test]
fn next_priority_is_written_like_the_others() {
    let line: Value = serde_json::from_str(&encode_user_message("hi", Some(Priority::Next)))
        .expect("encoder writes JSON");
    assert_eq!(
        line,
        json!({
            "type": "user",
            "message": {"role": "user", "content": "hi"},
            "parent_tool_use_id": null,
            "priority": "next",
        })
    );
}

const BASE_ARGS: [&str; 8] = [
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--replay-user-messages",
];

#[test]
fn a_fresh_session_uses_the_tech_spec_flags() {
    let args = launch_args(Launch::New { session_id: "abc" }, LaunchOptions::default());
    let mut expected = BASE_ARGS.to_vec();
    expected.extend(["--session-id", "abc"]);
    assert_eq!(args, expected);
}

#[test]
fn a_resume_passes_every_option_set() {
    let args = launch_args(
        Launch::Resume { session_id: "abc" },
        LaunchOptions {
            permission_prompt_tool: Some("mcp__formation__permission"),
            mcp_config: Some("/x/mcp.json"),
            settings: Some("/x/settings.json"),
            permission_mode: Some("default"),
        },
    );
    let mut expected = BASE_ARGS.to_vec();
    expected.extend([
        "--resume",
        "abc",
        "--permission-prompt-tool",
        "mcp__formation__permission",
        "--mcp-config",
        "/x/mcp.json",
        "--settings",
        "/x/settings.json",
        "--permission-mode",
        "default",
    ]);
    assert_eq!(args, expected);
}
