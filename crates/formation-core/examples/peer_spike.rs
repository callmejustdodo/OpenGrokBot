//! TASK-007 spike entry point for the manual two-Mac run.
//!
//! ```text
//! peer_spike serve [--bind <tailscale-ipv4>] [--port <port>] [--via auto|localapi|cli]
//! peer_spike --whois <ip[:port]> [--via auto|localapi|cli]
//! peer_spike discover
//! ```
//!
//! `serve` binds this Mac's Tailscale IPv4 address (port 7413 by default),
//! prints one JSON line at start and one per connection, and runs until
//! stopped (Ctrl-C). From the other Mac:
//! `curl http://<this Mac's 100.x>:7413/spike/whoami`. The spike serves none of
//! the contract's `/v1` endpoints; they get `404`.
//! `--whois` asks the LocalAPI and the CLI about one address and compares them.
//! `discover` prints what was found. Tokens are never printed.

use std::io::Write;
use std::net::Ipv4Addr;
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::{Value, json};

use formation_core::peer::github;
use formation_core::peer::identity::{PeerIdentity, WhoIsError};
use formation_core::peer::server::{self, ConnectionLog};
use formation_core::peer::tailscale::{self, Tailscale, Via};
use formation_proto::peer::PEER_PORT;

const USAGE: &str = "usage:
  peer_spike serve [--bind <tailscale-ipv4>] [--port <port>] [--via auto|localapi|cli]
  peer_spike --whois <ip[:port]> [--via auto|localapi|cli]
  peer_spike discover";

struct Args {
    mode: Mode,
    bind: Option<Ipv4Addr>,
    port: u16,
    via: Via,
}

enum Mode {
    Serve,
    Whois(String),
    Discover,
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match args.mode {
        Mode::Serve => serve(&args),
        Mode::Whois(ref addr) => whois(addr, args.via),
        Mode::Discover => discover(),
    }
}

fn parse_args(args: Vec<String>) -> Result<Args, String> {
    let mut parsed = Args {
        mode: Mode::Discover,
        bind: None,
        port: PEER_PORT,
        via: Via::Auto,
    };
    let mut mode = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "serve" => mode = Some(Mode::Serve),
            "discover" => mode = Some(Mode::Discover),
            "--whois" => {
                let addr = value("--whois")?;
                let addr = tailscale::normalize_addr(&addr)
                    .ok_or(format!("--whois takes ip or ip:port, not {addr:?}"))?;
                mode = Some(Mode::Whois(addr));
            }
            "--bind" => {
                let ip = value("--bind")?;
                parsed.bind = Some(ip.parse().map_err(|_| format!("bad --bind {ip:?}"))?);
            }
            "--port" => {
                let port = value("--port")?;
                parsed.port = port.parse().map_err(|_| format!("bad --port {port:?}"))?;
            }
            "--via" => {
                let via = value("--via")?;
                parsed.via = Via::parse(&via).ok_or(format!("bad --via {via:?}"))?;
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    parsed.mode = mode.ok_or("choose serve, --whois, or discover")?;
    Ok(parsed)
}

fn serve(args: &Args) -> ExitCode {
    let tailscale = Tailscale::discover(args.via);
    let self_node = tailscale.self_node();
    let ip = match (args.bind, &self_node) {
        (Some(ip), Some(me)) if ip != me.ipv4 => {
            eprintln!(
                "refusing to bind {ip}: this Mac's Tailscale IPv4 address is {}",
                me.ipv4
            );
            return ExitCode::from(2);
        }
        (Some(ip), _) => ip,
        (None, Some(me)) => me.ipv4,
        (None, None) => {
            eprintln!("no Tailscale IPv4 address found; is Tailscale connected? Or pass --bind");
            return ExitCode::from(2);
        }
    };
    let listener = match server::bind(ip, args.port) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cannot listen on {ip}:{}: {error}", args.port);
            return ExitCode::from(1);
        }
    };
    let github = github::github_login();
    print_line(json!({
        "event": "listening",
        "addr": listener.local_addr().map(|addr| addr.to_string()).ok(),
        "pid": std::process::id(),
        "self_node": self_node.as_ref().map(|me| json!({"ipv4": me.ipv4.to_string(), "dns_name": me.dns_name})),
        "via": format!("{:?}", tailscale.via()),
        "cli": tailscale.cli().map(|path| path.display().to_string()),
        "localapi": tailscale.localapi().map(|api| api.describe()),
        "github_login": github.as_ref().ok(),
        "github_error": github.as_ref().err(),
    }));
    let log: Arc<server::Logger> = Arc::new(|entry: &ConnectionLog| {
        print_line(serde_json::to_value(entry).unwrap_or(Value::Null));
    });
    match server::serve(listener, Arc::new(tailscale), log) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("accept failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn whois(addr: &str, via: Via) -> ExitCode {
    let tailscale = Tailscale::discover(via);
    let mut line = json!({
        "addr": addr,
        "cli": tailscale.cli().map(|path| path.display().to_string()),
        "localapi_endpoint": tailscale.localapi().map(|api| api.describe()),
    });
    let mut answers = Vec::new();
    if via != Via::Cli {
        let started = std::time::Instant::now();
        let result = tailscale.query_localapi(addr);
        line["localapi"] = outcome(&result, started.elapsed());
        answers.push(result);
    }
    if via != Via::LocalApi {
        let started = std::time::Instant::now();
        let result = tailscale.query_cli(addr);
        line["cli_result"] = outcome(&result, started.elapsed());
        answers.push(result);
    }
    if let [Ok(a), Ok(b)] = answers.as_slice() {
        line["agree"] = json!(a == b);
    }
    print_line(line);
    if answers.iter().any(Result::is_ok) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn outcome(result: &Result<PeerIdentity, WhoIsError>, elapsed: std::time::Duration) -> Value {
    let ms = (elapsed.as_secs_f64() * 1e6).round() / 1e3;
    match result {
        Ok(identity) => json!({
            "ok": true,
            "ms": ms,
            "identity": identity,
            "verified": identity.verified_user().is_ok(),
            "whois": identity.to_contract(),
        }),
        Err(error) => json!({"ok": false, "ms": ms, "error": error.to_string()}),
    }
}

fn discover() -> ExitCode {
    let tailscale = Tailscale::discover(Via::Auto);
    let self_node = tailscale.self_node();
    let github = github::github_login();
    print_line(json!({
        "path_env": std::env::var("PATH").ok(),
        "cli": tailscale.cli().map(|path| path.display().to_string()),
        "localapi": tailscale.localapi().map(|api| api.describe()),
        "self_node": self_node.map(|me| json!({"ipv4": me.ipv4.to_string(), "dns_name": me.dns_name})),
        "gh": github::find_gh().map(|path| path.display().to_string()),
        "github_login": github.as_ref().ok(),
        "github_error": github.as_ref().err(),
    }));
    ExitCode::SUCCESS
}

/// One JSON line on stdout. A closed stdout (`| head -n 1`) is ignored rather
/// than allowed to panic: the server must keep answering.
fn print_line(value: Value) {
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}
