//! Invitation links: `formation://join/<code>?host=<host>&port=<port>&team=<team id>`.
//!
//! The link carries everything the invitee's formationd needs to reach the creator's
//! Mac over the tailnet: the creator's MagicDNS name or tailnet IP, the peer port,
//! the team, and the code (tech spec, "Team and roster"). The code stays in the path,
//! where the mockup shows it.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::team::TeamId;

use super::code;
use crate::ports::Result;

pub const SCHEME_AND_PATH: &str = "formation://join/";

/// A parsed invitation link. Parsing validates every part, so its `Display` (the
/// link itself) needs no escaping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationLink {
    /// Canonical, such as `7F3K-Q9PX`.
    pub code: String,
    /// A MagicDNS name (lowercase, no trailing dot) or a tailnet IP.
    pub host: String,
    pub port: u16,
    pub team: TeamId,
}

impl InvitationLink {
    /// Builds a link from parts that may come from outside (the host from Tailscale),
    /// so they are checked the same way as a pasted link.
    pub fn new(code: &str, host: &str, port: u16, team: TeamId) -> Result<Self> {
        Ok(Self {
            code: code::normalize(code).ok_or_else(|| invalid("not an invitation code"))?,
            host: parse_host(host)?,
            port: Some(port)
                .filter(|&port| port != 0)
                .ok_or_else(|| invalid("the peer port is not valid"))?,
            team,
        })
    }

    /// Reads a pasted link. Surrounding spaces, letter case in the scheme, and
    /// percent-encoding are tolerated; unknown query parameters are ignored, so a
    /// later version can add some. Every failure is `invalid_params`.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let rest = text
            .get(..SCHEME_AND_PATH.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(SCHEME_AND_PATH))
            .map(|_| &text[SCHEME_AND_PATH.len()..])
            .ok_or_else(|| invalid("not a Formation invitation link"))?;
        let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
        let (path, query) = rest.split_once('?').ok_or_else(|| {
            invalid("the link has no address of the team creator's Mac; ask for a new link")
        })?;
        let code = code::normalize(&decode(path.trim_end_matches('/'))?)
            .ok_or_else(|| invalid("the link's invitation code is not valid"))?;

        let (mut host, mut port, mut team) = (None, None, None);
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let slot = match key {
                "host" => &mut host,
                "port" => &mut port,
                "team" => &mut team,
                _ => continue,
            };
            if slot.replace(decode(value)?).is_some() {
                return Err(invalid(format!("the link names `{key}` twice")));
            }
        }
        let missing = |key: &str| invalid(format!("the link has no `{key}`"));
        let team = team.ok_or_else(|| missing("team"))?;
        Ok(Self {
            code,
            host: parse_host(&host.ok_or_else(|| missing("host"))?)?,
            port: parse_port(&port.ok_or_else(|| missing("port"))?)?,
            team: team
                .parse()
                .map_err(|_| invalid("the link's team is not valid"))?,
        })
    }

    /// `host:port` for [`crate::ports::Peers::join`]; IPv6 hosts in brackets.
    pub fn address(&self) -> String {
        address(&self.host, self.port)
    }
}

impl fmt::Display for InvitationLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SCHEME_AND_PATH}{}?host={}&port={}&team={}",
            self.code, self.host, self.port, self.team
        )
    }
}

/// `host:port`, with an IPv6 host in brackets.
pub fn address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidParams, message)
}

/// Percent-decoding for the few characters a chat app might encode. Anything that
/// does not decode to ASCII is refused; valid parts are ASCII only.
fn decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = if bytes[i] == b'%' {
            let hex = value
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| invalid("the link has a broken %-escape"))?;
            i += 3;
            hex
        } else {
            i += 1;
            bytes[i - 1]
        };
        if !byte.is_ascii() || byte.is_ascii_control() {
            return Err(invalid("the link has characters a link cannot have"));
        }
        out.push(char::from(byte));
    }
    Ok(out)
}

/// Tailscale's address ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_tailnet_v4(v4),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            [s[0], s[1], s[2]] == [0xfd7a, 0x115c, 0xa1e0]
        }
    }
}

fn is_tailnet_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

/// A tailnet IP, or a DNS name such as a MagicDNS name (`macmini` or
/// `macmini.tail1ae7fe.ts.net`). An IP outside the tailnet is refused, so a link
/// cannot send the joiner's identity to an arbitrary address; a name is checked
/// for shape only, so the peer client must still connect over the tailnet.
pub(crate) fn parse_host(host: &str) -> Result<String> {
    let host = host.trim();
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return if is_tailnet_ip(ip) {
            Ok(ip.to_string())
        } else {
            Err(invalid("the link's address is not on a tailnet"))
        };
    }
    if bare.contains(':') {
        return Err(invalid("the link's address is not valid"));
    }
    let name = bare.strip_suffix('.').unwrap_or(bare).to_ascii_lowercase();
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let all_digits_and_dots = name.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    if name.is_empty() || name.len() > 253 || all_digits_and_dots || !name.split('.').all(label_ok)
    {
        return Err(invalid("the link's address is not valid"));
    }
    Ok(name)
}

