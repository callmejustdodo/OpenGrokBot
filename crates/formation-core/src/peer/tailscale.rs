//! Finds this Mac's Tailscale and asks it who is calling.
//!
//! The LocalAPI is tried first (about a millisecond per lookup); the
//! `tailscale` CLI is the fallback (25 to 75 ms, since it starts a process).
//! The LocalAPI is found the way Tailscale's own CLI finds it
//! (`safesocket_darwin.go`, v1.104.2, `portAndTokenFromSameUserProof`, then
//! the Unix socket), in this order:
//!
//! 1. Mac App Store build: the `IPNExtension` process of this user holds open a
//!    file named `sameuserproof-<port>-<token>` in its group container; `lsof`
//!    finds it. The LocalAPI listens on `127.0.0.1:<port>`.
//! 2. Standalone build: `/Library/Tailscale/ipnport` is a symlink whose target
//!    is the port, and `/Library/Tailscale/sameuserproof-<port>` holds the
//!    token. That file is mode 0640 root:admin, so users outside the admin
//!    group fall through to the CLI.
//! 3. Open-source `tailscaled` (Homebrew formula): the Unix socket
//!    `/var/run/tailscaled.socket`, which needs no token.
//!
//! Like upstream, the group container is never listed directly. Only `lsof`
//! shows that a live `IPNExtension` of this user holds the proof file; a file
//! left behind after Tailscale quit proves nothing, and whatever process then
//! listens on its port (any local user's) would be asked who is calling.
//!
//! LocalAPI requests carry `Host: local-tailscaled.sock` and, over TCP, Basic
//! authorization with an empty user and the token as password. Any other host
//! is refused with 403. The port and token change whenever the Tailscale
//! extension restarts, so a failed request rediscovers them once.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::http;
use super::identity::{
    PeerIdentity, Resolved, WhoIsError, WhoIsResolver, WhoIsSource, parse_whois,
};
use super::process::{self, EnvChange};
use super::server::is_tailscale_ipv4;

/// Where the `tailscale` CLI may be. The GUI app's binary is the CLI too.
pub const CLI_CANDIDATES: [&str; 3] = [
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/local/bin/tailscale",
];

const LSOF: &str = "/usr/sbin/lsof";
const APP_STORE_PROOF_MARKER: &str = ".tailscale.ipn.macos/sameuserproof-";
const MACSYS_DIR: &str = "/Library/Tailscale";
const TAILSCALED_SOCKET: &str = "/var/run/tailscaled.socket";
/// The `Host` header Tailscale's own client sends to the LocalAPI.
const LOCALAPI_HOST: &str = "local-tailscaled.sock";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const CLI_TIMEOUT: Duration = Duration::from_secs(5);
const LSOF_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

/// The GUI app's binary acts as the CLI only when it sees a shell (`TERM`,
/// `SHLVL`, ...); under launchd it prints "The Tailscale GUI failed to start"
/// and still exits 0. `TAILSCALE_BE_CLI=1` forces CLI mode (Tailscale KB 1080,
/// and observed on 1.104.2). `XPC_SERVICE_NAME` is removed as Happier's
/// Tailscale runner does; that it matters is not verified. Success is judged by
/// parsing the output, never by the exit code.
const CLI_ENV: [EnvChange<'static>; 2] =
    [("TAILSCALE_BE_CLI", Some("1")), ("XPC_SERVICE_NAME", None)];

// getuid(2) takes no arguments, cannot fail, and touches no memory.
unsafe extern "C" {
    safe fn getuid() -> u32;
}

/// Which interface to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The LocalAPI, falling back to the CLI when the LocalAPI fails.
    Auto,
    LocalApi,
    Cli,
}

impl Via {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "localapi" => Some(Self::LocalApi),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }
}

/// How the LocalAPI endpoint was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalApiSource {
    Lsof,
    Macsys,
}

/// A LocalAPI endpoint. `Debug` and [`LocalApi::describe`] never show the token.
#[derive(Clone, PartialEq, Eq)]
pub enum LocalApi {
    /// Loopback TCP with a same-user-proof token (both GUI builds).
    Tcp {
        port: u16,
        token: String,
        source: LocalApiSource,
    },
    /// `tailscaled`'s Unix socket (open-source build).
    Unix { path: PathBuf },
}

impl fmt::Debug for LocalApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

impl LocalApi {
    /// For logs: the address and how it was found, with the token redacted.
    pub fn describe(&self) -> String {
        match self {
            Self::Tcp {
                port,
                token,
                source,
            } => {
                let found = match source {
                    LocalApiSource::Lsof => "lsof on IPNExtension (App Store)",
                    LocalApiSource::Macsys => "/Library/Tailscale (standalone)",
                };
                format!("127.0.0.1:{port} via {found}, token {} chars", token.len())
            }
            Self::Unix { path } => format!("unix socket {} (tailscaled)", path.display()),
        }
    }

