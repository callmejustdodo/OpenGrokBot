//! The TASK-007 spike server: listens only on this Mac's Tailscale IPv4 address
//! and answers `GET /spike/whoami` with the caller's Tailscale identity.
//!
//! The caller is identified only by the TCP peer address of the connection,
//! resolved with WhoIs; request headers are never consulted. Anything short of
//! a verified user (an address outside the tailnet, a failed WhoIs, a tagged
//! node, a node without a login) gets `403` with the contract's error body,
//! sent at once without waiting for the request. A verified user gets `404`
//! for every other method and path: the spike serves none of the contract's
//! `/v1` endpoints, so a client built against the contract cannot mistake one
//! of its answers for a real one.
//!
//! Every connection is bounded in time. The request head must arrive within
//! [`HEAD_TIMEOUT`]. After the response, what the client still sends is read
//! and discarded for at most [`DRAIN_TIMEOUT`], so that closing the socket does
//! not reset the connection before the client has read the answer. Every
//! accepted connection gets one log line, including one closed because
//! [`MAX_CONNECTIONS`] are already open.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::json;

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::peer::PeerErrorBody;

use super::http::{self, HeadError, RequestHead};
use super::identity::{DenyReason, PeerIdentity, Resolved, WhoIsResolver, WhoIsSource};

/// The one path the spike answers. It is outside the contract's `/v1` paths on
/// purpose.
pub const WHOAMI_PATH: &str = "/spike/whoami";

/// Connections served at once; more are logged and closed without a response.
pub const MAX_CONNECTIONS: usize = 32;
/// The whole request head must arrive within this time.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the server keeps reading, and discarding, what the client sends
/// after the response.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// Most bytes discarded after the response. A larger request body may still be
/// cut off with a reset.
const DRAIN_LIMIT: u64 = 4 * 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether `ip` is in Tailscale's IPv4 range, 100.64.0.0/10. The range is
/// shared address space (RFC 6598), so a carrier network can use it too; the
/// caller should also check that the address is the one Tailscale reports.
pub fn is_tailscale_ipv4(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && (second & 0b1100_0000) == 0b0100_0000
}

/// Whether `ip` is a Tailscale address: 100.64.0.0/10 (also as an
/// IPv4-mapped IPv6 address) or the tailnet's IPv6 range fd7a:115c:a1e0::/48.
pub fn is_tailscale_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_tailscale_ipv4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => is_tailscale_ipv4(ip),
            None => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        },
    }
}

/// Binds `ip:port`, refusing any address outside 100.64.0.0/10 (including
/// `0.0.0.0` and loopback) before touching the network.
pub fn bind(ip: Ipv4Addr, port: u16) -> io::Result<TcpListener> {
    if !is_tailscale_ipv4(ip) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to bind {ip}: not a Tailscale address (100.64.0.0/10)"),
        ));
    }
    TcpListener::bind(SocketAddrV4::new(ip, port))
}

/// One line of the server log per connection.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionLog {
    /// Milliseconds since the Unix epoch.
    pub t_ms: u64,
    pub remote: String,
    pub local: String,
    pub method: Option<String>,
    pub path: Option<String>,
    /// `0` when the connection was closed without a response.
    pub status: u16,
    /// Why the caller was refused or the request failed (log only; the caller
    /// sees a shorter message).
    pub reason: Option<String>,
    pub via: Option<WhoIsSource>,
    pub whois_ms: Option<f64>,
    pub login: Option<String>,
    pub node: Option<String>,
    pub node_stable_id: Option<String>,
    pub tags: Vec<String>,
    /// The caller is this Mac (connected to its own Tailscale address).
    pub self_connection: bool,
}

impl ConnectionLog {
    fn new(peer: SocketAddr, local: SocketAddr) -> Self {
        Self {
            t_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_millis() as u64),
            remote: peer.to_string(),
            local: local.to_string(),
            method: None,
            path: None,
            status: 0,
            reason: None,
            via: None,
            whois_ms: None,
            login: None,
            node: None,
            node_stable_id: None,
            tags: Vec::new(),
            self_connection: peer.ip() == local.ip(),
        }
    }
}

