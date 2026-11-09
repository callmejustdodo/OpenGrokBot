//! TASK-004: proves how the real `claude` behaves in stream-json mode.
//!
//! Every test is ignored because it needs a logged-in `claude`. Run with:
//! `FORMATION_CLAUDE_BIN=$HOME/.local/bin/claude cargo test -p formation-core --test claude_spike -- --ignored --nocapture`
//! Each run re-records `testkit/fixtures/claude/`.

use formation_core::harness::claude::spike::{
    Launch, Session, find_transcript, fresh_folder, is_bash_call, is_init, is_result, is_user_text,
    result_text,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

const STEP: Duration = Duration::from_secs(180);
const SLEEP_RULE: &[&str] = &["Bash(sleep:*)"];
const LONG_TURN: &str = "Use the Bash tool in the foreground (no run_in_background) to run exactly `sleep 20`. After it finishes, reply with exactly: FIRST-DONE";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testkit/fixtures/claude")
}

fn session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Sends `prompt` and returns once its foreground Bash call is really executing.
fn run_until_tool_is_executing(session: &mut Session, prompt: &str) -> usize {
    session.send_user(prompt, json!({}));
    let call = session
        .wait_for(0, STEP, is_bash_call)
        .expect("the turn calls Bash");
    assert_ne!(
        session.events[call]["message"]["content"][0]["input"]["run_in_background"],
        json!(true),
        "the experiment needs a foreground call"
    );
    session
        .wait_for(call, STEP, |event| event["subtype"] == "task_started")
        .expect("the Bash call starts executing")
}

fn start_long_turn() -> (Session, usize, String, PathBuf) {
    let folder = fresh_folder();
    let id = session_id();
    let mut session = Session::start(&folder, Launch::New(&id), SLEEP_RULE);
    let running = run_until_tool_is_executing(&mut session, LONG_TURN);
    (session, running, id, folder)
}

fn count(session: &Session, matches: impl Fn(&Value) -> bool) -> usize {
    session.events.iter().filter(|event| matches(event)).count()
}

fn next_result(session: &mut Session, from: usize) -> usize {
    session
        .wait_for(from, STEP, is_result)
        .expect("a result event")
}

fn finish(mut session: Session, name: &str) {
    session.close_stdin();
    let status = session.wait_exit(Duration::from_secs(30));
    session.save(&fixtures(), name);
    println!("{name}: exit {status:?}");
}

const RECALL: &str = "Which shell command did I ask you to run? Reply with only the command.";

fn result_of(event: &Value) -> String {
    format!("{} {:?}", event["subtype"], result_text(event))
}

#[test]
#[ignore = "needs the real claude binary"]
fn one_message_runs_one_turn() {
    let folder = fresh_folder();
    let id = session_id();
    let mut session = Session::start(&folder, Launch::New(&id), SLEEP_RULE);
    session.send_user(
        "Use the Bash tool to run exactly `sleep 1`. After it finishes, reply with exactly: ONE-DONE",
        json!({}),
    );
    let end = next_result(&mut session, 0);
    let init = session
        .events
        .iter()
        .find(|event| is_init(event))
        .expect("init")
        .clone();
    println!(
        "version {} model {} capabilities {}",
        init["claude_code_version"], init["model"], init["capabilities"]
    );
    println!("result: {}", result_of(&session.events[end]));
    assert_eq!(session.events[end]["subtype"], "success");
    assert!(
        session
            .events
            .iter()
            .any(|event| event["type"] == "stream_event")
    );
    assert!(session.events.iter().any(is_bash_call));
    let replay = session
        .wait_for(0, STEP, |event| is_user_text(event, "ONE-DONE"))
        .expect("the message is replayed");
    assert_eq!(session.events[replay]["isReplay"], true);
    let first_assistant = session
        .wait_for(0, STEP, |event| event["type"] == "assistant")
        .expect("an assistant event");
    assert!(replay < first_assistant, "replay precedes the reply");
    finish(session, "1-one-turn");
}