    /// `GET path` on the LocalAPI.
    pub fn get(&self, path: &str) -> Result<http::Response, WhoIsError> {
        let mut request = format!(
            "GET {path} HTTP/1.1\r\nHost: {LOCALAPI_HOST}\r\nUser-Agent: formation-peer-spike\r\nAccept: application/json\r\nConnection: close\r\n"
        );
        let raw = match self {
            Self::Tcp { port, token, .. } => {
                request.push_str(&format!(
                    "Authorization: Basic {}\r\n\r\n",
                    http::base64(format!(":{token}").as_bytes())
                ));
                let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, *port));
                let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
                    .map_err(|error| failed(format!("LocalAPI {addr}: {error}")))?;
                stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
                stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
                exchange(stream, request.as_bytes())
                    .map_err(|error| failed(format!("LocalAPI {addr}: {error}")))?
            }
            Self::Unix { path } => {
                request.push_str("\r\n");
                let stream = UnixStream::connect(path)
                    .map_err(|error| failed(format!("LocalAPI {}: {error}", path.display())))?;
                stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
                stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
                exchange(stream, request.as_bytes())
                    .map_err(|error| failed(format!("LocalAPI {}: {error}", path.display())))?
            }
        };
        http::parse_response(&raw).map_err(|error| failed(format!("LocalAPI response: {error}")))
    }

    /// WhoIs for `addr` (`ip`, `ip:port`, or `[ipv6]:port`).
    pub fn whois(&self, addr: &str) -> Result<PeerIdentity, WhoIsError> {
        let response = self.get(&format!(
            "/localapi/v0/whois?addr={}",
            http::encode_query_value(addr)
        ))?;
        match response.status {
            200 => parse_whois(&response.body),
            404 => Err(WhoIsError::NotFound(response.body_text())),
            status => Err(failed(format!(
                "LocalAPI whois returned {status}: {}",
                response.body_text()
            ))),
        }
    }

    /// This Mac's own node, from `/localapi/v0/status` without the peer list.
    pub fn self_node(&self) -> Result<SelfNode, WhoIsError> {
        let response = self.get("/localapi/v0/status?peers=false")?;
        if response.status != 200 {
            return Err(failed(format!(
                "LocalAPI status returned {}: {}",
                response.status,
                response.body_text()
            )));
        }
        parse_status_self(&response.body)
            .ok_or_else(|| WhoIsError::BadResponse("status has no Tailscale IPv4".into()))
    }

    /// Every node's IPv4 by stable ID, from `/localapi/v0/status` with the peer
    /// list (see [`parse_status_nodes`]).
    pub fn node_addresses(&self) -> Result<NodeAddresses, WhoIsError> {
        let response = self.get("/localapi/v0/status")?;
        if response.status != 200 {
            return Err(failed(format!(
                "LocalAPI status returned {}: {}",
                response.status,
                response.body_text()
            )));
        }
        parse_status_nodes(&response.body)
    }
}

fn failed(detail: String) -> WhoIsError {
    WhoIsError::Failed(detail)
}

fn exchange<S: Read + Write>(mut stream: S, request: &[u8]) -> io::Result<Vec<u8>> {
    stream.write_all(request)?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream.take(MAX_RESPONSE).read_to_end(&mut raw)?;
    Ok(raw)
}

/// This Mac's node as its own Tailscale reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfNode {
    pub ipv4: Ipv4Addr,
    /// MagicDNS name without the trailing dot, when known.
    pub dns_name: Option<String>,
}

/// Reads `Self.TailscaleIPs` and `Self.DNSName` from an `ipnstate.Status`.
pub fn parse_status_self(json: &[u8]) -> Option<SelfNode> {
    let status: serde_json::Value = serde_json::from_slice(json).ok()?;
    let me = status.get("Self")?;
    let ipv4 = me
        .get("TailscaleIPs")?
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str()?.parse::<Ipv4Addr>().ok())
        .find(|ip| is_tailscale_ipv4(*ip))?;
    let dns_name = me
        .get("DNSName")
        .and_then(|name| name.as_str())
        .map(|name| name.trim_end_matches('.').to_owned())
        .filter(|name| !name.is_empty());
    Some(SelfNode { ipv4, dns_name })
}

/// Tailnet IPv4 addresses by node `StableID`.
pub type NodeAddresses = HashMap<String, Ipv4Addr>;

/// Reads `Self` and every `Peer[*]` of an `ipnstate.Status`: each node's `ID`
/// (its `StableID`, the same value WhoIs reports as `Node.StableID` and a
/// member's `tailscale_node` records) and its first tailnet IPv4 in
/// `TailscaleIPs`.
///
/// Tagged nodes are left out: a tagged node has no user, so it is never a
/// member's Mac, and a member's node that has since been tagged must not be
/// sent a member's questions. Nodes without a tailnet IPv4 are left out too,
/// since the peer server binds IPv4 only.
pub fn parse_status_nodes(json: &[u8]) -> Result<NodeAddresses, WhoIsError> {
    let status: RawStatus = serde_json::from_slice(json)
        .map_err(|error| WhoIsError::BadResponse(format!("not status JSON: {error}")))?;
    Ok(status
        .self_node
        .into_iter()
        .chain(status.peers.unwrap_or_default().into_values())
        .filter_map(RawPeerStatus::into_address)
        .collect())
}

#[derive(serde::Deserialize)]
struct RawStatus {
    #[serde(rename = "Self")]
    self_node: Option<RawPeerStatus>,
    /// Keyed by node key; `null` on a tailnet with no other nodes.
    #[serde(rename = "Peer")]
    peers: Option<HashMap<String, RawPeerStatus>>,
}

#[derive(serde::Deserialize)]
struct RawPeerStatus {
    #[serde(rename = "ID")]
    id: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    ips: Option<Vec<String>>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
}