/// Receives every log line. It should not panic: a panic while logging a
/// connection closed for capacity stops [`serve`].
pub type Logger = dyn Fn(&ConnectionLog) + Send + Sync;

/// A connection as the server uses it: besides reading and writing, it can
/// bound the next read and end its side of the exchange. [`TcpStream`] is the
/// real one; tests use an in-memory exchange.
pub trait Connection: Read + Write {
    /// Makes the next read fail after `timeout`, which is never zero.
    fn limit_next_read(&mut self, timeout: Duration) -> io::Result<()>;
    /// Ends the response, so the client sees the end of the stream.
    fn shutdown_write(&mut self) -> io::Result<()>;
}

impl Connection for TcpStream {
    fn limit_next_read(&mut self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }

    fn shutdown_write(&mut self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

/// Reads from a connection until `deadline`; any read after it fails with
/// `TimedOut`, however the client spaces its bytes.
struct Until<'a, C> {
    conn: &'a mut C,
    deadline: Instant,
}

impl<'a, C: Connection> Until<'a, C> {
    fn new(conn: &'a mut C, within: Duration) -> Self {
        Self {
            conn,
            deadline: Instant::now() + within,
        }
    }
}

impl<C: Connection> Read for Until<'_, C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let timed_out = || io::Error::new(io::ErrorKind::TimedOut, "timed out");
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timed_out());
        }
        self.conn.limit_next_read(remaining)?;
        self.conn.read(buf).map_err(|error| match error.kind() {
            // A socket read timeout surfaces as EAGAIN.
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => timed_out(),
            _ => error,
        })
    }
}

/// One of the [`MAX_CONNECTIONS`] slots. Dropping it gives the slot back, also
/// when the connection's thread panics.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(active: &Arc<AtomicUsize>) -> Option<Self> {
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(active.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accepts connections until accepting fails, one thread each, and logs every
/// one.
pub fn serve(
    listener: TcpListener,
    resolver: Arc<dyn WhoIsResolver>,
    log: Arc<Logger>,
) -> io::Result<()> {
    let local = listener.local_addr()?;
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        // accept(2) returns the peer address; asking the socket for it later
        // fails once the client has reset the connection.
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some(slot) = Slot::take(&active) else {
            let _ = stream.shutdown(Shutdown::Both);
            let mut entry = ConnectionLog::new(peer, local);
            entry.reason = Some(format!(
                "over capacity: {MAX_CONNECTIONS} connections open; closed without a response"
            ));
            log(&entry);
            continue;
        };
        let (thread_resolver, thread_log) = (resolver.clone(), log.clone());
        let spawned = thread::Builder::new()
            .name("peer-connection".into())
            .spawn(move || {
                let _slot = slot;
                let entry = serve_stream(stream, peer, local, thread_resolver.as_ref());
                thread_log(&entry);
            });
        if let Err(error) = spawned {
            // The closure, with the stream and the slot, is dropped.
            let mut entry = ConnectionLog::new(peer, local);
            entry.reason = Some(format!(
                "no thread for the connection: {error}; closed without a response"
            ));
            log(&entry);
        }
    }
}

fn serve_stream(
    mut stream: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
    resolver: &dyn WhoIsResolver,
) -> ConnectionLog {
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    handle_connection(&mut stream, peer, local, resolver)
}

/// Serves one connection: WhoIs on `peer`, answer, then drain what the client
/// still sends. A refused caller is answered before its request is read.
pub fn handle_connection<C: Connection>(
    conn: &mut C,
    peer: SocketAddr,
    local: SocketAddr,
    resolver: &dyn WhoIsResolver,
) -> ConnectionLog {
    handle_connection_within(conn, peer, local, resolver, HEAD_TIMEOUT, DRAIN_TIMEOUT)
}

