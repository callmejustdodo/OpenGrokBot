//! Owner: B. `stub-whois`: Tailscale WhoIs answers scripted per peer address, for
//! peer code and two-instance loopback tests (backend architecture, "Tests").

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Deserialize;

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::team::WhoIs;

use crate::ports::Result;

use super::{error, lock};

/// Resolves the Tailscale identity behind a peer connection's remote address.
///
/// No port defines this yet. TASK-007's LocalAPI (or `tailscale whois --json`)
/// client implements the same signature, and the trait moves into `ports/` as an
/// additive change; this stub then implements the port instead.
#[async_trait]
pub trait WhoIsResolver: Send + Sync {
    /// The caller's Tailscale login and node. `not_member` when the address is not
    /// a node of this tailnet, or the node is tagged and so has no user identity;
    /// `internal` when Tailscale cannot be asked.
    async fn whois(&self, addr: SocketAddr) -> Result<WhoIs>;
}

/// What the stub answers for one address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StubIdentity {
    /// A node owned by a Tailscale user.
    User(WhoIs),
    /// A tagged node: Tailscale gives it tags instead of a user (kb/1068), so it can
    /// never match a member. Resolves to `not_member`.
    Tagged { node: String, tags: Vec<String> },
    /// Any scripted failure, for example `internal` for an unreachable LocalAPI.
    Fail(ApiError),
}

impl StubIdentity {
    pub fn user(login: impl Into<String>, node: impl Into<String>) -> Self {
        Self::User(WhoIs {
            login: login.into(),
            node: node.into(),
        })
    }
}

/// A [`WhoIsResolver`] that answers from a table.
///
/// An address is looked up by `ip:port` first, then by IP (Tailscale identifies
/// nodes by address; the port only matters for proxied connections), then the
/// fallback. Unknown addresses get `not_member`, as Tailscale's "no match for
/// IP:port" does.
///
/// Two formationd instances over loopback both connect from `127.0.0.1`, so each
/// instance's stub maps `127.0.0.1` to the other instance's identity. With more
/// instances, make each client bind a known local port and map `127.0.0.1:<port>`.
#[derive(Debug, Default)]
pub struct StubWhoIs {
    state: Mutex<WhoIsState>,
}

#[derive(Debug, Default)]
struct WhoIsState {
    by_addr: HashMap<SocketAddr, StubIdentity>,
    by_ip: HashMap<IpAddr, StubIdentity>,
    fallback: Option<StubIdentity>,
    unavailable: Option<ApiError>,
    calls: Vec<SocketAddr>,
}

impl StubWhoIs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps every connection from `ip` to a user's node.
    pub fn with_user(
        self,
        ip: impl Into<IpAddr>,
        login: impl Into<String>,
        node: impl Into<String>,
    ) -> Self {
        self.insert_ip(ip.into(), StubIdentity::user(login, node));
        self
    }

    /// Maps every connection from `ip` to a tagged node.
    pub fn with_tagged(self, ip: impl Into<IpAddr>, node: &str, tags: &[&str]) -> Self {
        self.insert_ip(
            ip.into(),
            StubIdentity::Tagged {
                node: node.into(),
                tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            },
        );
        self
    }

    /// Fails every lookup of `ip` with `error`.
    pub fn with_failure(self, ip: impl Into<IpAddr>, error: ApiError) -> Self {
        self.insert_ip(ip.into(), StubIdentity::Fail(error));
        self
    }

    /// Answers `identity` for addresses in no table.
    pub fn with_fallback(self, identity: StubIdentity) -> Self {
        self.set_fallback(Some(identity));
        self
    }

    pub fn insert_ip(&self, ip: IpAddr, identity: StubIdentity) {
        lock(&self.state).by_ip.insert(ip, identity);
    }

    /// Takes precedence over [`StubWhoIs::insert_ip`] for this exact address.
    pub fn insert_addr(&self, addr: SocketAddr, identity: StubIdentity) {
        lock(&self.state).by_addr.insert(addr, identity);
    }

    /// Forgets `ip` and every `ip:port` entry on it, as when a node leaves the tailnet.
    pub fn remove_ip(&self, ip: IpAddr) {
        let mut state = lock(&self.state);
        state.by_ip.remove(&ip);
        state.by_addr.retain(|addr, _| addr.ip() != ip);
    }

    pub fn set_fallback(&self, identity: Option<StubIdentity>) {
        lock(&self.state).fallback = identity;
    }

    /// Fails every lookup with `error` (Tailscale stopped or signed out) until
    /// cleared with `None`.
    pub fn set_unavailable(&self, error: Option<ApiError>) {
        lock(&self.state).unavailable = error;
    }

    /// Every address looked up, in order.
    pub fn calls(&self) -> Vec<SocketAddr> {
        lock(&self.state).calls.clone()
    }

    /// Reads a table, so a formationd process can be given the stub by
    /// configuration. Each peer has `addr` (`ip` or `ip:port`) and one of `login`
    /// with `node`, `tags` with `node`, or `error`; `fallback` takes the same
    /// fields without `addr`:
    ///
    /// ```json
    /// { "peers": [
    ///     { "addr": "127.0.0.1", "login": "minsu@github", "node": "minsu-mbp" },
    ///     { "addr": "100.64.0.9", "node": "ci", "tags": ["tag:ci"] },
    ///     { "addr": "100.64.0.10", "error": { "code": "internal", "message": "LocalAPI down" } }
    ///   ],
    ///   "fallback": null }
    /// ```
    pub fn from_json(json: &str) -> Result<Self> {
        let table: Table = serde_json::from_str(json)
            .map_err(|e| error(ErrorCode::InvalidParams, format!("stub-whois table: {e}")))?;
        let stub = Self::new();
        for peer in table.peers {
            let identity = peer.entry.identity()?;
            if let Ok(addr) = peer.addr.parse::<SocketAddr>() {
                stub.insert_addr(addr, identity);
            } else if let Ok(ip) = peer.addr.parse::<IpAddr>() {
                stub.insert_ip(ip, identity);
            } else {
                return Err(error(
                    ErrorCode::InvalidParams,
                    format!("stub-whois table: {:?} is not an IP or ip:port", peer.addr),
                ));
            }
        }
        if let Some(fallback) = table.fallback {
            stub.set_fallback(Some(fallback.identity()?));
        }
        Ok(stub)
    }
}