impl RawPeerStatus {
    fn into_address(self) -> Option<(String, Ipv4Addr)> {
        if self.tags.is_some_and(|tags| !tags.is_empty()) {
            return None;
        }
        let id = self.id.filter(|id| !id.is_empty())?;
        let ipv4 = self
            .ips?
            .iter()
            .filter_map(|ip| ip.parse::<Ipv4Addr>().ok())
            .find(|ip| is_tailscale_ipv4(*ip))?;
        Some((id, ipv4))
    }
}

/// The first executable `tailscale` CLI.
pub fn find_cli() -> Option<PathBuf> {
    process::first_executable(CLI_CANDIDATES)
}

/// The LocalAPI endpoint, trying each install variant in turn.
pub fn discover_localapi() -> Option<LocalApi> {
    from_lsof()
        .or_else(|| from_macsys(Path::new(MACSYS_DIR)))
        .or_else(|| from_unix_socket(Path::new(TAILSCALED_SOCKET)))
}

fn from_lsof() -> Option<LocalApi> {
    let user = format!("-u{}", getuid());
    let output = process::run(
        Path::new(LSOF),
        &["-n", "-a", &user, "-c", "IPNExtension", "-F", "n"],
        &[],
        LSOF_TIMEOUT,
    )
    .ok()?;
    let (port, token) = parse_lsof_proof(&String::from_utf8_lossy(&output.stdout))?;
    Some(LocalApi::Tcp {
        port,
        token,
        source: LocalApiSource::Lsof,
    })
}

fn from_macsys(dir: &Path) -> Option<LocalApi> {
    let target = std::fs::read_link(dir.join("ipnport")).ok()?;
    let port = parse_ipnport(target.to_str()?)?;
    let token = std::fs::read_to_string(dir.join(format!("sameuserproof-{port}"))).ok()?;
    let token = token.trim();
    (is_token(token) && port_open(port)).then(|| LocalApi::Tcp {
        port,
        token: token.to_owned(),
        source: LocalApiSource::Macsys,
    })
}

fn from_unix_socket(path: &Path) -> Option<LocalApi> {
    let meta = std::fs::metadata(path).ok()?;
    meta.file_type().is_socket().then(|| LocalApi::Unix {
        path: path.to_owned(),
    })
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        CONNECT_TIMEOUT,
    )
    .is_ok()
}

/// Finds `…/<team>.group.io.tailscale.ipn.macos/sameuserproof-<port>-<token>`
/// in `lsof -F n` output (one `n<path>` line per open file). The standalone
/// build's `sameuserproof-<port>` has no token in its name and is skipped.
pub fn parse_lsof_proof(output: &str) -> Option<(u16, String)> {
    output.lines().find_map(|line| {
        let (_, rest) = line.split_once(APP_STORE_PROOF_MARKER)?;
        parse_port_and_token(rest)
    })
}

fn parse_port_and_token(rest: &str) -> Option<(u16, String)> {
    let (port, token) = rest.split_once('-')?;
    let port = parse_port(port)?;
    is_token(token).then(|| (port, token.to_owned()))
}

/// Parses the target of the standalone build's `ipnport` symlink.
pub fn parse_ipnport(target: &str) -> Option<u16> {
    parse_port(target.trim())
}

fn parse_port(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|port| *port != 0)
}

/// Tokens are hex (`%x` of random bytes).
fn is_token(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// WhoIs through the `tailscale` CLI at `cli`.
pub fn cli_whois(cli: &Path, addr: &str) -> Result<PeerIdentity, WhoIsError> {
    let output = process::run(cli, &["whois", "--json", addr], &CLI_ENV, CLI_TIMEOUT)
        .map_err(|error| failed(format!("tailscale CLI: {error}")))?;
    match parse_whois(&output.stdout) {
        Ok(identity) => Ok(identity),
        Err(parse_error) => {
            let line = output.first_line();
            let lower = line.to_ascii_lowercase();
            if lower.contains("peer not found") || lower.contains("no match") {
                Err(WhoIsError::NotFound(line))
            } else if line.is_empty() {
                Err(parse_error)
            } else {
                Err(failed(format!(
                    "tailscale whois ({}): {line}",
                    output.status
                )))
            }
        }
    }
}

/// Every node's IPv4 by stable ID through `tailscale status --json` at `cli`,
/// which prints the same `ipnstate.Status` as the LocalAPI.
pub fn cli_node_addresses(cli: &Path) -> Result<NodeAddresses, WhoIsError> {
    let output = process::run(cli, &["status", "--json"], &CLI_ENV, CLI_TIMEOUT)
        .map_err(|error| failed(format!("tailscale CLI: {error}")))?;
    parse_status_nodes(&output.stdout).map_err(|parse_error| {
        // The GUI binary prints an error and exits 0 instead of running the
        // command (see the module docs); name what it printed.
        let line = output.first_line();
        if line.is_empty() {
            parse_error
        } else {
            failed(format!("tailscale status ({}): {line}", output.status))
        }
    })
}

/// This Mac's Tailscale IPv4 from `tailscale ip -4`.
pub fn cli_self_ipv4(cli: &Path) -> Option<Ipv4Addr> {
    let output = process::run(cli, &["ip", "-4"], &CLI_ENV, CLI_TIMEOUT).ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<Ipv4Addr>().ok())
        .filter(|ip| is_tailscale_ipv4(*ip))
}