fn handle_connection_within<C: Connection>(
    conn: &mut C,
    peer: SocketAddr,
    local: SocketAddr,
    resolver: &dyn WhoIsResolver,
    head_timeout: Duration,
    drain_timeout: Duration,
) -> ConnectionLog {
    let mut entry = ConnectionLog::new(peer, local);

    // Identity first, from the TCP peer address only.
    let identity = if is_tailscale_ip(peer.ip()) {
        let started = Instant::now();
        let result = resolver.whois(peer);
        entry.whois_ms = Some((started.elapsed().as_secs_f64() * 1e6).round() / 1e3);
        result.map_err(DenyReason::Unverified)
    } else {
        Err(DenyReason::NotTailnetAddress)
    };
    if let Ok(resolved) = &identity {
        entry.via = Some(resolved.source);
        record_identity(&mut entry, &resolved.identity);
    }
    let verified = match &identity {
        Ok(resolved) => resolved.identity.verified_user().map(|_| resolved),
        Err(reason) => Err(reason.clone()),
    };

    let refused = verified.is_err();
    let (status, body, reason) = match verified {
        // Whatever they would send, the answer is the same.
        Err(reason) => forbidden(&reason),
        Ok(resolved) => {
            let head = http::read_request_head(&mut Until::new(conn, head_timeout));
            record_head(&mut entry, &head);
            answer(resolved, &head, entry.self_connection)
        }
    };
    entry.status = status;
    entry.reason = reason;
    let body = serde_json::to_vec(&body).unwrap_or_default();
    if let Err(error) = http::write_json_response(conn, status, &body) {
        let note = format!("response not sent: {error}");
        entry.reason = Some(match entry.reason.take() {
            Some(reason) => format!("{reason}; {note}"),
            None => note,
        });
    }
    let _ = conn.shutdown_write();

    // Closing with unread bytes would reset the connection, and the client
    // could lose the answer. A refused caller's request head is read here, for
    // the log only.
    let mut rest = Until::new(conn, drain_timeout).take(DRAIN_LIMIT);
    if refused {
        record_head(&mut entry, &http::read_request_head(&mut rest));
    }
    let _ = io::copy(&mut rest, &mut io::sink());
    entry
}

fn record_identity(entry: &mut ConnectionLog, identity: &PeerIdentity) {
    entry.login = identity.login().map(str::to_owned);
    entry.node = Some(identity.node.name.clone());
    entry.node_stable_id = Some(identity.node.stable_id.clone());
    entry.tags = identity.node.tags.clone();
}

fn record_head(entry: &mut ConnectionLog, head: &Result<RequestHead, HeadError>) {
    if let Ok(head) = head {
        entry.method = Some(head.method.clone());
        entry.path = Some(head.target.clone());
    }
}

/// The status, JSON body, and log reason for a verified user's request.
fn answer(
    resolved: &Resolved,
    head: &Result<RequestHead, HeadError>,
    self_connection: bool,
) -> (u16, serde_json::Value, Option<String>) {
    let head = match head {
        Ok(head) => head,
        Err(HeadError::Io(error)) if error.kind() == io::ErrorKind::TimedOut => {
            let body = error_body(
                ErrorCode::InvalidParams,
                "the request did not arrive in time",
            );
            return (
                408,
                body,
                Some(format!("no complete request head: {error}")),
            );
        }
        Err(error) => {
            let body = error_body(ErrorCode::InvalidParams, "malformed HTTP request");
            return (400, body, Some(error.to_string()));
        }
    };
    let path = head.target.split('?').next().unwrap_or_default();
    if head.method != "GET" || path != WHOAMI_PATH {
        let body = error_body(
            ErrorCode::NotFound,
            "the TASK-007 spike serves only GET /spike/whoami",
        );
        let reason = format!("not served by the spike: {} {}", head.method, head.target);
        return (404, body, Some(reason));
    }
    let body = json!({
        "caller": resolved.identity,
        "whois": resolved.identity.to_contract(),
        "via": resolved.source,
        "self_connection": self_connection,
    });
    (200, body, None)
}

fn forbidden(reason: &DenyReason) -> (u16, serde_json::Value, Option<String>) {
    let body = error_body(ErrorCode::NotMember, reason.public_message());
    (403, body, Some(reason.to_string()))
}