#[async_trait]
impl WhoIsResolver for StubWhoIs {
    async fn whois(&self, addr: SocketAddr) -> Result<WhoIs> {
        let mut state = lock(&self.state);
        state.calls.push(addr);
        if let Some(error) = &state.unavailable {
            return Err(error.clone());
        }
        let identity = state
            .by_addr
            .get(&addr)
            .or_else(|| state.by_ip.get(&addr.ip()))
            .or(state.fallback.as_ref());
        match identity {
            Some(StubIdentity::User(whois)) => Ok(whois.clone()),
            Some(StubIdentity::Tagged { node, tags }) => Err(error(
                ErrorCode::NotMember,
                format!(
                    "{node} is a tagged node ({}) with no user identity",
                    tags.join(", ")
                ),
            )),
            Some(StubIdentity::Fail(error)) => Err(error.clone()),
            None => Err(error(
                ErrorCode::NotMember,
                format!("no tailnet node at {addr}"),
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    #[serde(default)]
    peers: Vec<Peer>,
    #[serde(default)]
    fallback: Option<Entry>,
}

#[derive(Deserialize)]
struct Peer {
    addr: String,
    #[serde(flatten)]
    entry: Entry,
}

#[derive(Deserialize)]
struct Entry {
    login: Option<String>,
    node: Option<String>,
    tags: Option<Vec<String>>,
    error: Option<ApiError>,
}

impl Entry {
    fn identity(self) -> Result<StubIdentity> {
        match self {
            Entry {
                error: Some(error),
                login: None,
                tags: None,
                ..
            } => Ok(StubIdentity::Fail(error)),
            Entry {
                login: Some(login),
                node: Some(node),
                tags: None,
                error: None,
            } => Ok(StubIdentity::user(login, node)),
            Entry {
                tags: Some(tags),
                node: Some(node),
                login: None,
                error: None,
            } => Ok(StubIdentity::Tagged { node, tags }),
            _ => Err(error(
                ErrorCode::InvalidParams,
                "stub-whois table: give login and node, tags and node, or error",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::sync::Arc;

    use super::super::block_on;
    use super::*;

    const MINSU: Ipv4Addr = Ipv4Addr::new(100, 79, 59, 24);

    fn addr(ip: impl Into<IpAddr>, port: u16) -> SocketAddr {
        SocketAddr::new(ip.into(), port)
    }

    #[test]
    fn answers_users_tagged_nodes_failures_and_unknown_addresses() {
        let stub = StubWhoIs::new()
            .with_user(MINSU, "minsu@github", "minsu-mbp")
            .with_tagged(Ipv4Addr::new(100, 64, 0, 9), "ci", &["tag:ci"])
            .with_failure(
                Ipv4Addr::new(100, 64, 0, 10),
                ApiError::new(ErrorCode::Internal, "LocalAPI down"),
            );
        let resolver: Arc<dyn WhoIsResolver> = Arc::new(stub);
        block_on(async {
            let whois = resolver.whois(addr(MINSU, 51234)).await.unwrap();
            assert_eq!(whois.login, "minsu@github");
            assert_eq!(whois.node, "minsu-mbp");

            let tagged = resolver
                .whois(addr(Ipv4Addr::new(100, 64, 0, 9), 1))
                .await
                .unwrap_err();
            assert_eq!(tagged.code, ErrorCode::NotMember);
            assert!(tagged.message.contains("tag:ci"));

            let failed = resolver
                .whois(addr(Ipv4Addr::new(100, 64, 0, 10), 1))
                .await
                .unwrap_err();
            assert_eq!(failed.code, ErrorCode::Internal);

            let unknown = resolver
                .whois(addr(Ipv4Addr::new(192, 168, 0, 194), 7413))
                .await
                .unwrap_err();
            assert_eq!(unknown.code, ErrorCode::NotMember);
        });
    }

    #[test]
    fn exact_address_wins_over_ip_and_ip_over_fallback() {
        let stub = StubWhoIs::new()
            .with_user(Ipv4Addr::LOCALHOST, "minsu@github", "minsu-mbp")
            .with_fallback(StubIdentity::user("someone@github", "elsewhere"));
        stub.insert_addr(
            addr(Ipv4Addr::LOCALHOST, 40001),
            StubIdentity::user("jiwoo@github", "jiwoo-mbp"),
        );
        block_on(async {
            let exact = stub.whois(addr(Ipv4Addr::LOCALHOST, 40001)).await.unwrap();
            assert_eq!(exact.login, "jiwoo@github");
            let by_ip = stub.whois(addr(Ipv4Addr::LOCALHOST, 40002)).await.unwrap();
            assert_eq!(by_ip.login, "minsu@github");
            let fallback = stub.whois(addr(Ipv6Addr::LOCALHOST, 1)).await.unwrap();
            assert_eq!(fallback.login, "someone@github");
        });
        assert_eq!(stub.calls().len(), 3);
    }

    #[test]
    fn two_loopback_instances_see_each_other() {
        // Instance A runs as Dohyeon and instance B as Minsu, both on 127.0.0.1.
        let stub_a = StubWhoIs::new().with_user(Ipv4Addr::LOCALHOST, "minsu@github", "minsu-mbp");
        let stub_b =
            StubWhoIs::new().with_user(Ipv4Addr::LOCALHOST, "dohyeon@github", "dohyeon-mbp");
        block_on(async {
            let from_b = stub_a
                .whois(addr(Ipv4Addr::LOCALHOST, 50001))
                .await
                .unwrap();
            let from_a = stub_b
                .whois(addr(Ipv4Addr::LOCALHOST, 50002))
                .await
                .unwrap();
            assert_eq!(from_b.login, "minsu@github");
            assert_eq!(from_a.login, "dohyeon@github");
        });
    }

    #[test]
    fn removal_and_unavailability_take_effect_at_once() {
        let stub = StubWhoIs::new().with_user(MINSU, "minsu@github", "minsu-mbp");
        stub.insert_addr(
            addr(MINSU, 7413),
            StubIdentity::user("minsu@github", "minsu-mbp"),
        );
        block_on(async {
            stub.set_unavailable(Some(ApiError::new(
                ErrorCode::Internal,
                "Tailscale stopped",
            )));
            let down = stub.whois(addr(MINSU, 7413)).await.unwrap_err();
            assert_eq!(down.code, ErrorCode::Internal);
            stub.set_unavailable(None);
            assert!(stub.whois(addr(MINSU, 7413)).await.is_ok());

            stub.remove_ip(MINSU.into());
            let gone = stub.whois(addr(MINSU, 7413)).await.unwrap_err();
            assert_eq!(gone.code, ErrorCode::NotMember);
        });
    }

    #[test]
    fn reads_a_json_table() {
        let stub = StubWhoIs::from_json(
            r#"{ "peers": [
                { "addr": "127.0.0.1", "login": "minsu@github", "node": "minsu-mbp" },
                { "addr": "127.0.0.1:40001", "login": "jiwoo@github", "node": "jiwoo-mbp" },
                { "addr": "100.64.0.9", "node": "ci", "tags": ["tag:ci"] },
                { "addr": "100.64.0.10", "error": { "code": "internal", "message": "down" } }
              ] }"#,
        )
        .unwrap();
        block_on(async {
            let loopback = |port| addr(Ipv4Addr::LOCALHOST, port);
            assert_eq!(stub.whois(loopback(1)).await.unwrap().login, "minsu@github");
            assert_eq!(
                stub.whois(loopback(40001)).await.unwrap().login,
                "jiwoo@github"
            );
            let tagged = stub.whois(addr(Ipv4Addr::new(100, 64, 0, 9), 1)).await;
            assert_eq!(tagged.unwrap_err().code, ErrorCode::NotMember);
            let failed = stub.whois(addr(Ipv4Addr::new(100, 64, 0, 10), 1)).await;
            assert_eq!(failed.unwrap_err().code, ErrorCode::Internal);
        });

        for bad in [
            r#"{ "peers": [ { "addr": "minsu-mbp", "login": "a", "node": "b" } ] }"#,
            r#"{ "peers": [ { "addr": "127.0.0.1", "login": "a" } ] }"#,
            r#"{ "peers": [ { "addr": "127.0.0.1", "login": "a", "node": "b", "tags": [] } ] }"#,
            r#"{ "peer": [] }"#,
        ] {
            let error = StubWhoIs::from_json(bad).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidParams, "{bad}");
        }
    }
}
