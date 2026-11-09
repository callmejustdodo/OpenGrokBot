//! Owner: B. The peer server: HTTP and WebSocket on this Mac's Tailscale address,
//! port 7413, under `/v1` (tech spec, "Peer protocol").
//!
//! Every call is identified by Tailscale WhoIs on the connection
//! ([`CallerIdentity`]) and mapped to a member with
//! [`Policy::authenticate`]; anyone else gets `403` with the contract's
//! [`PeerErrorBody`](formation_proto::peer::PeerErrorBody), except
//! `POST /v1/team/join`, which the invitation code guards. Calls from this Mac's
//! own member are refused too: WhoIs names a machine, so another process or OS
//! user on this Mac would otherwise pass as a teammate.
//!
//! | Endpoint | Server checks | Handed to |
//! | --- | --- | --- |
//! | `GET /v1/agents` | member | [`Policy::visible_agents`] |
//! | `POST /v1/ask` (`202`) | member; target on this Mac; [`Policy::check_ask`] | [`AskEndpoints::ask`] |
//! | `POST /v1/questions/{id}/action` | member | [`AskEndpoints::question_action`] |
//! | `POST /v1/requests` | member; [`Policy::check_request`] | [`RequestEndpoints::create`] |
//! | `GET /v1/requests/{id}`, `POST /v1/requests/{id}/cancel` | member | [`RequestEndpoints`] |
//! | `GET /v1/team` | member | [`TeamRepo::members`] |
//! | `POST /v1/team/join` | Tailscale identity only | [`JoinEndpoint::join`] |
//! | `WS /v1/events` | member, again on roster changes | [`PeerEventHub`] |
//!
//! Error codes map to HTTP statuses with [`status_for`].

mod caller;
mod error;
mod events;
mod handlers;
mod routes;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use formation_proto::common::{ChainId, MemberId};
use formation_proto::policy::{Caller, Chain};
use formation_proto::team::WhoIs;

use crate::ports::{Policy, Result, TeamRepo};

pub use caller::{BlockingIdentity, CallerIdentity, not_member};
pub use error::{PeerError, status_for};
pub use events::{Audience, PeerEventHub};
pub use handlers::{AskEndpoints, JoinEndpoint, RequestEndpoints};

/// What the server needs from the rest of formationd.
pub struct PeerApiDeps {
    pub identity: Arc<dyn CallerIdentity>,
    pub policy: Arc<dyn Policy>,
    /// This Mac's member and the roster (`GET /v1/team`).
    pub team: Arc<dyn TeamRepo + Send + Sync>,
    /// `None` answers `not_found`, as on a Mac that did not create the team.
    pub join: Option<Arc<dyn JoinEndpoint>>,
    pub ask: Option<Arc<dyn AskEndpoints>>,
    pub requests: Option<Arc<dyn RequestEndpoints>>,
    pub hub: PeerEventHub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerApiConfig {
    /// This Mac's Tailscale address and port 7413; loopback in tests and
    /// development. See [`check_bind_address`].
    pub bind: SocketAddr,
    /// How often each events connection is pinged.
    pub heartbeat: Duration,
}

impl PeerApiConfig {
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            heartbeat: Duration::from_secs(15),
        }
    }
}

/// A running peer server.
pub struct PeerApi {
    local_addr: SocketAddr,
    hub: PeerEventHub,
    stop: watch::Sender<bool>,
    task: JoinHandle<io::Result<()>>,
}

impl PeerApi {
    /// Binds and starts serving. Call inside the tokio runtime.
    pub async fn bind(config: PeerApiConfig, deps: PeerApiDeps) -> io::Result<Self> {
        check_bind_address(config.bind)?;
        let listener = TcpListener::bind(config.bind).await?;
        let local_addr = listener.local_addr()?;
        let (stop, stopped) = watch::channel(false);
        let hub = deps.hub.clone();
        let state = Arc::new(ApiState {
            identity: deps.identity,
            policy: deps.policy,
            team: deps.team,
            join: deps.join,
            ask: deps.ask,
            requests: deps.requests,
            hub: deps.hub,
            heartbeat: config.heartbeat,
            shutdown: stopped.clone(),
        });
        let app = routes::router(state).into_make_service_with_connect_info::<SocketAddr>();
        let mut stopped = stopped;
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|stop| *stop).await;
                })
                .await
        });
        Ok(Self {
            local_addr,
            hub,
            stop,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn hub(&self) -> &PeerEventHub {
        &self.hub
    }

    /// Closes every events connection, finishes calls in progress, and releases
    /// the port.
    pub async fn shutdown(self) -> io::Result<()> {
        self.stop.send_replace(true);
        self.task.await.map_err(io::Error::other)?
    }
}

/// The server binds only a Tailscale address (100.64.0.0/10 or
/// fd7a:115c:a1e0::/48), so nothing on the LAN or the internet can reach it, or
/// loopback, which nothing outside this Mac can reach (tests, development).
/// `0.0.0.0` and `::` are always refused.
pub fn check_bind_address(addr: SocketAddr) -> io::Result<()> {
    let ip = match addr.ip() {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    let allowed = ip.is_loopback()
        || match ip {
            IpAddr::V4(v4) => is_tailscale_v4(v4),
            IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        };
    if allowed {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to bind {addr}: not a Tailscale or loopback address"),
        ))
    }
}

fn is_tailscale_v4(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && (second & 0b1100_0000) == 0b0100_0000
}

pub(crate) struct ApiState {
    identity: Arc<dyn CallerIdentity>,
    policy: Arc<dyn Policy>,
    team: Arc<dyn TeamRepo + Send + Sync>,
    join: Option<Arc<dyn JoinEndpoint>>,
    ask: Option<Arc<dyn AskEndpoints>>,
    requests: Option<Arc<dyn RequestEndpoints>>,
    hub: PeerEventHub,
    heartbeat: Duration,
    shutdown: watch::Receiver<bool>,
}

impl ApiState {
    /// The member behind `whois`, never this Mac's own.
    async fn authenticate(&self, whois: &WhoIs) -> Result<MemberId> {
        let member_id = self.policy.authenticate(whois).await?;
        if self.team.me()? == Some(member_id) {
            return Err(not_member(
                "calls from this Mac's own member are refused on the peer port",
            ));
        }
        Ok(member_id)
    }
}

/// A teammate as [`Caller`]. Calls without a chain of their own (listing agents,
/// the events hello) start one at depth 0.
fn member_caller(member_id: MemberId, via_agent: Option<String>, chain: Option<Chain>) -> Caller {
    Caller::Member {
        member_id,
        via_agent,
        chain: chain.unwrap_or_else(|| Chain {
            id: ChainId::generate(),
            depth: 0,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_only_tailscale_or_loopback_addresses() {
        for ok in [
            "100.64.0.1:7413",
            "100.127.255.254:7413",
            "127.0.0.1:0",
            "[::1]:7413",
            "[fd7a:115c:a1e0::1]:7413",
            "[::ffff:100.100.1.2]:7413",
        ] {
            assert!(check_bind_address(ok.parse().unwrap()).is_ok(), "{ok}");
        }
        for refused in [
            "0.0.0.0:7413",
            "[::]:7413",
            "192.168.1.10:7413",
            "100.128.0.1:7413",
            "100.63.255.255:7413",
            "[fd7a:115c:a1e1::1]:7413",
        ] {
            assert!(
                check_bind_address(refused.parse().unwrap()).is_err(),
                "{refused}"
            );
        }
    }
}
