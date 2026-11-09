//! Who is calling: the typed result of a Tailscale WhoIs lookup, and the
//! fail-closed check the peer server applies to it.

use std::fmt;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use formation_proto::team::WhoIs;

/// The owner name Tailscale reports for tagged nodes (`tailcfg.TaggedDevices`).
/// A tagged node has no user, so this is never a member's login.
pub const TAGGED_DEVICES_LOGIN: &str = "tagged-devices";

/// The caller of a peer connection as Tailscale WhoIs reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerIdentity {
    pub node: TailnetNode,
    /// `None` for a tagged node (tags replace the user, so there is no login to
    /// match) and for a response without a usable user profile.
    pub user: Option<TailnetUser>,
}

/// The Tailscale account that owns the calling node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TailnetUser {
    /// `UserProfile.ID`.
    pub id: i64,
    /// `UserProfile.LoginName`, such as an email address. Every node of the same
    /// account reports the same login.
    pub login: String,
    /// `UserProfile.DisplayName`.
    pub display_name: String,
}

/// The calling machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TailnetNode {
    /// `Node.StableID`. Unlike the name, it survives a rename.
    pub stable_id: String,
    /// `Node.Name` without its trailing dot, such as `alice-mbp.tail0000.ts.net`.
    pub name: String,
    /// `Node.ComputedName`, such as `alice-mbp`.
    pub short_name: String,
    /// `Node.Tags`, such as `tag:server`. Empty for a user's node.
    pub tags: Vec<String>,
}

impl PeerIdentity {
    pub fn is_tagged(&self) -> bool {
        !self.node.tags.is_empty()
    }

    pub fn login(&self) -> Option<&str> {
        self.user.as_ref().map(|user| user.login.as_str())
    }

    /// The caller's user, or why the caller is refused. Fails closed: a tagged
    /// node, or a node without a user login, is never treated as a person.
    pub fn verified_user(&self) -> Result<&TailnetUser, DenyReason> {
        if self.is_tagged() {
            return Err(DenyReason::TaggedNode {
                node: self.node.name.clone(),
                tags: self.node.tags.clone(),
            });
        }
        self.user.as_ref().ok_or_else(|| DenyReason::NoUser {
            node: self.node.name.clone(),
        })
    }

    /// The contract's [`WhoIs`] for a verified user. `node` carries the node's
    /// stable ID rather than its name, because names change on rename; this is
    /// the spike's proposal for TASK-017, which the contract does not settle.
    pub fn to_contract(&self) -> Option<WhoIs> {
        let user = self.verified_user().ok()?;
        Some(WhoIs {
            login: user.login.clone(),
            node: self.node.stable_id.clone(),
        })
    }
}

/// What every refused caller is told, whatever the reason. The reason itself
/// stays in the server's log: telling a caller that WhoIs failed would tell any
/// tailnet device that this Mac's Tailscale is not answering.
pub const REFUSED_MESSAGE: &str = "the caller is not a verified user of this tailnet";

/// Why the peer server refuses a caller. [`fmt::Display`] is the detail for the
/// server's log; [`DenyReason::public_message`] is what the caller sees, the
/// same for every reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// The TCP peer address is outside the tailnet's ranges.
    NotTailnetAddress,
    /// WhoIs failed, so the caller's identity is unknown.
    Unverified(WhoIsError),
    TaggedNode {
        node: String,
        tags: Vec<String>,
    },
    NoUser {
        node: String,
    },
}

impl DenyReason {
    /// [`REFUSED_MESSAGE`] for every reason.
    pub fn public_message(&self) -> &'static str {
        REFUSED_MESSAGE
    }
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTailnetAddress => f.write_str("remote address is not a Tailscale address"),
            Self::Unverified(error) => write!(f, "WhoIs failed: {error}"),
            Self::TaggedNode { node, tags } => {
                write!(
                    f,
                    "tagged node {node} ({}) has no user login",
                    tags.join(", ")
                )
            }
            Self::NoUser { node } => write!(f, "node {node} has no user login"),
        }
    }
}

/// Why a WhoIs lookup gave no identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhoIsError {
    /// Tailscale knows no node at this address.
    NotFound(String),
    /// Neither the LocalAPI nor the CLI was found.
    Unavailable(String),
    /// The LocalAPI or the CLI failed: connection, authentication, timeout, or exit.
    Failed(String),
    /// The answer was not a WhoIs response.
    BadResponse(String),
}