#[test]
#[ignore = "needs the real claude binary"]
fn message_during_a_tool_call_default_priority() {
    let (mut session, running, _, _) = start_long_turn();
    session.send_user("Also reply with exactly: SECOND-DONE", json!({}));
    let first = next_result(&mut session, running);
    println!("first result: {}", result_of(&session.events[first]));
    session.drain(Duration::from_secs(15));
    let replay = session
        .wait_for(0, STEP, |event| {
            is_user_text(event, "SECOND-DONE") && event["isReplay"] == true
        })
        .expect("the message is replayed");
    assert!(replay < first, "delivered inside the running turn");
    assert_eq!(count(&session, is_result), 1, "no separate turn");
    assert!(result_of(&session.events[first]).contains("SECOND-DONE"));
    finish(session, "2-queue-default");
}

#[test]
#[ignore = "needs the real claude binary"]
fn message_during_a_tool_call_priority_later() {
    let (mut session, running, _, _) = start_long_turn();
    session.send_user(
        "Reply with exactly: SECOND-DONE",
        json!({"priority": "later"}),
    );
    let first = next_result(&mut session, running);
    println!("first result: {}", result_of(&session.events[first]));
    let replay = session.wait_for(0, STEP, |event| is_user_text(event, "SECOND-DONE"));
    println!("replay index: {replay:?}, first result index: {first}");
    let second = next_result(&mut session, first + 1);
    println!("second result: {}", result_of(&session.events[second]));
    assert!(
        replay.expect("the message is replayed") > first,
        "held until the turn ends"
    );
    assert!(!result_of(&session.events[first]).contains("SECOND-DONE"));
    assert!(result_of(&session.events[second]).contains("SECOND-DONE"));
    finish(session, "2-queue-later");
}

const TWO_STEP_TURN: &str = "Use the Bash tool in the foreground (no run_in_background) to run exactly `sleep 12`. When it has finished, use the Bash tool the same way to run exactly `sleep 13`. After that, reply with exactly: ORIGINAL-PLAN";
const STEER: &str = "Change of plan: do not run any more commands. Reply with exactly: STEERED";

/// Returns the session after the steer so each test can assert what it proves.
fn steer(name: &str, extra: Value) -> (Session, usize) {
    let folder = fresh_folder();
    let id = session_id();
    let mut session = Session::start(&folder, Launch::New(&id), SLEEP_RULE);
    let running = run_until_tool_is_executing(&mut session, TWO_STEP_TURN);
    session.send_user(STEER, extra);
    let first = next_result(&mut session, running);
    println!("{name} first result: {}", result_of(&session.events[first]));
    session.drain(Duration::from_secs(30));
    let bash_calls = session
        .events
        .iter()
        .filter(|event| is_bash_call(event))
        .count();
    let results = session
        .events
        .iter()
        .filter(|event| is_result(event))
        .count();
    println!("{name}: bash calls {bash_calls}, results {results}");
    (session, first)
}

#[test]
#[ignore = "needs the real claude binary"]
fn steer_default_priority() {
    let name = "3-steer-default";
    let (session, first) = steer(name, json!({}));
    assert_eq!(
        count(&session, is_bash_call),
        1,
        "the second command was dropped"
    );
    assert_eq!(count(&session, is_result), 1, "steered inside one turn");
    assert!(result_of(&session.events[first]).contains("STEERED"));
    finish(session, name);
}

#[test]
#[ignore = "needs the real claude binary"]
fn steer_priority_now_human() {
    let name = "3-steer-now-human";
    let (session, _) = steer(
        name,
        json!({"priority": "now", "origin": {"kind": "human"}}),
    );
    assert_eq!(
        count(&session, is_bash_call),
        1,
        "the second command was dropped"
    );
    assert!(
        session
            .events
            .iter()
            .any(|event| result_of(event).contains("STEERED"))
    );
    finish(session, name);
}