/// Finds the LocalAPI endpoint; [`discover_localapi`] is the real one.
pub type Discover = dyn Fn() -> Option<LocalApi> + Send + Sync;

/// This Mac's Tailscale: the CLI path and the LocalAPI endpoint, found once and
/// rediscovered when a LocalAPI request fails.
pub struct Tailscale {
    via: Via,
    cli: Option<PathBuf>,
    localapi: Mutex<Option<LocalApi>>,
    discover: Box<Discover>,
}

impl Tailscale {
    /// Finds what `via` needs.
    pub fn discover(via: Via) -> Self {
        let cli = if via == Via::LocalApi {
            None
        } else {
            find_cli()
        };
        let localapi = if via == Via::Cli {
            None
        } else {
            discover_localapi()
        };
        Self::new(via, cli, localapi)
    }

    pub fn new(via: Via, cli: Option<PathBuf>, localapi: Option<LocalApi>) -> Self {
        Self::with_discovery(via, cli, localapi, discover_localapi)
    }

    /// Like [`Tailscale::new`], but rediscovers the LocalAPI with `discover`
    /// instead of [`discover_localapi`], so tests stay off this Mac's
    /// Tailscale.
    pub fn with_discovery(
        via: Via,
        cli: Option<PathBuf>,
        localapi: Option<LocalApi>,
        discover: impl Fn() -> Option<LocalApi> + Send + Sync + 'static,
    ) -> Self {
        Self {
            via,
            cli,
            localapi: Mutex::new(localapi),
            discover: Box::new(discover),
        }
    }

    pub fn via(&self) -> Via {
        self.via
    }

    pub fn cli(&self) -> Option<&Path> {
        self.cli.as_deref()
    }