impl fmt::Display for WhoIsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(detail) => write!(f, "peer not found: {detail}"),
            Self::Unavailable(detail) => write!(f, "Tailscale unavailable: {detail}"),
            Self::Failed(detail) => write!(f, "{detail}"),
            Self::BadResponse(detail) => write!(f, "bad WhoIs response: {detail}"),
        }
    }
}

impl std::error::Error for WhoIsError {}

/// Which Tailscale interface answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WhoIsSource {
    #[serde(rename = "localapi")]
    LocalApi,
    #[serde(rename = "cli")]
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub identity: PeerIdentity,
    pub source: WhoIsSource,
}

/// Resolves the TCP peer address of a connection to its Tailscale identity.
/// [`super::tailscale::Tailscale`] is the real one; tests use a stub.
pub trait WhoIsResolver: Send + Sync {
    fn whois(&self, addr: SocketAddr) -> Result<Resolved, WhoIsError>;
}

/// Parses an `apitype.WhoIsResponse` (the same JSON from the LocalAPI and from
/// `tailscale whois --json`). Unknown fields are ignored.
pub fn parse_whois(json: &[u8]) -> Result<PeerIdentity, WhoIsError> {
    let raw: RawWhoIs = serde_json::from_slice(json)
        .map_err(|error| WhoIsError::BadResponse(format!("not WhoIs JSON: {error}")))?;
    let node = raw
        .node
        .ok_or_else(|| WhoIsError::BadResponse("no Node".into()))?;
    let stable_id = node.stable_id.unwrap_or_default();
    if stable_id.is_empty() {
        return Err(WhoIsError::BadResponse("Node has no StableID".into()));
    }
    let short_name = node.computed_name.unwrap_or_default();
    let name = match node.name.as_deref().map(|name| name.trim_end_matches('.')) {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => short_name.clone(),
    };
    let tags = node.tags.unwrap_or_default();
    let user = if tags.is_empty() {
        raw.user_profile.and_then(RawUserProfile::into_user)
    } else {
        None
    };
    Ok(PeerIdentity {
        node: TailnetNode {
            stable_id,
            name,
            short_name,
            tags,
        },
        user,
    })
}

#[derive(Deserialize)]
struct RawWhoIs {
    #[serde(rename = "Node")]
    node: Option<RawNode>,
    #[serde(rename = "UserProfile")]
    user_profile: Option<RawUserProfile>,
}

#[derive(Deserialize)]
struct RawNode {
    #[serde(rename = "StableID")]
    stable_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "ComputedName")]
    computed_name: Option<String>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawUserProfile {
    #[serde(rename = "ID")]
    id: Option<i64>,
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
    #[serde(rename = "DisplayName")]
    display_name: Option<String>,
}

