//! A spike for a second peer transport that needs no Tailscale: iroh, QUIC
//! between endpoints addressed by an ed25519 public key, whose handshake proves
//! the remote key. Nothing in formationd uses it yet. It is built only with the
//! `iroh-spike` feature, and the `peer_iroh_spike` example runs it between two
//! processes or two Macs:
//!
//! ```text
//! cargo test -p formation-core --features iroh-spike peer::iroh_spike
//! cargo clippy -p formation-core --features iroh-spike --all-targets -- -D warnings
//! ```
//!
//! - [`StreamListener`] is an [`axum::serve::Listener`]: every bidirectional
//!   stream accepted on any connection to the endpoint is one HTTP/1.1
//!   connection, and its address is the remote endpoint's key ([`PeerKey`]).
//!   [`serve`] serves an unchanged [`Router`] over it.
//! - [`spike_router`] answers `GET /spike/whoami` with the caller's key and
//!   `GET /spike/events` with a WebSocket that sends [`EVENTS`] and closes. A
//!   caller outside the allow-list gets `403` from both before any handler
//!   runs.
//! - [`get`] and [`events`] are the asker's side: a fresh stream per request on
//!   one shared connection, through hyper's HTTP/1.1 client and
//!   tokio-tungstenite.
//! - [`load_or_create_secret_key`] creates this Mac's key file with mode 0600,
//!   so the endpoint id survives restarts. It refuses an existing file unless
//!   it is a regular file owned by this user, with no group or other
//!   permission bits, holding exactly 32 bytes.
//!
//! What the spike established (iroh 1.4.1 with only its `tls-ring` feature,
//! axum 0.8.9, hyper 1.12.0, tokio-tungstenite 0.29.0). The tests run
//! endpoints in one process over loopback with no relay and no address lookup.
//! The example ran by hand as two processes on one Mac on 2026-11-20, through
//! n0's public relays and address lookup. The two-Mac run is still to do.
//!
//! - The feature exists because of Cargo's feature unification: with iroh in
//!   the build, reqwest's `rustls-no-provider` feature is on (iroh, iroh-relay
//!   and n0-dns-resolver enable it), and every `reqwest::Client` then needs a
//!   process-wide rustls crypto provider. Observed on 2026-11-20 with reqwest
//!   0.13.5: six `peer::loopback_test` tests panicked in
//!   `reqwest::ClientBuilder::build`, and `PeerClient::new` would panic the
//!   same way in formationd. Hence the narrow test filter above. Before this
//!   transport ships, the process must install a crypto provider once, or the
//!   tailnet client must move off reqwest.
//! - iroh's `RecvStream` and `SendStream` implement tokio's `AsyncRead` and
//!   `AsyncWrite`, so `tokio::io::join(recv, send)` is the listener's `Io`.
//!   axum's server (the WebSocket upgrade included), hyper's HTTP/1.1 client
//!   and tokio-tungstenite's `client_async` ran over it unmodified.
//! - A handler reads the caller's key as `ConnectInfo<PeerKey>`, taken from
//!   `Connection::remote_id()`; no socket address is involved. The orphan rule
//!   refuses axum's `Connected` for iroh's `EndpointId`, hence the newtype.
//! - axum's `Listener::accept` cannot report that the listener has ended. Once
//!   the endpoint closes, the listener waits forever and
//!   `with_graceful_shutdown(endpoint.closed())` ends [`serve`]; in every test
//!   the serve task returned after the endpoints closed.
//! - The allow-list is a middleware in front of the routes: an unlisted caller
//!   gets `403` on both paths, the WebSocket one before its upgrade. Any key can
//!   still complete the QUIC handshake and open streams.
//! - Three requests in sequence and three at once on one connection, each on a
//!   fresh stream, all succeeded. Two callers' requests interleaved on one
//!   server each carried their own connection's key. A handshake that failed on
//!   its ALPN did not stop the server from accepting the next connection.
//! - In three manual runs the caller connected through the relay in 0.8 to
//!   1.7 s, got `200` with its own key and the whole WebSocket sequence, and
//!   the connection then selected an IP path. On one Mac that path is a local
//!   one (the LAN address in one run, the Tailscale interface's 100.x address
//!   in another), so the runs show nothing about hole punching. Restarting with
//!   the same key files kept both endpoint ids.
//!
//! Left open:
//!
//! - The run between two Macs on different networks with Tailscale off: how
//!   often hole punching succeeds, and the relay-only fallback.
//! - Whose relays and address lookup production uses. With `presets::N0` an
//!   endpoint publishes a record signed with its key to n0's pkarr server
//!   (`https://dns.iroh.link/pkarr`), and the record holds only its home relay
//!   URL: `PkarrPublisher` filters with `AddrFilter::relay_only` unless told
//!   otherwise (iroh 1.4.1, `src/address_lookup/pkarr.rs`). Direct addresses
//!   are not published, but anyone who knows the id learns its relay.
//! - Whether iroh's `portmapper` or `fast-apple-datapath` features change hole
//!   punching or throughput.
//! - What must change before this serves real routes:
//!   - Membership is checked once per connection, in the accept loop or with
//!     iroh's `EndpointHooks::after_handshake`, so strangers hold no streams.
//!     A route added to the router after the allow-list layer is not covered
//!     by it, another reason to check at the connection.
//!   - Limits on connections, per key and in all, and on streams per
//!     connection; and a header-read timeout: axum's `serve` sets no timer, so
//!     hyper drops its default 30 s one.
//!   - Whether 0-RTT is refused: iroh's server configuration enables early
//!     data.
//!   - A removed member's open connections are closed.
//!   - The key file: two processes creating it at once race (`create_new`
//!     makes the second fail instead of loading the first's key), and a crash
//!     during the write leaves a short file that loading then refuses. Where
//!     the key lives (a file today, perhaps the Keychain).
//!   - How [`PeerApi`](super::api::PeerApi) serves its routes through [`serve`]
//!     with the key mapped to a member, and how
//!     [`PeerClient`](super::client::PeerClient) keeps one connection per
//!     teammate for its requests and events.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::connect_info::Connected;
use axum::extract::ws::{self, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get as get_route;
use axum::serve::{IncomingStream, Listener};
use axum::{Json, Router};
use futures_util::StreamExt;
use hyper_util::rt::TokioIo;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointId, SecretKey};
use serde_json::json;
use tokio::io::Join;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