    pub fn localapi(&self) -> Option<LocalApi> {
        self.localapi
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// WhoIs for `addr` through the interface `via` names.
    pub fn query(&self, addr: &str) -> Result<Resolved, WhoIsError> {
        let localapi = |addr: &str| {
            self.query_localapi(addr).map(|identity| Resolved {
                identity,
                source: WhoIsSource::LocalApi,
            })
        };
        let cli = |addr: &str| {
            self.query_cli(addr).map(|identity| Resolved {
                identity,
                source: WhoIsSource::Cli,
            })
        };
        match self.via {
            Via::LocalApi => localapi(addr),
            Via::Cli => cli(addr),
            Via::Auto => match localapi(addr) {
                Ok(resolved) => Ok(resolved),
                // The CLI asks the same daemon; it would not know the peer either.
                Err(error @ WhoIsError::NotFound(_)) => Err(error),
                Err(api_error) if self.cli.is_some() => {
                    cli(addr).map_err(|cli_error| combine(api_error, cli_error))
                }
                Err(api_error) => Err(api_error),
            },
        }
    }

    /// WhoIs through the LocalAPI only, rediscovering the endpoint once when
    /// it is missing or a request fails.
    pub fn query_localapi(&self, addr: &str) -> Result<PeerIdentity, WhoIsError> {
        self.with_localapi(|api| api.whois(addr))
    }

    /// Every node's tailnet IPv4 by stable ID (this Mac's and its peers'),
    /// through the interface `via` names: the LocalAPI's status with peers, or
    /// `tailscale status --json`. Blocking, like [`Tailscale::query`].
    pub fn node_addresses(&self) -> Result<NodeAddresses, WhoIsError> {
        let localapi = || self.with_localapi(LocalApi::node_addresses);
        let cli = || {
            let cli = self.cli.as_deref().ok_or_else(|| {
                WhoIsError::Unavailable(format!(
                    "no tailscale CLI at {}",
                    CLI_CANDIDATES.join(", ")
                ))
            })?;
            cli_node_addresses(cli)
        };
        match self.via {
            Via::LocalApi => localapi(),
            Via::Cli => cli(),
            Via::Auto => match localapi() {
                Ok(nodes) => Ok(nodes),
                Err(api_error) if self.cli.is_some() => {
                    cli().map_err(|cli_error| combine(api_error, cli_error))
                }
                Err(api_error) => Err(api_error),
            },
        }
    }

    /// `request` on the LocalAPI, rediscovering the endpoint once when it is
    /// missing or the request fails.
    fn with_localapi<T>(
        &self,
        request: impl Fn(&LocalApi) -> Result<T, WhoIsError>,
    ) -> Result<T, WhoIsError> {
        let Some(current) = self.localapi().or_else(|| self.rediscover(None)) else {
            return Err(WhoIsError::Unavailable(
                "no Tailscale LocalAPI found".into(),
            ));
        };
        match request(&current) {
            Err(WhoIsError::Failed(detail)) => match self.rediscover(Some(&current)) {
                Some(fresh) => request(&fresh),
                None => Err(WhoIsError::Failed(detail)),
            },
            other => other,
        }
    }

    /// WhoIs through the CLI only.
    pub fn query_cli(&self, addr: &str) -> Result<PeerIdentity, WhoIsError> {
        let cli = self.cli.as_deref().ok_or_else(|| {
            WhoIsError::Unavailable(format!("no tailscale CLI at {}", CLI_CANDIDATES.join(", ")))
        })?;
        cli_whois(cli, addr)
    }

    /// This Mac's node, from the LocalAPI or else `tailscale ip -4`.
    pub fn self_node(&self) -> Option<SelfNode> {
        if let Some(localapi) = self.localapi()
            && let Ok(node) = localapi.self_node()
        {
            return Some(node);
        }
        let cli = self.cli.clone().or_else(find_cli)?;
        let ipv4 = cli_self_ipv4(&cli)?;
        Some(SelfNode {
            ipv4,
            dns_name: None,
        })
    }

    /// A newly found endpoint that differs from `stale`, stored for next time.
    fn rediscover(&self, stale: Option<&LocalApi>) -> Option<LocalApi> {
        let fresh = (self.discover)()?;
        if stale == Some(&fresh) {
            return None;
        }
        *self.localapi.lock().unwrap_or_else(PoisonError::into_inner) = Some(fresh.clone());
        Some(fresh)
    }
}

impl WhoIsResolver for Tailscale {
    fn whois(&self, addr: SocketAddr) -> Result<Resolved, WhoIsError> {
        self.query(&addr.to_string())
    }
}

/// The WhoIs form of an address typed as `ip` or `ip:port`; `None` for a host
/// name, which WhoIs does not accept.
pub fn normalize_addr(text: &str) -> Option<String> {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Some(addr.to_string());
    }
    text.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

fn combine(api_error: WhoIsError, cli_error: WhoIsError) -> WhoIsError {
    match cli_error {
        WhoIsError::NotFound(detail) => WhoIsError::NotFound(detail),
        cli_error => WhoIsError::Failed(format!("LocalAPI: {api_error}; CLI: {cli_error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;
    use crate::peer::identity::fixtures::WHOIS_USER;

    const TOKEN: &str = "0123456789abcdef0123";

    #[test]
    fn finds_the_app_store_proof_in_lsof_output() {
        let output = format!(
            "p493\nfcwd\nn/\nftxt\nn/Applications/Tailscale.app/Contents/PlugIns/IPNExtension.appex/Contents/MacOS/IPNExtension\n\
             f3\nn/Users/USER/Library/Group Containers/TEAMID.group.io.tailscale.ipn.macos/ipn.log\n\
             f7\nn/Users/USER/Library/Group Containers/TEAMID.group.io.tailscale.ipn.macos/sameuserproof-49159-{TOKEN}\n\
             f8\nn127.0.0.1:49159\n"
        );
        assert_eq!(parse_lsof_proof(&output), Some((49159, TOKEN.to_owned())));
    }

    #[test]
    fn ignores_lsof_output_without_a_valid_proof() {
        for output in [
            "",
            "p493\nn/Users/USER/Library/Group Containers/TEAMID.group.io.tailscale.ipn.macos/ipn.log\n",
            // The standalone build's file name has no token.
            "n/Library/Tailscale/sameuserproof-49159\n",
            // Another app's container.
            "n/Users/USER/Library/Group Containers/TEAMID.group.example.macos/sameuserproof-49159-abcd\n",
            "n/x.tailscale.ipn.macos/sameuserproof-notaport-abcd\n",
            "n/x.tailscale.ipn.macos/sameuserproof-70000-abcd\n",
            "n/x.tailscale.ipn.macos/sameuserproof-49159-\n",
            "n/x.tailscale.ipn.macos/sameuserproof-49159-not-hex!\n",
        ] {
            assert_eq!(parse_lsof_proof(output), None, "{output:?}");
        }
    }

    #[test]
    fn parses_proof_file_names_in_lsof_output() {
        let line = |name: &str| {
            format!(
                "n/Users/USER/Library/Group Containers/TEAMID.group.io.tailscale.ipn.macos/{name}\n"
            )
        };
        assert_eq!(
            parse_lsof_proof(&line(&format!("sameuserproof-49159-{TOKEN}"))),
            Some((49159, TOKEN.to_owned()))
        );
        assert_eq!(
            parse_lsof_proof(&line("sameuserproof-1-ABCDEF")),
            Some((1, "ABCDEF".to_owned()))
        );
        for name in [
            "sameuserproof-49159",
            "sameuserproof--abcd",
            "sameuserproof-0-abcd",
            "sameuserproof-+80-abcd",
            "sameuserproof-65536-abcd",
            "sameuserproof-49159-xyz",
            "sameuserproof-49159-ab-cd",
            "ipnport",
            "xsameuserproof-49159-abcd",
        ] {
            assert_eq!(parse_lsof_proof(&line(name)), None, "{name}");
        }
    }

    #[test]
    fn parses_the_ipnport_symlink_target() {
        assert_eq!(parse_ipnport("49159"), Some(49159));
        assert_eq!(parse_ipnport("49159\n"), Some(49159));
        for target in [
            "",
            "0",
            "65536",
            "-1",
            "port",
            "49159/",
            "/Library/Tailscale/49159",
        ] {
            assert_eq!(parse_ipnport(target), None, "{target:?}");
        }
    }

    #[test]
    fn reads_the_self_node_from_status() {
        let status = br#"{"Version":"1.104.2","BackendState":"Running",
            "Self":{"HostName":"alice-mbp","DNSName":"alice-mbp.tail0000.ts.net.",
            "TailscaleIPs":["fd7a:115c:a1e0::1234:5678","100.101.102.103"]},
            "CurrentTailnet":{"MagicDNSSuffix":"tail0000.ts.net"}}"#;
        assert_eq!(
            parse_status_self(status),
            Some(SelfNode {
                ipv4: Ipv4Addr::new(100, 101, 102, 103),
                dns_name: Some("alice-mbp.tail0000.ts.net".into()),
            })
        );
        for status in [
            &br#"{"Self":{"TailscaleIPs":["fd7a:115c:a1e0::1"]}}"#[..],
            br#"{"Self":{"TailscaleIPs":["192.168.0.10"]}}"#,
            br#"{"Self":null}"#,
            br#"{"BackendState":"Stopped"}"#,
            b"not json",
        ] {
            assert_eq!(parse_status_self(status), None);
        }
    }

    #[test]
    fn describe_and_debug_redact_the_token() {
        let api = LocalApi::Tcp {
            port: 49159,
            token: TOKEN.into(),
            source: LocalApiSource::Lsof,
        };
        for text in [api.describe(), format!("{api:?}")] {
            assert!(!text.contains(TOKEN), "{text}");
            assert!(text.contains("127.0.0.1:49159"), "{text}");
        }
    }

    #[test]
    fn nothing_found_means_unavailable_not_a_guess() {
        let tailscale = Tailscale::new(Via::Cli, None, None);
        assert!(matches!(
            tailscale.query("100.101.102.103:7413"),
            Err(WhoIsError::Unavailable(_))
        ));
        let (discover, discovered) = counting(None);
        for via in [Via::LocalApi, Via::Auto] {
            let tailscale = Tailscale::with_discovery(via, None, None, discover.clone());
            assert!(matches!(
                tailscale.query(ADDR),
                Err(WhoIsError::Unavailable(_))
            ));
        }
        assert_eq!(discovered.load(Ordering::SeqCst), 2);
    }

    /// A LocalAPI on a loopback port that gives every request the same answer
    /// and keeps each request head.
    struct FakeLocalApi {
        port: u16,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl FakeLocalApi {
        fn start(status: u16, body: &'static str) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && matches!(stream.read(&mut byte), Ok(1)) {
                        head.push(byte[0]);
                    }
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
            Self { port, requests }
        }

        fn endpoint(&self) -> LocalApi {
            LocalApi::Tcp {
                port: self.port,
                token: TOKEN.into(),
                source: LocalApiSource::Lsof,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// An endpoint on a port nothing listens on.
    fn dead_endpoint() -> LocalApi {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        LocalApi::Tcp {
            port,
            token: TOKEN.into(),
            source: LocalApiSource::Lsof,
        }
    }

    /// Discovery that always finds `endpoint`, and counts how often it ran.
    fn counting(
        endpoint: Option<LocalApi>,
    ) -> (
        impl Fn() -> Option<LocalApi> + Clone + Send + Sync + 'static,
        Arc<AtomicUsize>,
    ) {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let discover = move || {
            counter.fetch_add(1, Ordering::SeqCst);
            endpoint.clone()
        };
        (discover, count)
    }

    /// A stand-in `tailscale` CLI that prints `stdout` whatever it is asked.
    struct FakeCli {
        dir: PathBuf,
    }

    impl FakeCli {
        fn printing(name: &str, stdout: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let dir =
                std::env::temp_dir().join(format!("formation-peer-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("tailscale");
            std::fs::write(&script, format!("#!/bin/sh\ncat <<'END'\n{stdout}\nEND\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir }
        }

        fn path(&self) -> PathBuf {
            self.dir.join("tailscale")
        }
    }

    impl Drop for FakeCli {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const ADDR: &str = "100.101.102.103:52000";

    fn alice() -> PeerIdentity {
        parse_whois(WHOIS_USER.as_bytes()).unwrap()
    }

    #[test]
    fn a_localapi_request_carries_the_host_and_the_token() {
        let api = FakeLocalApi::start(200, WHOIS_USER);
        assert_eq!(api.endpoint().whois(ADDR), Ok(alice()));
        let requests = api.requests();
        assert_eq!(requests.len(), 1);
        let head = &requests[0];
        assert!(
            head.starts_with("GET /localapi/v0/whois?addr=100.101.102.103%3A52000 HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(
            head.contains("\r\nHost: local-tailscaled.sock\r\n"),
            "{head}"
        );
        let auth = format!(
            "\r\nAuthorization: Basic {}\r\n",
            http::base64(format!(":{TOKEN}").as_bytes())
        );
        assert!(head.contains(&auth), "{head}");
    }

    #[test]
    fn auto_stops_at_not_found_without_asking_the_cli() {
        let api = FakeLocalApi::start(404, "no match for IP:port");
        let cli = FakeCli::printing("not-found", WHOIS_USER);
        let (discover, discovered) = counting(Some(api.endpoint()));
        let tailscale = Tailscale::with_discovery(
            Via::Auto,
            Some(cli.path()),
            Some(api.endpoint()),
            discover.clone(),
        );
        // The CLI would have answered; NotFound must win.
        assert_eq!(
            tailscale.query(ADDR),
            Err(WhoIsError::NotFound("no match for IP:port".into()))
        );
        assert_eq!(discovered.load(Ordering::SeqCst), 0);
        assert_eq!(api.requests().len(), 1);
    }

    #[test]
    fn auto_falls_back_to_the_cli_when_the_localapi_fails() {
        let api = FakeLocalApi::start(403, "bad password");
        let cli = FakeCli::printing("fallback", WHOIS_USER);
        // Discovery finds the same endpoint again, so there is nothing to retry.
        let (discover, discovered) = counting(Some(api.endpoint()));
        let tailscale = Tailscale::with_discovery(
            Via::Auto,
            Some(cli.path()),
            Some(api.endpoint()),
            discover.clone(),
        );
        assert_eq!(
            tailscale.query(ADDR),
            Ok(Resolved {
                identity: alice(),
                source: WhoIsSource::Cli,
            })
        );
        assert_eq!(discovered.load(Ordering::SeqCst), 1);
        assert_eq!(api.requests().len(), 1);
    }

    #[test]
    fn auto_falls_back_to_the_cli_on_an_answer_that_is_not_whois() {
        let api = FakeLocalApi::start(200, "{}");
        let cli = FakeCli::printing("bad-body", WHOIS_USER);
        let (discover, discovered) = counting(None);
        let tailscale = Tailscale::with_discovery(
            Via::Auto,
            Some(cli.path()),
            Some(api.endpoint()),
            discover.clone(),
        );
        assert_eq!(
            tailscale.query(ADDR).map(|resolved| resolved.source),
            Ok(WhoIsSource::Cli)
        );
        // Only a failed request rediscovers; a bad answer does not.
        assert_eq!(discovered.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn when_both_fail_the_error_names_both() {
        let api = FakeLocalApi::start(500, "boom");
        let (discover, _) = counting(None);
        // /bin/echo stands in for a CLI that prints no JSON.
        let tailscale = Tailscale::with_discovery(
            Via::Auto,
            Some(PathBuf::from("/bin/echo")),
            Some(api.endpoint()),
            discover.clone(),
        );
        match tailscale.query(ADDR) {
            Err(WhoIsError::Failed(detail)) => {
                assert!(
                    detail.contains("LocalAPI: LocalAPI whois returned 500"),
                    "{detail}"
                );
                assert!(detail.contains("CLI: tailscale whois"), "{detail}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stale_endpoint_is_rediscovered_once_and_kept() {
        // After a restart the old port refuses the token and a new one answers.
        let stale = FakeLocalApi::start(403, "bad password");
        let fresh = FakeLocalApi::start(200, WHOIS_USER);
        let (discover, discovered) = counting(Some(fresh.endpoint()));
        let tailscale = Tailscale::with_discovery(
            Via::LocalApi,
            None,
            Some(stale.endpoint()),
            discover.clone(),
        );
        assert_eq!(
            tailscale.query(ADDR),
            Ok(Resolved {
                identity: alice(),
                source: WhoIsSource::LocalApi,
            })
        );
        assert_eq!(tailscale.localapi(), Some(fresh.endpoint()));
        assert_eq!(
            tailscale.query(ADDR).map(|r| r.source),
            Ok(WhoIsSource::LocalApi)
        );
        assert_eq!(discovered.load(Ordering::SeqCst), 1);
        assert_eq!((stale.requests().len(), fresh.requests().len()), (1, 2));
    }

    #[test]
    fn a_dead_or_missing_endpoint_is_rediscovered() {
        let fresh = FakeLocalApi::start(200, WHOIS_USER);
        for initial in [Some(dead_endpoint()), None] {
            let (discover, discovered) = counting(Some(fresh.endpoint()));
            let tailscale =
                Tailscale::with_discovery(Via::LocalApi, None, initial.clone(), discover.clone());
            assert_eq!(tailscale.query_localapi(ADDR), Ok(alice()), "{initial:?}");
            assert_eq!(discovered.load(Ordering::SeqCst), 1, "{initial:?}");
        }
    }

    #[test]
    fn rediscovery_is_tried_once_per_lookup() {
        let stale = FakeLocalApi::start(403, "bad password");
        let fresh = FakeLocalApi::start(403, "bad password");
        let (discover, discovered) = counting(Some(fresh.endpoint()));
        let tailscale = Tailscale::with_discovery(
            Via::LocalApi,
            None,
            Some(stale.endpoint()),
            discover.clone(),
        );
        match tailscale.query(ADDR) {
            Err(WhoIsError::Failed(detail)) => assert!(detail.contains("403"), "{detail}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(discovered.load(Ordering::SeqCst), 1);
        assert_eq!((stale.requests().len(), fresh.requests().len()), (1, 1));
    }

    #[test]
    fn a_cli_that_prints_no_json_is_a_failure() {
        // Stands in for the App Store binary started without TAILSCALE_BE_CLI:
        // it prints the GUI error and exits 0.
        let error = cli_whois(Path::new("/bin/echo"), "100.101.102.103:7413").unwrap_err();
        match error {
            WhoIsError::Failed(detail) => assert!(detail.contains("exit status: 0"), "{detail}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn normalizes_whois_addresses() {
        assert_eq!(
            normalize_addr("100.101.102.103:7413").as_deref(),
            Some("100.101.102.103:7413")
        );
        assert_eq!(
            normalize_addr("100.101.102.103").as_deref(),
            Some("100.101.102.103")
        );
        assert_eq!(
            normalize_addr("[fd7a:115c:a1e0::1]:7413").as_deref(),
            Some("[fd7a:115c:a1e0::1]:7413")
        );
        assert_eq!(normalize_addr("alice-mbp"), None);
    }

    /// Run with `cargo test -p formation-core -- --ignored` on a Mac signed in
    /// to Tailscale.
    #[test]
    #[ignore = "needs Tailscale"]
    fn whois_resolves_this_macs_own_tailscale_address() {
        let tailscale = Tailscale::discover(Via::Auto);
        let me = tailscale.self_node().expect("this Mac's Tailscale IPv4");
        let addr = SocketAddr::from((me.ipv4, 7413)).to_string();

        let via_localapi = tailscale.query_localapi(&addr);
        let via_cli = tailscale.query_cli(&addr);
        eprintln!(
            "self {me:?}\nlocalapi {:?}\n  -> {via_localapi:?}\ncli {:?}\n  -> {via_cli:?}",
            tailscale.localapi(),
            tailscale.cli()
        );
        let identity = match (&via_localapi, &via_cli) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a, b, "the LocalAPI and the CLI disagree");
                a.clone()
            }
            (Ok(identity), Err(_)) | (Err(_), Ok(identity)) => identity.clone(),
            (Err(a), Err(b)) => panic!("both failed: {a}; {b}"),
        };
        assert!(!identity.node.stable_id.is_empty());
        if let Some(dns_name) = &me.dns_name {
            assert_eq!(&identity.node.name, dns_name);
        }
        assert!(identity.verified_user().is_ok(), "{identity:?}");

        // An address Tailscale does not know is not found, not an error.
        assert!(matches!(
            tailscale.query("100.64.0.1:7413"),
            Err(WhoIsError::NotFound(_))
        ));
    }

    const STATUS_PEERS: &str = include_str!("testdata/status-peers.json");

    #[test]
    fn reads_every_users_node_ipv4_from_status() {
        let nodes = parse_status_nodes(STATUS_PEERS.as_bytes()).unwrap();
        assert_eq!(
            nodes,
            NodeAddresses::from([
                // This Mac.
                (
                    "nEXAMPLE0001CNTRL".to_owned(),
                    Ipv4Addr::new(100, 101, 102, 103)
                ),
                // IPv6 listed first; the IPv4 is still found.
                (
                    "nEXAMPLE0002CNTRL".to_owned(),
                    Ipv4Addr::new(100, 88, 77, 66)
                ),
                // The tagged server and the IPv6-only node are left out.
            ])
        );
    }

    #[test]
    fn status_without_peers_or_usable_nodes() {
        let cases: [(&[u8], usize); 5] = [
            (
                br#"{"Self":{"ID":"nA","TailscaleIPs":["100.64.0.1"]},"Peer":null}"#,
                1,
            ),
            (br#"{"BackendState":"Stopped","Self":null}"#, 0),
            (
                br#"{"Peer":{"nodekey:1":{"ID":"","TailscaleIPs":["100.64.0.2"]}}}"#,
                0,
            ),
            (
                br#"{"Peer":{"nodekey:1":{"ID":"nB","TailscaleIPs":["192.168.0.2"]}}}"#,
                0,
            ),
            (
                br#"{"Peer":{"nodekey:1":{"ID":"nB","TailscaleIPs":["100.64.0.2"],"Tags":[]}}}"#,
                1,
            ),
        ];
        for (json, expected) in cases {
            let nodes = parse_status_nodes(json).unwrap();
            assert_eq!(nodes.len(), expected, "{}", String::from_utf8_lossy(json));
        }
        for json in [&b"not json"[..], b"", b"[1]"] {
            assert!(matches!(
                parse_status_nodes(json),
                Err(WhoIsError::BadResponse(_))
            ));
        }
    }

    #[test]
    fn node_addresses_come_from_the_localapi_status_with_peers() {
        let api = FakeLocalApi::start(200, STATUS_PEERS);
        let (discover, _) = counting(None);
        let tailscale =
            Tailscale::with_discovery(Via::LocalApi, None, Some(api.endpoint()), discover);
        let nodes = tailscale.node_addresses().unwrap();
        assert_eq!(
            nodes.get("nEXAMPLE0002CNTRL"),
            Some(&Ipv4Addr::new(100, 88, 77, 66))
        );
        let requests = api.requests();
        assert!(
            requests[0].starts_with("GET /localapi/v0/status HTTP/1.1\r\n"),
            "{}",
            requests[0]
        );
    }

    #[test]
    fn node_addresses_fall_back_to_the_cli() {
        let api = FakeLocalApi::start(403, "bad password");
        let cli = FakeCli::printing("status", STATUS_PEERS);
        let (discover, _) = counting(Some(api.endpoint()));
        let tailscale =
            Tailscale::with_discovery(Via::Auto, Some(cli.path()), Some(api.endpoint()), discover);
        let nodes = tailscale.node_addresses().unwrap();
        assert_eq!(nodes.len(), 2);

        // A CLI that prints text instead of JSON (the GUI binary's error, say) is
        // named in the error, not reported as bad JSON. /bin/echo stands in.
        let tailscale = Tailscale::new(Via::Cli, Some(PathBuf::from("/bin/echo")), None);
        match tailscale.node_addresses() {
            Err(WhoIsError::Failed(detail)) => {
                assert!(detail.contains("status --json"), "{detail}")
            }
            other => panic!("{other:?}"),
        }
    }
}