impl RawUserProfile {
    fn into_user(self) -> Option<TailnetUser> {
        let login = self.login_name.filter(|login| !login.is_empty())?;
        if login == TAGGED_DEVICES_LOGIN {
            return None;
        }
        Some(TailnetUser {
            id: self.id?,
            login,
            display_name: self.display_name.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    pub const WHOIS_USER: &str = include_str!("testdata/whois-user.json");
    pub const WHOIS_TAGGED: &str = include_str!("testdata/whois-tagged.json");
}

#[cfg(test)]
mod tests {
    use super::fixtures::{WHOIS_TAGGED, WHOIS_USER};
    use super::*;

    #[test]
    fn parses_a_users_node() {
        let identity = parse_whois(WHOIS_USER.as_bytes()).unwrap();
        assert_eq!(
            identity,
            PeerIdentity {
                node: TailnetNode {
                    stable_id: "nEXAMPLE0001CNTRL".into(),
                    name: "alice-mbp.tail0000.ts.net".into(),
                    short_name: "alice-mbp".into(),
                    tags: vec![],
                },
                user: Some(TailnetUser {
                    id: 2000000000000002,
                    login: "alice@example.com".into(),
                    display_name: "Alice Example".into(),
                }),
            }
        );
        assert!(!identity.is_tagged());
        assert_eq!(identity.login(), Some("alice@example.com"));
        assert_eq!(identity.verified_user().unwrap().id, 2000000000000002);
        assert_eq!(
            identity.to_contract(),
            Some(WhoIs {
                login: "alice@example.com".into(),
                node: "nEXAMPLE0001CNTRL".into(),
            })
        );
    }

    #[test]
    fn a_tagged_node_has_no_user_and_is_refused() {
        let identity = parse_whois(WHOIS_TAGGED.as_bytes()).unwrap();
        assert!(identity.is_tagged());
        assert_eq!(identity.node.tags, vec!["tag:server".to_owned()]);
        assert_eq!(identity.user, None);
        assert_eq!(identity.login(), None);
        assert_eq!(identity.to_contract(), None);
        let reason = identity.verified_user().unwrap_err();
        assert_eq!(
            reason,
            DenyReason::TaggedNode {
                node: "build-server.tail0000.ts.net".into(),
                tags: vec!["tag:server".into()],
            }
        );
        assert_eq!(
            reason.to_string(),
            "tagged node build-server.tail0000.ts.net (tag:server) has no user login"
        );
    }

    #[test]
    fn tags_win_over_a_user_profile() {
        // Even if a tagged node came with a real-looking user, it is not a person.
        let json = WHOIS_TAGGED.replace("\"tagged-devices\"", "\"alice@example.com\"");
        let identity = parse_whois(json.as_bytes()).unwrap();
        assert_eq!(identity.user, None);
        assert!(identity.verified_user().is_err());
    }

    #[test]
    fn the_tagged_devices_owner_is_never_a_user() {
        // Tags missing but the placeholder owner present: still no user.
        let json = r#"{"Node":{"StableID":"nX","Name":"x.ts.net.","ComputedName":"x","Tags":null},
            "UserProfile":{"ID":7,"LoginName":"tagged-devices","DisplayName":"Tagged Devices"},"CapMap":null}"#;
        let identity = parse_whois(json.as_bytes()).unwrap();
        assert!(!identity.is_tagged());
        assert_eq!(identity.user, None);
        assert_eq!(
            identity.verified_user().unwrap_err(),
            DenyReason::NoUser {
                node: "x.ts.net".into()
            }
        );
    }

    #[test]
    fn missing_or_empty_user_fields_give_no_user() {
        for profile in [
            r#"null"#,
            r#"{"ID":7,"DisplayName":"No Login"}"#,
            r#"{"ID":7,"LoginName":"","DisplayName":"Empty"}"#,
            r#"{"LoginName":"alice@example.com"}"#,
        ] {
            let json = format!(
                r#"{{"Node":{{"StableID":"nX","Name":"x.ts.net.","ComputedName":"x"}},"UserProfile":{profile}}}"#
            );
            let identity = parse_whois(json.as_bytes()).unwrap();
            assert_eq!(identity.user, None, "{profile}");
        }
    }

    #[test]
    fn rejects_what_is_not_a_whois_response() {
        let cases: [(&[u8], &str); 5] = [
            (
                b"The Tailscale GUI failed to start: (Tailscale.CLIError error 3.)",
                "not WhoIs JSON",
            ),
            (b"", "not WhoIs JSON"),
            (b"{}", "no Node"),
            (b"{\"Node\":{\"Name\":\"x.ts.net.\"}}", "no StableID"),
            (b"{\"Node\":{\"StableID\":\"\"}}", "no StableID"),
        ];
        for (input, expected) in cases {
            match parse_whois(input) {
                Err(WhoIsError::BadResponse(detail)) => {
                    assert!(detail.contains(expected), "{detail}")
                }
                other => panic!("{:?}: {other:?}", String::from_utf8_lossy(input)),
            }
        }
    }

    #[test]
    fn a_missing_name_falls_back_to_the_short_name() {
        let json = r#"{"Node":{"StableID":"nX","ComputedName":"x"},"UserProfile":null}"#;
        assert_eq!(parse_whois(json.as_bytes()).unwrap().node.name, "x");
    }

    #[test]
    fn public_messages_do_not_leak_details() {
        let reason = DenyReason::Unverified(WhoIsError::Failed(
            "LocalAPI 127.0.0.1:49159 returned 403".into(),
        ));
        assert!(!reason.public_message().contains("49159"));
        assert!(reason.to_string().contains("49159"));
    }

    #[test]
    fn every_refusal_tells_the_caller_the_same() {
        for reason in [
            DenyReason::NotTailnetAddress,
            DenyReason::Unverified(WhoIsError::Unavailable("no LocalAPI".into())),
            DenyReason::TaggedNode {
                node: "x".into(),
                tags: vec!["tag:server".into()],
            },
            DenyReason::NoUser { node: "x".into() },
        ] {
            assert_eq!(reason.public_message(), REFUSED_MESSAGE, "{reason}");
        }
    }
}