/// `{"error": {"code", "message"}}`, the contract's [`PeerErrorBody`].
fn error_body(code: ErrorCode, message: &str) -> serde_json::Value {
    let body = PeerErrorBody {
        error: ApiError::new(code, message),
    };
    serde_json::to_value(body).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::{Ipv6Addr, TcpStream};
    use std::sync::Mutex;

    use super::*;
    use crate::peer::identity::fixtures::{WHOIS_TAGGED, WHOIS_USER};
    use crate::peer::identity::{REFUSED_MESSAGE, WhoIsError, parse_whois};

    /// Answers WhoIs from a script and counts the calls.
    struct StubResolver {
        answer: Result<Resolved, WhoIsError>,
        calls: Mutex<Vec<SocketAddr>>,
    }

    impl StubResolver {
        fn new(answer: Result<Resolved, WhoIsError>) -> Self {
            Self {
                answer,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn fixture(json: &str) -> Self {
            Self::new(Ok(Resolved {
                identity: parse_whois(json.as_bytes()).unwrap(),
                source: WhoIsSource::LocalApi,
            }))
        }

        fn calls(&self) -> Vec<SocketAddr> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl WhoIsResolver for StubResolver {
        fn whois(&self, addr: SocketAddr) -> Result<Resolved, WhoIsError> {
            self.calls.lock().unwrap().push(addr);
            self.answer.clone()
        }
    }

    /// A connection in memory: the request in, the response out.
    struct Exchange {
        request: Cursor<Vec<u8>>,
        response: Vec<u8>,
        /// Fail the test if the server reads before it has answered.
        answer_first: bool,
    }

    impl Exchange {
        fn new(request: &[u8]) -> Self {
            Self {
                request: Cursor::new(request.to_vec()),
                response: Vec::new(),
                answer_first: false,
            }
        }
    }

    impl Read for Exchange {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            assert!(
                !self.answer_first || !self.response.is_empty(),
                "read the request before answering"
            );
            self.request.read(buf)
        }
    }

    impl Write for Exchange {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.response.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Connection for Exchange {
        fn limit_next_read(&mut self, _timeout: Duration) -> io::Result<()> {
            Ok(())
        }
        fn shutdown_write(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    const CALLER: &str = "100.101.102.103:52000";
    const LOCAL: &str = "100.101.102.104:7413";
    const GET: &[u8] = b"GET /spike/whoami HTTP/1.1\r\nHost: 100.101.102.104:7413\r\n\r\n";

    fn serve_exchange(
        resolver: &StubResolver,
        peer: &str,
        exchange: &mut Exchange,
    ) -> (ConnectionLog, http::Response) {
        let log = handle_connection(
            exchange,
            peer.parse().unwrap(),
            LOCAL.parse().unwrap(),
            resolver,
        );
        let response = http::parse_response(&exchange.response).unwrap();
        (log, response)
    }

    fn serve_one(
        resolver: &StubResolver,
        peer: &str,
        request: &[u8],
    ) -> (ConnectionLog, http::Response) {
        serve_exchange(resolver, peer, &mut Exchange::new(request))
    }

    fn body(response: &http::Response) -> serde_json::Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    fn error_shape(code: &str, message: &str) -> serde_json::Value {
        json!({"error": {"code": code, "message": message}})
    }

    type Logs = Arc<Mutex<Vec<ConnectionLog>>>;

    /// [`serve`] on a loopback port, logging into a list.
    fn start_server(resolver: Arc<dyn WhoIsResolver>) -> (SocketAddr, Logs) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let logs = Logs::default();
        let sink = logs.clone();
        thread::spawn(move || {
            serve(
                listener,
                resolver,
                Arc::new(move |entry: &ConnectionLog| sink.lock().unwrap().push(entry.clone())),
            )
        });
        (addr, logs)
    }

    fn wait_for_logs(logs: &Logs, count: usize, matching: impl Fn(&ConnectionLog) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while logs.lock().unwrap().iter().filter(|e| matching(e)).count() < count {
            assert!(Instant::now() < deadline, "{:#?}", logs.lock().unwrap());
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_tailscale_range_is_100_64_slash_10() {
        for (ip, inside) in [
            ("100.64.0.0", true),
            ("100.101.102.103", true),
            ("100.127.255.255", true),
            ("100.63.255.255", false),
            ("100.128.0.0", false),
            ("100.100.100.100", true),
            ("127.0.0.1", false),
            ("0.0.0.0", false),
            ("192.168.1.20", false),
            ("10.64.0.1", false),
        ] {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert_eq!(is_tailscale_ipv4(ip), inside, "{ip}");
            assert_eq!(is_tailscale_ip(IpAddr::V4(ip)), inside, "{ip}");
            assert_eq!(
                is_tailscale_ip(IpAddr::V6(ip.to_ipv6_mapped())),
                inside,
                "mapped {ip}"
            );
        }
        for (ip, inside) in [
            ("fd7a:115c:a1e0::1234:5678", true),
            ("fd7a:115c:a1e1::1", false),
            ("::1", false),
            ("fe80::1", false),
        ] {
            let ip: Ipv6Addr = ip.parse().unwrap();
            assert_eq!(is_tailscale_ip(IpAddr::V6(ip)), inside, "{ip}");
        }
    }

    #[test]
    fn refuses_to_bind_outside_the_tailnet() {
        for ip in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(100, 128, 0, 1),
        ] {
            let error = bind(ip, 0).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{ip}");
            assert!(error.to_string().contains("refusing to bind"), "{error}");
        }
    }

    #[test]
    fn a_verified_user_gets_200_with_their_identity() {
        let resolver = StubResolver::fixture(WHOIS_USER);
        let request = b"GET /spike/whoami HTTP/1.1\r\nHost: x\r\nX-Tailscale-User: mallory@example.com\r\nX-Forwarded-For: 100.64.0.9\r\n\r\n";
        let (log, response) = serve_one(&resolver, CALLER, request);

        assert_eq!(response.status, 200);
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.header("connection"), Some("close"));
        assert_eq!(
            body(&response),
            json!({
                "caller": {
                    "node": {
                        "stable_id": "nEXAMPLE0001CNTRL",
                        "name": "alice-mbp.tail0000.ts.net",
                        "short_name": "alice-mbp",
                        "tags": [],
                    },
                    "user": {
                        "id": 2000000000000002_i64,
                        "login": "alice@example.com",
                        "display_name": "Alice Example",
                    },
                },
                "whois": {"login": "alice@example.com", "node": "nEXAMPLE0001CNTRL"},
                "via": "localapi",
                "self_connection": false,
            })
        );
        // WhoIs ran on the TCP peer address, not on anything in the headers.
        assert_eq!(resolver.calls(), vec![CALLER.parse().unwrap()]);

        assert_eq!(log.status, 200);
        assert_eq!(log.remote, CALLER);
        assert_eq!(log.local, LOCAL);
        assert_eq!(log.login.as_deref(), Some("alice@example.com"));
        assert_eq!(log.node.as_deref(), Some("alice-mbp.tail0000.ts.net"));
        assert_eq!(log.node_stable_id.as_deref(), Some("nEXAMPLE0001CNTRL"));
        assert_eq!(log.method.as_deref(), Some("GET"));
        assert_eq!(log.path.as_deref(), Some(WHOAMI_PATH));
        assert_eq!(log.via, Some(WhoIsSource::LocalApi));
        assert!(log.whois_ms.is_some());
        assert_eq!(log.reason, None);
    }

    #[test]
    fn contract_paths_get_404_not_a_spike_answer() {
        let resolver = StubResolver::fixture(WHOIS_USER);
        for request in [
            &b"GET /v1/agents HTTP/1.1\r\n\r\n"[..],
            b"POST /v1/team/join HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}",
            b"POST /spike/whoami HTTP/1.1\r\n\r\n",
            b"GET /spike/whoami/extra HTTP/1.1\r\n\r\n",
        ] {
            let (log, response) = serve_one(&resolver, CALLER, request);
            let request = String::from_utf8_lossy(request);
            assert_eq!(response.status, 404, "{request}");
            assert_eq!(body(&response)["error"]["code"], "not_found", "{request}");
            assert!(log.reason.unwrap().contains("not served"), "{request}");
        }
        let (_, response) = serve_one(
            &resolver,
            CALLER,
            b"GET /spike/whoami?verbose=1 HTTP/1.1\r\n\r\n",
        );
        assert_eq!(response.status, 200);
    }

    #[test]
    fn a_failed_whois_gets_403_with_the_contract_error_shape() {
        let resolver = StubResolver::new(Err(WhoIsError::Failed(
            "LocalAPI 127.0.0.1:49159: connection refused".into(),
        )));
        let (log, response) = serve_one(&resolver, CALLER, GET);
        assert_eq!(response.status, 403);
        assert_eq!(body(&response), error_shape("not_member", REFUSED_MESSAGE));
        assert_eq!(log.status, 403);
        assert_eq!(log.login, None);
        assert!(
            log.reason
                .as_deref()
                .unwrap()
                .contains("connection refused")
        );
        // The detail stays in the log.
        assert!(!String::from_utf8_lossy(&response.body).contains("49159"));
    }

    #[test]
    fn an_unknown_peer_gets_403() {
        let resolver = StubResolver::new(Err(WhoIsError::NotFound("no match for IP:port".into())));
        let (log, response) = serve_one(&resolver, CALLER, GET);
        assert_eq!(response.status, 403);
        assert_eq!(body(&response)["error"]["code"], "not_member");
        assert!(log.reason.unwrap().contains("peer not found"));
    }

    #[test]
    fn a_tagged_node_gets_403() {
        let resolver = StubResolver::fixture(WHOIS_TAGGED);
        let (log, response) = serve_one(&resolver, CALLER, GET);
        assert_eq!(response.status, 403);
        assert_eq!(body(&response), error_shape("not_member", REFUSED_MESSAGE));
        assert_eq!(log.login, None);
        assert_eq!(log.node.as_deref(), Some("build-server.tail0000.ts.net"));
        assert_eq!(log.tags, vec!["tag:server".to_owned()]);
        assert!(log.reason.unwrap().contains("tagged node"));
    }

    #[test]
    fn an_address_outside_the_tailnet_gets_403_without_whois() {
        let resolver = StubResolver::fixture(WHOIS_USER);
        for peer in ["127.0.0.1:52000", "192.168.0.10:52000", "[::1]:52000"] {
            let (log, response) = serve_one(&resolver, peer, GET);
            assert_eq!(response.status, 403, "{peer}");
            assert_eq!(body(&response), error_shape("not_member", REFUSED_MESSAGE));
            assert_eq!(log.whois_ms, None);
        }
        assert!(resolver.calls().is_empty());
    }

    #[test]
    fn a_refused_caller_is_answered_before_its_request_is_read() {
        let resolver = StubResolver::new(Err(WhoIsError::NotFound("no match".into())));
        let mut exchange = Exchange::new(GET);
        exchange.answer_first = true;
        let (log, response) = serve_exchange(&resolver, CALLER, &mut exchange);
        assert_eq!(response.status, 403);
        // The head is still read afterwards, for the log.
        assert_eq!(log.method.as_deref(), Some("GET"));
        assert_eq!(log.path.as_deref(), Some(WHOAMI_PATH));

        let (log, response) = serve_one(&resolver, CALLER, b"");
        assert_eq!(response.status, 403);
        assert_eq!(log.method, None);
    }

    #[test]
    fn a_stranger_gets_403_even_for_a_malformed_request() {
        let resolver = StubResolver::new(Err(WhoIsError::NotFound("no match".into())));
        let (_, response) = serve_one(&resolver, CALLER, b"garbage\r\n\r\n");
        assert_eq!(response.status, 403);
    }

    #[test]
    fn a_verified_users_malformed_request_gets_400() {
        let resolver = StubResolver::fixture(WHOIS_USER);
        let (log, response) = serve_one(&resolver, CALLER, b"garbage\r\n\r\n");
        assert_eq!(response.status, 400);
        assert_eq!(
            body(&response),
            error_shape("invalid_params", "malformed HTTP request")
        );
        assert_eq!(log.login.as_deref(), Some("alice@example.com"));
        assert!(log.reason.unwrap().contains("malformed"));
    }

    #[test]
    fn a_connection_to_its_own_address_is_marked() {
        let resolver = StubResolver::fixture(WHOIS_USER);
        let (log, response) = serve_one(&resolver, "100.101.102.104:52000", GET);
        assert!(log.self_connection);
        assert_eq!(body(&response)["self_connection"], true);
    }

    #[test]
    fn the_log_line_is_json() {
        let resolver = StubResolver::fixture(WHOIS_USER);
        let (log, _) = serve_one(&resolver, CALLER, GET);
        let line = serde_json::to_value(&log).unwrap();
        assert_eq!(line["status"], 200);
        assert_eq!(line["via"], "localapi");
        assert_eq!(line["login"], "alice@example.com");
    }

    /// A loopback socket pair: the server's end and the client's.
    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client)
    }

    #[test]
    fn a_request_head_sent_a_byte_at_a_time_times_out() {
        let (mut server, client) = socket_pair();
        let mut writer = client.try_clone().unwrap();
        // Each byte arrives well within any per-read timeout, but the head
        // never ends.
        thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(3) {
                if writer.write_all(b"G").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let resolver = StubResolver::fixture(WHOIS_USER);
        let started = Instant::now();
        let log = handle_connection_within(
            &mut server,
            CALLER.parse().unwrap(),
            LOCAL.parse().unwrap(),
            &resolver,
            Duration::from_millis(300),
            Duration::from_millis(100),
        );
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
        assert_eq!(log.status, 408);
        assert!(log.reason.unwrap().contains("timed out"));
        drop(client);
    }

    #[test]
    fn a_request_body_is_drained_so_the_client_gets_the_answer() {
        let (mut server, mut client) = socket_pair();
        let handle = thread::spawn(move || {
            let resolver = StubResolver::fixture(WHOIS_USER);
            handle_connection(
                &mut server,
                CALLER.parse().unwrap(),
                LOCAL.parse().unwrap(),
                &resolver,
            )
        });
        let body = vec![b'x'; 2 * 1024 * 1024];
        let mut request = format!(
            "POST /v1/ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Without the drain the server closes with the body unread, and this
        // write or the read below fails with a reset.
        client.write_all(&request).unwrap();
        let mut raw = Vec::new();
        client.read_to_end(&mut raw).unwrap();
        assert_eq!(http::parse_response(&raw).unwrap().status, 404);
        drop(client);
        assert_eq!(handle.join().unwrap().status, 404);
    }

    /// The whole server over real TCP on loopback (no Tailscale): a loopback
    /// caller is refused and WhoIs is never asked.
    #[test]
    fn the_server_refuses_a_loopback_caller_over_tcp() {
        let resolver = Arc::new(StubResolver::fixture(WHOIS_USER));
        let (addr, logs) = start_server(resolver.clone());
        let mut client = TcpStream::connect(addr).unwrap();
        let client_addr = client.local_addr().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(GET).unwrap();
        let mut raw = Vec::new();
        client.read_to_end(&mut raw).unwrap();
        drop(client);
        let response = http::parse_response(&raw).unwrap();
        assert_eq!(response.status, 403);
        assert_eq!(body(&response)["error"]["code"], "not_member");
        assert!(resolver.calls().is_empty());

        wait_for_logs(&logs, 1, |_| true);
        let logs = logs.lock().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].status, 403);
        assert_eq!(logs[0].method.as_deref(), Some("GET"));
        assert_eq!(logs[0].remote, client_addr.to_string());
        assert_eq!(logs[0].local, addr.to_string());
    }

    #[test]
    fn a_connection_over_capacity_is_logged() {
        let (addr, logs) = start_server(Arc::new(StubResolver::fixture(WHOIS_USER)));
        // A loopback caller is answered at once, but a client that keeps its
        // side open holds its slot while the server drains.
        let clients: Vec<TcpStream> = (0..=MAX_CONNECTIONS)
            .map(|_| TcpStream::connect(addr).unwrap())
            .collect();
        let over = |entry: &ConnectionLog| {
            entry
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("over capacity"))
        };
        wait_for_logs(&logs, 1, over);
        drop(clients);
        wait_for_logs(&logs, MAX_CONNECTIONS + 1, |_| true);
        let logs = logs.lock().unwrap();
        assert_eq!(logs.iter().filter(|entry| over(entry)).count(), 1);
        let entry = logs.iter().find(|entry| over(entry)).unwrap();
        assert_eq!(entry.status, 0);
        assert_eq!(entry.local, addr.to_string());
        assert_eq!(
            logs.iter().filter(|entry| entry.status == 403).count(),
            MAX_CONNECTIONS
        );
    }

    #[test]
    fn a_panicking_logger_does_not_use_up_the_slots() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let resolver: Arc<dyn WhoIsResolver> = Arc::new(StubResolver::fixture(WHOIS_USER));
        thread::spawn(move || {
            serve(
                listener,
                resolver,
                Arc::new(|_: &ConnectionLog| panic!("the log is gone")),
            )
        });
        // Without the slot guard, every panic would keep a slot and connection
        // 33 would be closed without an answer.
        // A panicking thread gives its slot back only after unwinding, which can
        // lag behind the response on a busy runner, so a turned-away connection
        // is retried for a while. Without the guard the slots never come back.
        for i in 0..MAX_CONNECTIONS + 2 {
            let mut status = Err("not tried".to_owned());
            for _ in 0..200 {
                let mut client = TcpStream::connect(addr).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                client.write_all(GET).unwrap();
                let mut raw = Vec::new();
                client.read_to_end(&mut raw).unwrap();
                status = http::parse_response(&raw)
                    .map(|response| response.status)
                    .map_err(|error| error.to_string());
                if status.is_ok() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            assert_eq!(status, Ok(403), "connection {i}");
        }
    }

    /// Run with `cargo test -p formation-core -- --ignored` on a Mac signed in
    /// to Tailscale: the real server on this Mac's Tailscale address answers a
    /// call to that address with this Mac's node and is unreachable on loopback.
    #[test]
    #[ignore = "needs Tailscale"]
    fn the_server_answers_on_the_tailscale_address_only() {
        use crate::peer::tailscale::{Tailscale, Via};

        let tailscale = Arc::new(Tailscale::discover(Via::Auto));
        let me = tailscale.self_node().expect("this Mac's Tailscale IPv4");
        let listener = bind(me.ipv4, 0).unwrap();
        let addr = listener.local_addr().unwrap();
        let logs = Logs::default();
        {
            let resolver: Arc<dyn WhoIsResolver> = tailscale.clone();
            let logs = logs.clone();
            thread::spawn(move || {
                serve(
                    listener,
                    resolver,
                    Arc::new(move |entry: &ConnectionLog| logs.lock().unwrap().push(entry.clone())),
                )
            });
        }

        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        client.write_all(GET).unwrap();
        let mut raw = Vec::new();
        client.read_to_end(&mut raw).unwrap();
        let response = http::parse_response(&raw).unwrap();
        assert_eq!(response.status, 200, "{}", response.body_text());
        let body = body(&response);
        assert_eq!(body["self_connection"], true);
        if let Some(dns_name) = &me.dns_name {
            assert_eq!(body["caller"]["node"]["name"], dns_name.as_str());
        }
        assert!(body["caller"]["user"]["login"].is_string());

        let loopback = TcpStream::connect((Ipv4Addr::LOCALHOST, addr.port())).unwrap_err();
        assert_eq!(loopback.kind(), io::ErrorKind::ConnectionRefused);
    }
}
