//! Who a peer caller is: Tailscale WhoIs mapped to a member (D-046).
//!
//! The peer server resolves WhoIs on every connection and hands the result here as
//! a [`WhoIs`], or as `None` when there is no user identity (a tagged node, or a
//! failed lookup). A caller is a member only when both the recorded Tailscale login
//! and the recorded node (`Node.StableID`) match, and the member is not removed.

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::team::{Member, WhoIs};

use crate::ports::{Result, TeamRepo};

/// The owner name Tailscale reports for tagged nodes. A tagged node has no user, so
/// this is never a member's login. (TASK-007's `peer::identity` maps tagged nodes to
/// no `WhoIs` at all; this is the second line of defense.)
pub const TAGGED_DEVICES_LOGIN: &str = "tagged-devices";

/// Longest display name kept; longer names are cut.
pub const DISPLAY_NAME_MAX_CHARS: usize = 64;

pub(crate) fn not_member() -> ApiError {
    ApiError::new(
        ErrorCode::NotMember,
        "the caller is not a member of this team",
    )
}

/// The caller as a Tailscale user, or `not_member` for a caller without one.
pub fn person(caller: Option<&WhoIs>) -> Result<&WhoIs> {
    caller
        .filter(|whois| {
            !whois.login.trim().is_empty()
                && !whois.node.trim().is_empty()
                && whois.login != TAGGED_DEVICES_LOGIN
        })
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotMember,
                "the caller has no Tailscale user (tagged or unknown node)",
            )
        })
}

/// Maps a peer caller to a current member. `not_member` for a tagged or unknown
/// node, a login no current member has (removed members included), or a member's
/// login from a node other than the recorded one.
pub fn authenticate<S: TeamRepo + ?Sized>(store: &S, caller: Option<&WhoIs>) -> Result<Member> {
    let whois = person(caller)?;
    store
        .member_by_tailscale(&whois.login)?
        .filter(|member| member.tailscale_node == whois.node)
        .ok_or_else(not_member)
}

/// A GitHub login: 1 to 39 ASCII letters, digits, or hyphens, not starting with a
/// hyphen. The MVP takes it from `gh api user` on the member's own Mac, so the
/// creator's Mac can check only its shape.
pub fn github_login(login: &str) -> Result<String> {
    let login = login.trim();
    let valid = (1..=39).contains(&login.len())
        && !login.starts_with('-')
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if valid {
        Ok(login.to_owned())
    } else {
        Err(ApiError::new(
            ErrorCode::InvalidParams,
            "not a GitHub login",
        ))
    }
}

/// The name shown for a member: trimmed, without control characters, at most
/// [`DISPLAY_NAME_MAX_CHARS`]; the GitHub login when nothing is left.
pub fn display_name(name: &str, github_login: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(DISPLAY_NAME_MAX_CHARS)
        .collect();
    let name = name.trim_end();
    if name.is_empty() {
        github_login.to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_logins() {
        for ok in ["baekdusan", "a", "dohyeon-k", " Minsu99 ", &"a".repeat(39)] {
            assert!(github_login(ok).is_ok(), "{ok:?}");
        }
        assert_eq!(github_login(" Minsu99 ").unwrap(), "Minsu99");
        for bad in ["", "-x", "a b", "a_b", "한글", &"a".repeat(40), "a@b"] {
            assert_eq!(
                github_login(bad).unwrap_err().code,
                ErrorCode::InvalidParams,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("  Minsu Park ", "minsu"), "Minsu Park");
        assert_eq!(display_name(" \n\t ", "minsu"), "minsu");
        assert_eq!(display_name("Min\u{7}su", "minsu"), "Minsu");
        assert_eq!(display_name("박민수", "minsu"), "박민수");
        assert_eq!(
            display_name(&"가".repeat(100), "minsu").chars().count(),
            DISPLAY_NAME_MAX_CHARS
        );
    }

    #[test]
    fn callers_without_a_user_are_not_people() {
        let whois = |login: &str, node: &str| WhoIs {
            login: login.into(),
            node: node.into(),
        };
        assert!(person(None).is_err());
        assert!(person(Some(&whois(TAGGED_DEVICES_LOGIN, "nX"))).is_err());
        assert!(person(Some(&whois("", "nX"))).is_err());
        assert!(person(Some(&whois("minsu@example.com", " "))).is_err());
        assert!(person(Some(&whois("minsu@example.com", "nX"))).is_ok());
    }
}