/// The ALPN both sides offer. It names the spike so that no real peer protocol
/// version can be mistaken for it.
pub const ALPN: &[u8] = b"formation-peer-spike/0";
pub const WHOAMI_PATH: &str = "/spike/whoami";
pub const EVENTS_PATH: &str = "/spike/events";
/// The text messages `GET /spike/events` sends, in order, before it closes.
pub const EVENTS: [&str; 3] = ["first", "second", "third"];

/// Sent as `Host`. Over iroh the QUIC handshake has already named the server,
/// so the value carries no meaning.
const AUTHORITY: &str = "formation-peer";
/// The largest response body [`get`] reads.
const BODY_LIMIT: usize = 1024 * 1024;
/// Streams accepted but not yet taken by axum. Each connection has at most one
/// stream waiting here, and waiting streams are taken in arrival order, so one
/// connection cannot fill the queue. A peer that opens many connections still
/// can, until the connection limits the module documentation leaves open
/// exist.
const PENDING_STREAMS: usize = 16;

/// One HTTP/1.1 connection: a QUIC bidirectional stream.
pub type PeerStream = Join<RecvStream, SendStream>;

/// A stream waiting for axum, with its connection's turn: the connection
/// accepts its next stream only once this one is taken.
type Pending = (PeerStream, PeerKey, OwnedSemaphorePermit);

/// An endpoint's public key. For an accepted stream it is the remote endpoint's
/// key, proven by the QUIC handshake; handlers read it as
/// `ConnectInfo<PeerKey>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeerKey(pub EndpointId);

impl fmt::Display for PeerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Connected<IncomingStream<'_, StreamListener>> for PeerKey {
    fn connect_info(stream: IncomingStream<'_, StreamListener>) -> Self {
        *stream.remote_addr()
    }
}