fn parse_port(port: &str) -> Result<u16> {
    port.parse::<u16>()
        .ok()
        .filter(|&port| port != 0)
        .ok_or_else(|| invalid("the link's port is not valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team() -> TeamId {
        TeamId("0196f3b0-0000-7000-8000-000000000001".parse().unwrap())
    }

    #[test]
    fn a_built_link_parses_back() {
        let link =
            InvitationLink::new("7f3k q9px", "MacMini.tail1ae7fe.ts.net.", 7413, team()).unwrap();
        let text = link.to_string();
        assert_eq!(
            text,
            "formation://join/7F3K-Q9PX?host=macmini.tail1ae7fe.ts.net&port=7413\
             &team=0196f3b0-0000-7000-8000-000000000001"
        );
        assert_eq!(InvitationLink::parse(&text).unwrap(), link);
        assert_eq!(link.address(), "macmini.tail1ae7fe.ts.net:7413");
    }

    #[test]
    fn tailnet_ips_work_and_ipv6_gets_brackets() {
        let v4 = InvitationLink::new("7F3K-Q9PX", "100.66.100.62", 7413, team()).unwrap();
        assert_eq!(v4.address(), "100.66.100.62:7413");
        let v6 =
            InvitationLink::new("7F3K-Q9PX", "[fd7a:115c:a1e0::4101:3b8d]", 7413, team()).unwrap();
        assert_eq!(v6.host, "fd7a:115c:a1e0::4101:3b8d");
        assert_eq!(v6.address(), "[fd7a:115c:a1e0::4101:3b8d]:7413");
        assert_eq!(InvitationLink::parse(&v6.to_string()).unwrap(), v6);
    }

    #[test]
    fn pasted_links_are_forgiving() {
        let pasted = "  FORMATION://JOIN/7f3k-q9px/?team=0196f3b0-0000-7000-8000-000000000001\
                      &port=7413&host=fd7a%3A115c%3Aa1e0%3A%3A1&utm=chat#frag \n";
        let link = InvitationLink::parse(pasted).unwrap();
        assert_eq!(link.code, "7F3K-Q9PX");
        assert_eq!(link.host, "fd7a:115c:a1e0::1");
        assert_eq!(link.port, 7413);
        assert_eq!(link.team, team());
    }

    #[test]
    fn bad_links_are_invalid_params() {
        let team = "team=0196f3b0-0000-7000-8000-000000000001";
        for (text, why) in [
            ("7F3K-Q9PX", "a bare code has no address"),
            (
                "formation://join/7F3K-Q9PX",
                "the mockup's short form has no address",
            ),
            (
                "https://join/7F3K-Q9PX?host=a&port=1&team=x",
                "another scheme",
            ),
            (
                "formation://join/NOPE?host=macmini&port=7413&team=x",
                "bad code",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?port=7413&{team}"),
                "no host",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=macmini&{team}"),
                "no port",
            ),
            (
                "formation://join/7F3K-Q9PX?host=macmini&port=7413",
                "no team",
            ),
            (
                "formation://join/7F3K-Q9PX?host=macmini&port=7413&team=x",
                "bad team",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=macmini&port=0&{team}"),
                "port 0",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=macmini&port=70000&{team}"),
                "port",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=8.8.8.8&port=7413&{team}"),
                "public IP",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=::1&port=7413&{team}"),
                "loopback",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=a_b&port=7413&{team}"),
                "bad name",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=a%2Fb&port=7413&{team}"),
                "slash",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=a&host=b&port=7413&{team}"),
                "twice",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=a%0A&port=7413&{team}"),
                "control",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=a%2&port=7413&{team}"),
                "escape",
            ),
            (
                &format!("formation://join/7F3K-Q9PX?host=1.2.3&port=7413&{team}"),
                "digits",
            ),
        ] {
            let error = InvitationLink::parse(text).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidParams, "{why}: {text}");
        }
    }

    #[test]
    fn tailnet_ranges() {
        for ip in ["100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"] {
            assert!(is_tailnet_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "100.63.255.255",
            "100.128.0.1",
            "127.0.0.1",
            "fd7a:115c:a1e1::1",
        ] {
            assert!(!is_tailnet_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
