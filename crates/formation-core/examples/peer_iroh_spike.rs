//! iroh spike entry point for a run between two processes or two Macs. It is
//! built only with the `iroh` feature:
//!
//! ```text
//! cargo run -p formation-core --features iroh --example peer_iroh_spike -- \
//!     serve [--key <path>] [--allow <endpoint-id>]...
//! cargo run -p formation-core --features iroh --example peer_iroh_spike -- \
//!     call <endpoint-id> [--key <path>]
//! ```
//!
//! `serve` prints this endpoint's id, serves `GET /spike/whoami` and the
//! `GET /spike/events` WebSocket over iroh, prints one JSON line per request,
//! and runs until stopped (Ctrl-C). Without `--allow` it lets every caller in;
//! with it, only the endpoint ids it names, and every other caller gets `403`.
//! `call` dials the id, makes both calls on one connection, and prints the
//! answers and the connection's paths, each classified as `relay`, `tailscale`
//! (100.64.0.0/10, fd7a:115c:a1e0::/48), `lan` (loopback, private, link-local
//! and unique-local addresses) or `direct` (any other address). Only a selected
//! `direct` path shows hole punching: run the two on Macs on different
//! networks, with Tailscale off on both.
//!
//! Both ends use iroh's `presets::N0`, so the id alone is enough to reach the
//! other side. Each end connects to a home relay among n0's public relays,
//! which carries the connection until an IP path is selected. Each publishes a
//! record signed with its key to n0's pkarr server (`dns.iroh.link`), holding
//! only its home relay URL, not its IP addresses. `call` looks the server's id
//! up there and through DNS under `dns.iroh.link`.
//!
//! `--key` keeps the key in that file, created with mode 0600 if missing and
//! refused if anyone but its owner has access, so the id survives restarts.
//! Without it each run has a new id.

use std::fmt::Display;
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use futures_util::StreamExt;
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};
use serde_json::{Value, json};

use formation_core::peer::iroh_spike::{
    self, ALPN, Allowed, EVENTS, PeerKey, WHOAMI_PATH, load_or_create_secret_key, spike_router,
};
use formation_core::peer::is_tailscale_ip;

const USAGE: &str = "usage:
  peer_iroh_spike serve [--key <path>] [--allow <endpoint-id>]...
  peer_iroh_spike call <endpoint-id> [--key <path>]";

/// How long to wait for a home relay before printing the address anyway.
const ONLINE_WAIT: Duration = Duration::from_secs(15);
const CONNECT_WAIT: Duration = Duration::from_secs(30);
const REQUEST_WAIT: Duration = Duration::from_secs(15);
/// How long `call` waits for the connection to select an IP path over the
/// relay.
const IP_PATH_WAIT: Duration = Duration::from_secs(10);

struct Args {
    mode: Mode,
    key: Option<PathBuf>,
    allow: Vec<EndpointId>,
}

enum Mode {
    Serve,
    Call(EndpointId),
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let key = match &args.key {
        Some(path) => match load_or_create_secret_key(path) {
            Ok(key) => key,
            Err(error) => {
                eprintln!("cannot use key file {}: {error}", path.display());
                return ExitCode::from(1);
            }
        },
        None => SecretKey::generate(),
    };
    match args.mode {
        Mode::Serve => serve(key, &args).await,
        Mode::Call(remote) => call(key, remote).await,
    }
}

/// The arguments, or `None` when help was asked for.
fn parse_args(args: Vec<String>) -> Result<Option<Args>, String> {
    let mut mode = None;
    let mut key = None;
    let mut allow = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        let chosen = match arg.as_str() {
            "serve" => Some(Mode::Serve),
            "call" => Some(Mode::Call(parse_id(&value("call")?)?)),
            "--key" => {
                let path = PathBuf::from(value("--key")?);
                if key.replace(path).is_some() {
                    return Err("--key given more than once".into());
                }
                None
            }
            "--allow" => {
                allow.push(parse_id(&value("--allow")?)?);
                None
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}")),
        };
        if let Some(chosen) = chosen
            && mode.replace(chosen).is_some()
        {
            return Err("choose one of serve or call".into());
        }
    }
    let mode = mode.ok_or("choose serve or call")?;
    if matches!(mode, Mode::Call(_)) && !allow.is_empty() {
        return Err("--allow is for serve".into());
    }
    Ok(Some(Args { mode, key, allow }))
}

fn parse_id(raw: &str) -> Result<EndpointId, String> {
    raw.parse()
        .map_err(|error| format!("{raw:?} is not an endpoint id: {error}"))
}

async fn serve(key: SecretKey, args: &Args) -> ExitCode {
    let builder = Endpoint::builder(presets::N0)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()]);
    let endpoint = match builder.bind().await {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("cannot bind the endpoint: {error}");
            return ExitCode::from(1);
        }
    };
    let allowed = if args.allow.is_empty() {
        Allowed::Everyone
    } else {
        Allowed::Only(args.allow.iter().copied().collect())
    };
    print_line(json!({
        "event": "listening",
        "endpoint_id": endpoint.id().to_string(),
        "key_file": args.key.as_ref().map(|path| path.display().to_string()),
        "allowed": args.allow.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "pid": std::process::id(),
    }));
    let router = spike_router(allowed).layer(middleware::from_fn(log_request));
    let server = tokio::spawn(iroh_spike::serve(endpoint.clone(), router));
    let online = tokio::time::timeout(ONLINE_WAIT, endpoint.online())
        .await
        .is_ok();
    print_line(json!({
        "event": if online { "online" } else { "not_online" },
        "addr": describe_addr(&endpoint.addr()),
    }));
    match server.await {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            eprintln!("serve failed: {error}");
            ExitCode::from(1)
        }
        Err(error) => {
            eprintln!("serve task failed: {error}");
            ExitCode::from(1)
        }
    }
}

