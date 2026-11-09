//! The `fake-formationd` command line: serve the fixtures, or control a running
//! fake. See README.md.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use fake_formationd::{fixtures, hub};
use serde_json::{Value, json};

const USAGE: &str = "\
usage:
  fake-formationd [serve] [--state NAME]   serve the fixtures (default state: SC-005/working)
  fake-formationd state NAME               switch the running fake to a state; clients reconnect
  fake-formationd play SCRIPT              play a script on the running fake
  fake-formationd list                     list states and scripts
  fake-formationd check                    check the fixtures against the contract and exit

options:
  --socket PATH     default: ~/Library/Application Support/Formation/fake-formationd.sock
  --fixtures DIR    default: the fixtures folder next to this tool's Cargo.toml";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fake-formationd: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    command: String,
    arg: Option<String>,
    socket: PathBuf,
    fixtures: PathBuf,
    state: String,
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut opts = Options {
        command: "serve".into(),
        arg: None,
        socket: default_socket()?,
        fixtures: fixtures::default_dir(),
        state: "SC-005/working".into(),
    };
    let mut positional = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--socket" => opts.socket = value("--socket")?.into(),
            "--fixtures" => opts.fixtures = value("--fixtures")?.into(),
            "--state" => opts.state = value("--state")?,
            "-h" | "--help" => return Err(USAGE.into()),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}\n{USAGE}")),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    if let Some(command) = positional.next() {
        opts.command = command;
    }
    opts.arg = positional.next();
    Ok(opts)
}

/// The fake's own socket, apart from the real formationd's, so both can run.
fn default_socket() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Formation/fake-formationd.sock"))
}

fn run(args: Vec<String>) -> Result<(), String> {
    let opts = parse(args)?;
    match opts.command.as_str() {
        "serve" => serve(&opts),
        "check" => {
            let f = fixtures::Fixtures::load(&opts.fixtures)?;
            println!(
                "fixtures OK: {} worlds, {} scripts, {} states, {} reacting methods",
                f.worlds.len(),
                f.scripts.len(),
                f.states.len(),
                f.reactions.len()
            );
            Ok(())
        }
        "state" => {
            let name = opts.arg.as_deref().ok_or("state needs a NAME")?;
            print(request(
                &opts.socket,
                "fake_state",
                json!({ "state": name }),
            )?)
        }
        "play" => {
            let name = opts.arg.as_deref().ok_or("play needs a SCRIPT")?;
            print(request(
                &opts.socket,
                "fake_play",
                json!({ "script": name }),
            )?)
        }
        "list" => {
            let list = request(&opts.socket, "fake_list", json!({}))?;
            println!("current: {}", list["current"].as_str().unwrap_or("?"));
            println!("\nstates:");
            for s in list["states"].as_array().into_iter().flatten() {
                let ui = s["ui"]
                    .as_str()
                    .map(|u| format!("  [app: {u}]"))
                    .unwrap_or_default();
                println!(
                    "  {:<26} {}{ui}",
                    s["name"].as_str().unwrap_or(""),
                    s["description"].as_str().unwrap_or("")
                );
            }
            println!("\nscripts:");
            for s in list["scripts"].as_array().into_iter().flatten() {
                println!(
                    "  {:<26} {}",
                    s["name"].as_str().unwrap_or(""),
                    s["description"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn serve(opts: &Options) -> Result<(), String> {
    let fixtures = fixtures::Fixtures::load(&opts.fixtures)?;
    let hub = hub::Hub::new(fixtures, &opts.state)?;
    if let Some(dir) = opts.socket.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if opts.socket.exists() {
        if UnixStream::connect(&opts.socket).is_ok() {
            return Err(format!("another fake is serving {}", opts.socket.display()));
        }
        std::fs::remove_file(&opts.socket).map_err(|e| e.to_string())?;
    }
    let listener =
        UnixListener::bind(&opts.socket).map_err(|e| format!("{}: {e}", opts.socket.display()))?;
    // Owner-only, like the real socket (tech spec, "Security").
    std::fs::set_permissions(&opts.socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    eprintln!(
        "fake-formationd: serving {} on {}",
        opts.state,
        opts.socket.display()
    );
    hub::serve(Arc::new(Mutex::new(hub)), listener);
    Ok(())
}

/// Sends one request to a running fake and returns its result.
fn request(socket: &PathBuf, method: &str, params: Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        format!(
            "{}: {e} (is `fake-formationd serve` running?)",
            socket.display()
        )
    })?;
    let line = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    writeln!(stream, "{line}").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    match reply.get("error") {
        Some(err) if !err.is_null() => Err(err["message"].as_str().unwrap_or("error").to_owned()),
        _ => Ok(reply["result"].clone()),
    }
}

fn print(value: Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}
