use formation_core::harness::claude::protocol::{self, Event};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    env,
    error::Error,
    fs,
    io::{self, BufRead, Write},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

fn write_line(out: &mut impl Write, line: &Value) -> io::Result<()> {
    if env::var_os("FAKE_CLAUDE_WRITE_TRANSCRIPT").is_some()
        && let Some(session) = line["session_id"].as_str()
    {
        let path = formation_core::ask::transcript::transcript_path(
            &env::var("CLAUDE_CONFIG_DIR")
                .map(std::path::PathBuf::from)
                .map_err(io::Error::other)?,
            &env::current_dir()?,
            session,
        )
        .map_err(io::Error::other)?;
        fs::create_dir_all(path.parent().unwrap())?;
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
            "{line}"
        )?;
    }
    writeln!(out, "{line}")?;
    out.flush()
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let mut session = None;
    let mut fork = false;
    let mut no_tools = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fork-session" => fork = true,
            "--tools" => no_tools = args.next().is_some_and(|tools| tools.is_empty()),
            "--session-id" | "--resume" => {
                session = Some(
                    args.next()
                        .filter(|s| !s.is_empty())
                        .ok_or("missing session ID")?,
                )
            }
            "--input-format"
            | "--output-format"
            | "--permission-prompt-tool"
            | "--mcp-config"
            | "--settings"
            | "--permission-mode" => {
                args.next().ok_or("missing flag value")?;
            }
            _ => {}
        }
    }
    let session = session.ok_or("requires --session-id or --resume")?;
    let session = if fork {
        formation_proto::common::RunId::generate().to_string()
    } else {
        session
    };
    if let Ok(path) = env::var("FAKE_CLAUDE_LAUNCH_LOG") {
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let record = json!({"session":session,"args":env::args().skip(1).collect::<Vec<_>>(),"run_id":env::var("FORMATION_RUN_ID").ok()});
        log.write_all(format!("{record}\n").as_bytes())?;
    }
    let accept_steer = env::var_os("FAKE_CLAUDE_ACCEPT_STEER").is_some();
    let delay = env::var("FAKE_CLAUDE_LINE_DELAY_MS").map_or(Ok(0), |v| v.parse::<u64>())?;
    let transcript = fs::read_to_string(env::var("FAKE_CLAUDE_TRANSCRIPT")?)?;
    let mut turns = VecDeque::new();
    let mut turn = VecDeque::new();
    for line in transcript.lines() {
        let mut value: Value = serde_json::from_str(line)?;
        let end = value["type"] == "result";
        if no_tools && value["type"] == "system" && value["subtype"] == "init" {
            value["tools"] = json!([]);
        }
        if let Some(id) = value.get_mut("session_id") {
            *id = json!(session);
        }
        turn.push_back(value);
        if end {
            turns.push_back(std::mem::take(&mut turn));
        }
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut out = io::stdout().lock();
    let mut active: Option<VecDeque<Value>> = None;
    let mut queued = 0usize;
    let mut eof = false;
    let mut next_line = Instant::now();
    loop {
        if active.is_none() && queued > 0 {
            queued -= 1;
            active = turns.pop_front();
            next_line = Instant::now();
        }
        if eof && active.is_none() && queued == 0 {
            break;
        }
        // Drain stdin before writing each line, including when delay is zero.
        let input = if eof {
            None
        } else if active.is_some() {
            match rx.recv_timeout(next_line.saturating_duration_since(Instant::now())) {
                Ok(line) => Some(line),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    eof = true;
                    None
                }
            }
        } else {
            match rx.recv() {
                Ok(line) => Some(line),
                Err(_) => {
                    eof = true;
                    None
                }
            }
        };
        if let Some(line) = input {
            // The protocol decoder preserves stdin-only shapes as Unknown values.
            let line = line?;
            let value = match protocol::decode_stdout_line(&line)? {
                Event::Unknown(value) => value,
                Event::User(_) => serde_json::from_str(&line)?,
                _ => continue,
            };
            if let Ok(path) = env::var("FAKE_CLAUDE_INPUT_LOG") {
                let mut log = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                writeln!(log, "{value}")?;
            }
            if value["type"] == "user" {
                if !accept_steer || active.is_none() || value["priority"] == "later" {
                    queued += 1;
                }
            } else if value["type"] == "control_request"
                && value["request"]["subtype"] == "interrupt"
            {
                let request_id = value["request_id"].as_str().ok_or("missing request_id")?;
                write_line(
                    &mut out,
                    &json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":{"still_queued":[]}}}),
                )?;
                if active.take().is_some() {
                    write_line(
                        &mut out,
                        &json!({"type":"result","session_id":session,"subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_tools"}),
                    )?;
                }
            }
            continue;
        }
        if let Some(lines) = &mut active {
            if next_line > Instant::now() {
                thread::sleep(next_line.saturating_duration_since(Instant::now()));
            }
            let line = lines.pop_front().expect("nonempty recorded turn");
            write_line(&mut out, &line)?;
            if lines.is_empty() {
                active = None;
            }
            next_line = Instant::now() + Duration::from_millis(delay);
        }
    }
    Ok(())
}