async fn log_request(
    ConnectInfo(caller): ConnectInfo<PeerKey>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    print_line(json!({
        "event": "request",
        "caller": caller.to_string(),
        "method": method,
        "path": path,
        "status": response.status().as_u16(),
    }));
    response
}

async fn call(key: SecretKey, remote: EndpointId) -> ExitCode {
    let endpoint = match Endpoint::builder(presets::N0).secret_key(key).bind().await {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("cannot bind the endpoint: {error}");
            return ExitCode::from(1);
        }
    };
    let ok = call_with(&endpoint, remote).await;
    endpoint.close().await;
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

async fn call_with(endpoint: &Endpoint, remote: EndpointId) -> bool {
    print_line(json!({
        "event": "calling",
        "endpoint_id": endpoint.id().to_string(),
        "remote": remote.to_string(),
    }));
    let started = Instant::now();
    let connection = match bounded(CONNECT_WAIT, endpoint.connect(remote, ALPN)).await {
        Ok(connection) => connection,
        Err(error) => {
            print_line(json!({"event": "connect_failed", "ms": ms(started), "error": error}));
            return false;
        }
    };
    let paths = describe_paths(&connection);
    print_line(json!({"event": "connected", "ms": ms(started), "paths": paths}));

    let started = Instant::now();
    let whoami_ok = match bounded(REQUEST_WAIT, iroh_spike::get(&connection, WHOAMI_PATH)).await {
        Ok((status, body)) => {
            let body = serde_json::from_slice::<Value>(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
            let status_code = status.as_u16();
            print_line(
                json!({"event": "whoami", "ms": ms(started), "status": status_code, "body": body}),
            );
            status.is_success()
        }
        Err(error) => {
            print_line(json!({"event": "whoami", "ms": ms(started), "error": error}));
            false
        }
    };

    let started = Instant::now();
    let events_ok = match bounded(REQUEST_WAIT, iroh_spike::events(&connection)).await {
        Ok(texts) => {
            let complete = texts == EVENTS;
            print_line(
                json!({"event": "events", "ms": ms(started), "messages": texts, "complete": complete}),
            );
            complete
        }
        Err(error) => {
            print_line(json!({"event": "events", "ms": ms(started), "error": error}));
            false
        }
    };

    let started = Instant::now();
    let _ = tokio::time::timeout(IP_PATH_WAIT, ip_path_selected(&connection)).await;
    let selected = connection
        .paths()
        .iter()
        .find(|path| path.is_selected())
        .map(|path| path_kind(path.remote_addr()));
    print_line(json!({
        "event": "paths",
        "selected_kind": selected,
        "direct_selected": selected == Some("direct"),
        "waited_ms": ms(started),
        "paths": describe_paths(&connection),
    }));
    if matches!(selected, Some("lan" | "tailscale")) {
        print_line(json!({
            "event": "note",
            "message": "The selected path is local (LAN or Tailscale), so this run does not show \
                hole punching. Run serve and call on two Macs on different networks, with \
                Tailscale off on both.",
        }));
    }
    connection.close(0u32.into(), b"done");
    whoami_ok && events_ok
}

/// `future`'s result, or an error once `wait` has passed.
async fn bounded<T, E: Display>(
    wait: Duration,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(wait, future).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err(format!("timed out after {wait:?}")),
    }
}

/// Waits until the connection selects an IP path over the relay, or ends.
async fn ip_path_selected(connection: &Connection) {
    let mut lists = connection.paths_stream();
    while let Some(list) = lists.next().await {
        if list.iter().any(|path| path.is_selected() && path.is_ip()) {
            return;
        }
    }
}

/// `relay`, `tailscale`, `lan` or `direct`. Only `direct` reaches another
/// network without a relay, which is what hole punching is for.
fn path_kind(addr: &TransportAddr) -> &'static str {
    match addr {
        TransportAddr::Relay(_) => "relay",
        TransportAddr::Ip(socket) => ip_kind(socket.ip()),
        _ => "other",
    }
}

fn ip_kind(ip: IpAddr) -> &'static str {
    // Tailscale's IPv6 range lies inside the unique-local one, so it goes first.
    if is_tailscale_ip(ip) {
        return "tailscale";
    }
    let local = match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unicast_link_local() || ip.is_unique_local(),
    };
    if local { "lan" } else { "direct" }
}

fn describe_paths(connection: &Connection) -> Value {
    let paths = connection.paths();
    paths
        .iter()
        .map(|path| {
            json!({
                "remote": path.remote_addr().to_string(),
                "kind": path_kind(path.remote_addr()),
                "selected": path.is_selected(),
                "rtt_ms": path.rtt().as_secs_f64() * 1e3,
            })
        })
        .collect()
}

fn describe_addr(addr: &EndpointAddr) -> Value {
    json!({
        "relays": addr.relay_urls().map(ToString::to_string).collect::<Vec<_>>(),
        "ip": addr.ip_addrs().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

fn ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 1e6).round() / 1e3
}

/// One JSON line on stdout. A closed stdout (`| head -n 1`) is ignored rather
/// than allowed to panic: the server must keep answering.
fn print_line(value: Value) {
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}
