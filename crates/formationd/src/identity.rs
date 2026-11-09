use formation_core::peer::github;
use formation_core::peer::{Tailscale, Via};
use formation_core::ports::Result;
use formation_core::team::{LocalIdentity, TailnetSelf};
use formation_proto::common::{ApiError, ErrorCode};

/// This Mac's identity for the team service: its own Tailscale node and user, and
/// the GitHub login of the local `gh` session. Every call asks again, so starting
/// Tailscale or signing in to `gh` later needs no restart.
pub struct TailnetIdentity;

fn unavailable(message: String) -> ApiError {
    ApiError::new(ErrorCode::Internal, message)
}

impl LocalIdentity for TailnetIdentity {
    fn tailnet_self(&self) -> Result<TailnetSelf> {
        let tailscale = Tailscale::discover(Via::Auto);
        let node = tailscale
            .self_node()
            .ok_or_else(|| unavailable("Tailscale is not running on this Mac".into()))?;
        let resolved = tailscale
            .query(&node.ipv4.to_string())
            .map_err(|error| unavailable(format!("Tailscale WhoIs of this Mac: {error}")))?;
        let whois = resolved
            .identity
            .to_contract()
            .ok_or_else(|| unavailable("this Mac's Tailscale node has no signed-in user".into()))?;
        Ok(TailnetSelf {
            whois,
            host: node.dns_name.unwrap_or_else(|| node.ipv4.to_string()),
        })
    }

    fn github_login(&self) -> Result<String> {
        github::github_login().map_err(unavailable)
    }
}