#[test]
#[ignore = "needs the real claude binary"]
fn steer_priority_now_without_origin() {
    let name = "3-steer-now-no-origin";
    let (session, _) = steer(name, json!({"priority": "now"}));
    assert_eq!(
        count(&session, is_bash_call),
        1,
        "the second command was dropped"
    );
    assert!(
        session
            .events
            .iter()
            .any(|event| result_of(event).contains("STEERED"))
    );
    finish(session, name);
}

#[test]
#[ignore = "needs the real claude binary"]
fn stop_with_control_request() {
    let (mut session, running, id, _) = start_long_turn();
    session.send(json!({
        "type": "control_request",
        "request_id": "stop-1",
        "request": {"subtype": "interrupt"},
    }));
    let response = session
        .wait_for(running, STEP, |event| event["type"] == "control_response")
        .expect("the interrupt is acknowledged");
    println!("control_response: {}", session.events[response]);
    assert_eq!(session.events[response]["response"]["subtype"], "success");
    assert_eq!(session.events[response]["response"]["request_id"], "stop-1");
    let stopped = next_result(&mut session, running);
    println!("stopped result: {}", result_of(&session.events[stopped]));
    assert_eq!(session.events[stopped]["subtype"], "error_during_execution");
    assert!(session.is_running(), "the process survives the interrupt");
    session.send_user(RECALL, json!({}));
    let after = next_result(&mut session, stopped + 1);
    println!("after stop: {}", result_of(&session.events[after]));
    assert_eq!(session.events[after]["session_id"], id.as_str());
    assert!(result_of(&session.events[after]).contains("sleep 20"));
    finish(session, "4-stop-control-request");
}

#[test]
#[ignore = "needs the real claude binary"]
fn stop_with_sigint_then_resume() {
    let (mut session, running, id, folder) = start_long_turn();
    session.sigint();
    let stopped = session
        .wait_for(running, Duration::from_secs(30), is_result)
        .expect("SIGINT ends the turn with a result");
    println!(
        "result after SIGINT: {}",
        result_of(&session.events[stopped])
    );
    assert_eq!(session.events[stopped]["subtype"], "error_during_execution");
    let status = session.wait_exit(Duration::from_secs(15));
    println!("exit after SIGINT: {status:?}");
    assert!(
        status.is_some(),
        "SIGINT ends the process, not only the turn"
    );
    session.save(&fixtures(), "4-stop-sigint");

    let mut resumed = Session::start(&folder, Launch::Resume(&id), SLEEP_RULE);
    resumed.send_user(RECALL, json!({}));
    let end = next_result(&mut resumed, 0);
    println!("resumed: {}", result_of(&resumed.events[end]));
    assert_eq!(resumed.events[end]["session_id"], id.as_str());
    assert!(result_of(&resumed.events[end]).contains("sleep 20"));
    finish(resumed, "4-stop-sigint-resume");
}

#[test]
#[ignore = "needs the real claude binary"]
fn resume_continues_the_conversation() {
    let folder = fresh_folder();
    let id = session_id();
    let mut first = Session::start(&folder, Launch::New(&id), &[]);
    first.send_user(
        "The codeword is PINEAPPLE-42. Reply with exactly: NOTED",
        json!({}),
    );
    next_result(&mut first, 0);
    finish(first, "5-resume-first");

    let transcript = find_transcript(&id).expect("transcript exists after the first process");
    println!("folder: {}", folder.display());
    println!("transcript: {}", transcript.display());

    let mut second = Session::start(&folder, Launch::Resume(&id), &[]);
    second.send_user(
        "What is the codeword? Reply with only the codeword.",
        json!({}),
    );
    let end = next_result(&mut second, 0);
    let answer = result_of(&second.events[end]);
    println!("resumed answer: {answer}");
    println!("resumed session id: {}", second.events[end]["session_id"]);
    assert!(answer.contains("PINEAPPLE-42"));
    assert_eq!(second.events[end]["session_id"], id.as_str());
    finish(second, "5-resume-second");
}