/// Accepts connections on an endpoint and hands each bidirectional stream to
/// axum as one connection. Construct it inside the tokio runtime.
#[derive(Debug)]
pub struct StreamListener {
    local: PeerKey,
    streams: mpsc::Receiver<Pending>,
}

impl StreamListener {
    pub fn new(endpoint: Endpoint) -> Self {
        let local = PeerKey(endpoint.id());
        let (sender, streams) = mpsc::channel(PENDING_STREAMS);
        tokio::spawn(accept_connections(endpoint, sender));
        Self { local, streams }
    }
}

impl Listener for StreamListener {
    type Io = PeerStream;
    type Addr = PeerKey;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.streams.recv().await {
            // Dropping the turn lets that connection accept its next stream.
            Some((stream, caller, _turn)) => (stream, caller),
            // The endpoint has closed. axum's accept cannot report an end, so
            // the graceful shutdown in `serve` stops the loop instead.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

/// Ends when the endpoint closes: `accept` then returns `None`, and every
/// connection's `accept_bi` fails.
async fn accept_connections(endpoint: Endpoint, streams: mpsc::Sender<Pending>) {
    while let Some(incoming) = endpoint.accept().await {
        let streams = streams.clone();
        tokio::spawn(async move {
            let Ok(connection) = incoming.await else {
                return;
            };
            let caller = PeerKey(connection.remote_id());
            let turn = Arc::new(Semaphore::new(1));
            loop {
                let Ok(permit) = turn.clone().acquire_owned().await else {
                    return;
                };
                let Ok((send, recv)) = connection.accept_bi().await else {
                    return;
                };
                let stream = tokio::io::join(recv, send);
                if streams.send((stream, caller, permit)).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// Serves `router` over `endpoint` until the endpoint closes, then waits for
/// the connections in progress to end.
pub async fn serve(endpoint: Endpoint, router: Router) -> io::Result<()> {
    let closed = endpoint.closed();
    let app = router.into_make_service_with_connect_info::<PeerKey>();
    axum::serve(StreamListener::new(endpoint), app)
        .with_graceful_shutdown(closed)
        .await
}

/// Who may call the spike routes.
#[derive(Clone, Debug)]
pub enum Allowed {
    Everyone,
    Only(HashSet<EndpointId>),
}

impl Allowed {
    fn admits(&self, caller: PeerKey) -> bool {
        match self {
            Self::Everyone => true,
            Self::Only(keys) => keys.contains(&caller.0),
        }
    }
}

pub fn spike_router(allowed: Allowed) -> Router {
    Router::new()
        .route(WHOAMI_PATH, get_route(whoami))
        .route(EVENTS_PATH, get_route(event_socket))
        .layer(middleware::from_fn_with_state(Arc::new(allowed), admit))
}

async fn admit(
    State(allowed): State<Arc<Allowed>>,
    ConnectInfo(caller): ConnectInfo<PeerKey>,
    request: Request,
    next: Next,
) -> Response {
    if !allowed.admits(caller) {
        let body = json!({"caller": caller.to_string(), "error": "not on the allow-list"});
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }
    next.run(request).await
}

async fn whoami(ConnectInfo(caller): ConnectInfo<PeerKey>) -> Json<serde_json::Value> {
    Json(json!({"caller": caller.to_string()}))
}

async fn event_socket(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(send_events)
}

async fn send_events(mut socket: WebSocket) {
    for event in EVENTS {
        if socket.send(ws::Message::Text(event.into())).await.is_err() {
            return;
        }
    }
    if socket.send(ws::Message::Close(None)).await.is_err() {
        return;
    }
    // Read until the client's close reply, so the stream is not stopped while
    // the client is still writing it.
    while let Some(Ok(_)) = socket.recv().await {}
}

/// `GET path` over a fresh stream on `connection`.
pub async fn get(connection: &Connection, path: &str) -> io::Result<(StatusCode, Bytes)> {
    let (send, recv) = connection.open_bi().await.map_err(io::Error::other)?;
    let io = TokioIo::new(tokio::io::join(recv, send));
    let (mut sender, http) = hyper::client::conn::http1::handshake::<_, Body>(io)
        .await
        .map_err(io::Error::other)?;
    let request = axum::http::Request::get(path)
        .header(header::HOST, AUTHORITY)
        .body(Body::empty())
        .map_err(io::Error::other)?;
    // `sender` moves in, so the connection shuts the stream down once the body
    // is read.
    let exchange = async move {
        let response = sender
            .send_request(request)
            .await
            .map_err(io::Error::other)?;
        let status = response.status();
        let body = axum::body::to_bytes(Body::new(response.into_body()), BODY_LIMIT)
            .await
            .map_err(io::Error::other)?;
        Ok::<_, io::Error>((status, body))
    };
    let (reply, http) = tokio::join!(exchange, http);
    let reply = reply?;
    http.map_err(io::Error::other)?;
    Ok(reply)
}

/// Opens `GET /spike/events` as a WebSocket over a fresh stream on
/// `connection` and returns its text messages once the server closes it.
pub async fn events(connection: &Connection) -> Result<Vec<String>, tungstenite::Error> {
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|error| tungstenite::Error::Io(io::Error::other(error)))?;
    let request = format!("ws://{AUTHORITY}{EVENTS_PATH}").into_client_request()?;
    let (mut socket, _) =
        tokio_tungstenite::client_async(request, tokio::io::join(recv, send)).await?;
    let mut texts = Vec::new();
    while let Some(message) = socket.next().await {
        if let Message::Text(text) = message? {
            texts.push(text.as_str().to_owned());
        }
    }
    Ok(texts)
}

/// Loads the secret key at `path`, or creates it there readable and writable
/// by the owner only (0600). The file holds the key's 32 bytes. An existing
/// file is refused unless it is a regular file owned by this user, with no
/// group or other permission bits, holding exactly 32 bytes.
pub fn load_or_create_secret_key(path: &Path) -> io::Result<SecretKey> {
    // `symlink_metadata` does not follow a link, and checking the type before
    // opening keeps a FIFO from blocking the open.
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => Err(refused(
            path,
            io::ErrorKind::InvalidInput,
            "is not a regular file",
        )),
        Ok(_) => load_secret_key(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(&key.to_bytes())?;
            file.sync_all()?;
            Ok(key)
        }
        Err(error) => Err(error),
    }
}

/// Checks the open file, so the bytes read are the ones that passed.
fn load_secret_key(path: &Path) -> io::Result<SecretKey> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let uid = current_uid()?;
    if !metadata.file_type().is_file() {
        return Err(refused(
            path,
            io::ErrorKind::InvalidInput,
            "is not a regular file",
        ));
    }
    if metadata.uid() != uid {
        let problem = format!(
            "is owned by uid {}, not by this user (uid {uid})",
            metadata.uid()
        );
        return Err(refused(path, io::ErrorKind::PermissionDenied, &problem));
    }
    if metadata.mode() & 0o077 != 0 {
        let problem = format!(
            "has mode {:o}; group and others must have no permissions (use 600)",
            metadata.mode() & 0o777
        );
        return Err(refused(path, io::ErrorKind::PermissionDenied, &problem));
    }
    if metadata.len() != 32 {
        let problem = format!("holds {} bytes, not a 32-byte key", metadata.len());
        return Err(refused(path, io::ErrorKind::InvalidData, &problem));
    }
    let mut bytes = [0; 32];
    file.read_exact(&mut bytes)?;
    Ok(SecretKey::from_bytes(&bytes))
}

fn refused(path: &Path, kind: io::ErrorKind, problem: &str) -> io::Error {
    io::Error::new(
        kind,
        format!("refusing key file {}: it {problem}", path.display()),
    )
}

/// This process's effective user id, read as the owner of a file it has just
/// created: std has no `getuid`, and the spike adds no dependency for one.
fn current_uid() -> io::Result<u32> {
    static PROBES: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let probe = std::env::temp_dir().join(format!(
        ".formation-uid-{}-{nanos}-{}",
        std::process::id(),
        PROBES.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)?;
    let owner = file.metadata().map(|metadata| metadata.uid());
    drop(file);
    fs::remove_file(&probe)?;
    owner
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::Ipv4Addr;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use iroh::endpoint::presets;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::ask::test_support::TempDir;

    const WAIT: Duration = Duration::from_secs(20);

    async fn within<F: Future>(step: &str, future: F) -> F::Output {
        tokio::time::timeout(WAIT, future)
            .await
            .unwrap_or_else(|_| panic!("{step} did not finish within {WAIT:?}"))
    }

    /// An endpoint on loopback only, with no relays and no address lookup, so
    /// the tests never leave this Mac.
    async fn local_endpoint(secret_key: SecretKey, alpns: Vec<Vec<u8>>) -> Endpoint {
        let builder = Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("loopback address")
            .secret_key(secret_key)
            .alpns(alpns);
        within("bind", builder.bind()).await.expect("bind")
    }

    async fn client() -> Endpoint {
        local_endpoint(SecretKey::generate(), Vec::new()).await
    }

    struct Server {
        endpoint: Endpoint,
        task: JoinHandle<io::Result<()>>,
    }

    impl Server {
        async fn start(allowed: Allowed) -> Self {
            let endpoint = local_endpoint(SecretKey::generate(), vec![ALPN.to_vec()]).await;
            let task = tokio::spawn(serve(endpoint.clone(), spike_router(allowed)));
            Self { endpoint, task }
        }

        async fn connect(&self, client: &Endpoint) -> Connection {
            within("connect", client.connect(self.endpoint.addr(), ALPN))
                .await
                .expect("connect")
        }

        /// Closes the clients and the server, and checks that the server task
        /// ends.
        async fn stop(self, clients: impl IntoIterator<Item = Endpoint>) {
            for client in clients {
                within("client close", client.close()).await;
            }
            within("server close", self.endpoint.close()).await;
            within("server task", self.task)
                .await
                .expect("server task panicked")
                .expect("serve");
        }
    }

    fn only(keys: &[EndpointId]) -> Allowed {
        Allowed::Only(keys.iter().copied().collect())
    }

    fn caller_in(body: &[u8]) -> String {
        let value: serde_json::Value = serde_json::from_slice(body).expect("JSON body");
        value["caller"].as_str().expect("caller").to_owned()
    }

    async fn whoami(connection: &Connection) -> (StatusCode, Bytes) {
        within("whoami", get(connection, WHOAMI_PATH))
            .await
            .expect("whoami")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn whoami_returns_the_callers_key() {
        let client = client().await;
        let server = Server::start(only(&[client.id()])).await;
        let connection = server.connect(&client).await;

        let (status, body) = whoami(&connection).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(caller_in(&body), client.id().to_string());
        server.stop([client]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_caller_not_on_the_allow_list_gets_403() {
        let listed = SecretKey::generate().public();
        let client = client().await;
        let server = Server::start(only(&[listed])).await;
        let connection = server.connect(&client).await;

        let (status, body) = whoami(&connection).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(caller_in(&body), client.id().to_string());

        match within("events", events(&connection)).await {
            Err(tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
            other => panic!("expected 403, got {other:?}"),
        }
        server.stop([client]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn interleaved_callers_each_carry_their_own_key() {
        let a = client().await;
        let b = client().await;
        let server = Server::start(only(&[a.id()])).await;
        let (to_a, to_b) = tokio::join!(server.connect(&a), server.connect(&b));

        for _ in 0..3 {
            let ((a_status, a_body), (b_status, b_body)) =
                tokio::join!(whoami(&to_a), whoami(&to_b));
            assert_eq!(a_status, StatusCode::OK);
            assert_eq!(caller_in(&a_body), a.id().to_string());
            assert_eq!(b_status, StatusCode::FORBIDDEN);
            assert_eq!(caller_in(&b_body), b.id().to_string());
        }
        server.stop([a, b]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_handshake_does_not_stop_the_accept_loop() {
        let stranger = client().await;
        let client = client().await;
        let server = Server::start(only(&[client.id()])).await;

        let refused = within(
            "connect with another ALPN",
            stranger.connect(server.endpoint.addr(), b"another-protocol/0"),
        )
        .await;
        assert!(refused.is_err(), "a connection with another ALPN succeeded");

        let connection = server.connect(&client).await;
        let (status, body) = whoami(&connection).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(caller_in(&body), client.id().to_string());
        server.stop([stranger, client]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_in_sequence_and_at_once_share_one_connection() {
        let client = client().await;
        let server = Server::start(only(&[client.id()])).await;
        let connection = server.connect(&client).await;
        let expected = client.id().to_string();

        for _ in 0..3 {
            let (status, body) = whoami(&connection).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(caller_in(&body), expected);
        }
        let (a, b, c) = tokio::join!(
            whoami(&connection),
            whoami(&connection),
            whoami(&connection)
        );
        for (status, body) in [a, b, c] {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(caller_in(&body), expected);
        }
        assert!(connection.close_reason().is_none());
        server.stop([client]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_websocket_delivers_the_whole_sequence() {
        let client = client().await;
        let server = Server::start(only(&[client.id()])).await;
        let connection = server.connect(&client).await;

        let texts = within("events", events(&connection)).await.expect("events");

        assert_eq!(texts, EVENTS);
        let (status, _) = whoami(&connection).await;
        assert_eq!(status, StatusCode::OK);
        server.stop([client]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_key_file_is_private_and_keeps_the_endpoint_id() {
        let dir = TempDir::new("iroh-key");
        let path = dir.path().join("peer.key");

        let created = load_or_create_secret_key(&path).expect("create");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let first = local_endpoint(created, Vec::new()).await;
        let first_id = first.id();
        within("close", first.close()).await;

        let loaded = load_or_create_secret_key(&path).expect("load");
        let second = local_endpoint(loaded, Vec::new()).await;
        assert_eq!(second.id(), first_id);
        within("close", second.close()).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_key_file_gives_another_endpoint_id() {
        let dir = TempDir::new("iroh-key");
        let mine = load_or_create_secret_key(&dir.path().join("mine.key")).expect("mine");
        let theirs = load_or_create_secret_key(&dir.path().join("theirs.key")).expect("theirs");

        let mine = local_endpoint(mine, Vec::new()).await;
        let theirs = local_endpoint(theirs, Vec::new()).await;
        assert_ne!(mine.id(), theirs.id());
        within("close", mine.close()).await;
        within("close", theirs.close()).await;
    }

    fn private_file(path: &Path, contents: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .expect("create");
        file.write_all(contents).expect("write");
    }

    fn assert_refused(path: &Path, kind: io::ErrorKind, problem: &str) {
        let error = load_or_create_secret_key(path).expect_err("the key file was accepted");
        assert_eq!(error.kind(), kind, "{error}");
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains(problem), "{message}");
    }

    #[test]
    fn a_key_file_of_the_wrong_length_is_refused() {
        let dir = TempDir::new("iroh-key");
        let short = dir.path().join("short.key");
        private_file(&short, &[7; 31]);
        assert_refused(&short, io::ErrorKind::InvalidData, "holds 31 bytes");

        let long = dir.path().join("long.key");
        private_file(&long, &[7; 33]);
        assert_refused(&long, io::ErrorKind::InvalidData, "holds 33 bytes");
    }

    #[test]
    fn a_key_file_others_may_read_is_refused() {
        let dir = TempDir::new("iroh-key");
        let path = dir.path().join("peer.key");
        load_or_create_secret_key(&path).expect("create");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");

        assert_refused(&path, io::ErrorKind::PermissionDenied, "has mode 644");
    }

    #[test]
    fn a_key_path_that_is_not_a_regular_file_is_refused() {
        let dir = TempDir::new("iroh-key");
        let path = dir.path().join("peer.key");
        fs::create_dir(&path).expect("mkdir");

        assert_refused(&path, io::ErrorKind::InvalidInput, "is not a regular file");
    }
}
