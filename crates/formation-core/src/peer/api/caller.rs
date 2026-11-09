//! Who is calling the peer server.
//!
//! The server identifies every connection by its TCP peer address alone, never by
//! a header, and asks [`CallerIdentity`] for the Tailscale user and node behind
//! it; [`Policy::authenticate`](crate::ports::Policy::authenticate) then maps that
//! to a member. formationd adapts TASK-007's Tailscale WhoIs resolver (LocalAPI,
//! else CLI), which is synchronous, with [`BlockingIdentity`]:
//!
//! ```ignore
//! let identity = BlockingIdentity::new(move |addr| {
//!     let resolved = tailscale.whois(addr).map_err(|e| not_member(e.to_string()))?;
//!     resolved.identity.to_contract().ok_or_else(|| not_member("tagged node or no user"))
//! });
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::team::WhoIs;

use crate::ports::Result;

#[async_trait]
pub trait CallerIdentity: Send + Sync {
    /// The Tailscale user and node behind `addr`. `not_member` when the address
    /// is not a user's node of this tailnet (tagged nodes included); any error
    /// refuses the call.
    async fn identify(&self, addr: SocketAddr) -> Result<WhoIs>;
}

/// Runs a blocking lookup (a process or a LocalAPI round trip) off the async
/// runtime.
pub struct BlockingIdentity<F> {
    lookup: Arc<F>,
}

impl<F> BlockingIdentity<F>
where
    F: Fn(SocketAddr) -> Result<WhoIs> + Send + Sync + 'static,
{
    pub fn new(lookup: F) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }
}

#[async_trait]
impl<F> CallerIdentity for BlockingIdentity<F>
where
    F: Fn(SocketAddr) -> Result<WhoIs> + Send + Sync + 'static,
{
    async fn identify(&self, addr: SocketAddr) -> Result<WhoIs> {
        let lookup = self.lookup.clone();
        tokio::task::spawn_blocking(move || lookup(addr))
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("WhoIs lookup failed: {e}")))?
    }
}

/// The error for a caller who is not a member, or cannot be identified.
pub fn not_member(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::NotMember, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_blocking_lookup_answers_through_the_trait() {
        let identity = BlockingIdentity::new(|addr: SocketAddr| {
            if addr.ip().is_loopback() {
                Ok(WhoIs {
                    login: "minsu@github".into(),
                    node: "nStable1".into(),
                })
            } else {
                Err(not_member("not a tailnet address"))
            }
        });
        let caller: &dyn CallerIdentity = &identity;
        let whois = caller.identify("127.0.0.1:5000".parse().unwrap()).await;
        assert_eq!(whois.unwrap().login, "minsu@github");
        let refused = caller.identify("192.168.1.2:5000".parse().unwrap()).await;
        assert_eq!(refused.unwrap_err().code, ErrorCode::NotMember);
    }
}
