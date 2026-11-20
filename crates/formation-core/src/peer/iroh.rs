//! What the peer protocol over iroh needs on both sides (TASK-046): the ALPN,
//! the caller's key and the identity the policy sees for it, the close codes,
//! the endpoint's settings, and this Mac's key file. The server is
//! [`api::IrohPeerApi`](super::api::IrohPeerApi); the client side is still to
//! come. Built only with the `iroh` feature.
//!
//! The first step keeps routes, policy and schema unchanged: a caller's key
//! stands where the Tailscale identity stood. [`whois_for`] puts
//! `iroh:<key in lowercase hex>` in both [`WhoIs::login`] and
//! [`WhoIs::node`], so a member is matched by that text in `tailscale_login`
//! and `tailscale_node`. The `iroh:` prefix keeps it from ever equalling a
//! Tailscale login or node, whatever a policy compares; settle it before
//! anything stores it.
//!
//! | Setting | Value | Where |
//! | --- | --- | --- |
//! | ALPN | `formation-peer/1` ([`ALPN`]) | [`endpoint_builder`]; the server refuses any other |
//! | Bidirectional streams a peer may have open on one connection | [`MAX_STREAMS_PER_CONNECTION`] | QUIC transport limit |
//! | Unidirectional streams | none | QUIC transport limit |
//! | Bytes a peer may send ahead on one stream / one connection | 256 KiB / 2 MiB | QUIC transport limit |
//! | QUIC datagrams | refused | QUIC transport limit |
//! | Idle timeout | 30 s, iroh's default, pinned | QUIC transport limit |
//! | Relays and address lookup | n0's public ones, or none ([`Discovery`]) | [`endpoint_builder`] |
//!
//! iroh 1.4.1 gives no way to lower noq's limits on connections waiting to be
//! accepted (`max_incoming`, 65536) or their buffered bytes (10 MiB each,
//! 100 MiB in all): they live in the endpoint's server configuration, which
//! iroh builds itself from the ALPNs. The server keeps that queue short by
//! answering every incoming connection at once (see
//! [`api::IrohPeerApi`](super::api::IrohPeerApi)).
//!
//! Left open: the client side; whose relays and address lookup production
//! uses (with [`Discovery::PublicRelays`] the endpoint publishes its home relay
//! URL under its key to n0's pkarr server, see the spike's notes); the key
//! file's creation race (two processes creating it at once: the second fails
//! instead of loading the first's key) and a crash during the write, which
//! leaves a short file that loading refuses; where the key lives (a file
//! today, perhaps the Keychain).

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use iroh::endpoint::{Builder, Connection, IdleTimeout, QuicTransportConfig, VarInt, presets};
use iroh::{Endpoint, EndpointId, SecretKey};

use formation_proto::team::WhoIs;

/// The peer protocol's ALPN. It is a wire identifier: never change what it
/// means; a new protocol version gets a new one.
pub const ALPN: &[u8] = b"formation-peer/1";

/// Bidirectional streams a peer may have open on one connection at once, as a
/// QUIC transport limit; the server's per-connection limit defaults to the
/// same number.
pub const MAX_STREAMS_PER_CONNECTION: u32 = 16;

/// iroh's default, pinned so a change in iroh does not change it silently.
/// iroh's keep-alives (every 5 s) keep a live connection from reaching it.
const IDLE_TIMEOUT_MS: u32 = 30_000;
/// Bytes a peer may send ahead on one stream; a request body is far smaller.
const STREAM_RECEIVE_WINDOW: u32 = 256 * 1024;
/// Bytes a peer may send ahead across one connection's streams.
const RECEIVE_WINDOW: u32 = 2 * 1024 * 1024;

/// What [`whois_for`] puts before the key.
pub const IDENTITY_PREFIX: &str = "iroh:";

/// An endpoint's public key. For an accepted connection it is the remote
/// endpoint's key, proven by the QUIC handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeerKey(pub EndpointId);

impl fmt::Display for PeerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The identity the policy sees for `key`: `iroh:<key in lowercase hex>` as
/// both login and node.
pub fn whois_for(key: PeerKey) -> WhoIs {
    let text = format!("{IDENTITY_PREFIX}{key}");
    WhoIs {
        login: text.clone(),
        node: text,
    }
}

/// Why a peer server closed a connection: a QUIC application close code and a
/// short reason naming the rule. The codes are wire values; never reuse one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseCode {
    /// The server is stopping.
    Shutdown,
    /// The key is not a member of this Mac's team, is this Mac's own member,
    /// or stopped being a member while connected.
    NotMember,
    /// The server already holds its most connections.
    TooManyConnections,
    /// The key already holds its most connections.
    TooManyConnectionsForKey,
    /// The connection negotiated another ALPN.
    UnexpectedProtocol,
}

impl CloseCode {
    pub const fn code(self) -> u32 {
        match self {
            Self::Shutdown => 1,
            Self::NotMember => 2,
            Self::TooManyConnections => 3,
            Self::TooManyConnectionsForKey => 4,
            Self::UnexpectedProtocol => 5,
        }
    }

    pub const fn reason(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::NotMember => "not_member",
            Self::TooManyConnections => "max_connections",
            Self::TooManyConnectionsForKey => "max_connections_per_key",
            Self::UnexpectedProtocol => "unexpected_alpn",
        }
    }

    pub fn close(self, connection: &Connection) {
        connection.close(VarInt::from_u32(self.code()), self.reason().as_bytes());
    }
}

/// How an endpoint finds and is found by others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discovery {
    /// n0's public relays and address lookup (iroh's `presets::N0`).
    PublicRelays,
    /// No relay and no address lookup (`presets::Minimal`): peers are dialed
    /// by their direct addresses. For tests, and later for an own relay only.
    None,
}

/// An endpoint builder with this protocol's ALPN and transport limits. Bind it
/// with [`Builder::bind`]; tests add `bind_addr` for loopback first.
pub fn endpoint_builder(secret_key: SecretKey, discovery: Discovery) -> Builder {
    let builder = match discovery {
        Discovery::PublicRelays => Endpoint::builder(presets::N0),
        Discovery::None => Endpoint::builder(presets::Minimal),
    };
    let transport = QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS_PER_CONNECTION))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .stream_receive_window(VarInt::from_u32(STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(RECEIVE_WINDOW))
        .datagram_receive_buffer_size(None)
        .max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(IDLE_TIMEOUT_MS))))
        .build();
    builder
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(transport)
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
/// created: std has no `getuid`, and the crate adds no dependency for one.
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

    use super::*;
    use crate::ask::test_support::TempDir;

    const WAIT: Duration = Duration::from_secs(20);

    async fn within<F: Future>(step: &str, future: F) -> F::Output {
        tokio::time::timeout(WAIT, future)
            .await
            .unwrap_or_else(|_| panic!("{step} did not finish within {WAIT:?}"))
    }

    async fn local_endpoint(secret_key: SecretKey) -> Endpoint {
        let builder = endpoint_builder(secret_key, Discovery::None)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("loopback address");
        within("bind", builder.bind()).await.expect("bind")
    }

    #[test]
    fn the_identity_of_a_key_is_its_prefixed_hex() {
        let key = PeerKey(SecretKey::generate().public());
        let whois = whois_for(key);
        let hex: String = key
            .0
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(whois.login, format!("iroh:{hex}"));
        assert_eq!(whois.node, whois.login);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_key_file_is_private_and_keeps_the_endpoint_id() {
        let dir = TempDir::new("iroh-key");
        let path = dir.path().join("peer.key");

        let created = load_or_create_secret_key(&path).expect("create");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let first = local_endpoint(created).await;
        let first_id = first.id();
        within("close", first.close()).await;

        let loaded = load_or_create_secret_key(&path).expect("load");
        let second = local_endpoint(loaded).await;
        assert_eq!(second.id(), first_id);
        within("close", second.close()).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_key_file_gives_another_endpoint_id() {
        let dir = TempDir::new("iroh-key");
        let mine = load_or_create_secret_key(&dir.path().join("mine.key")).expect("mine");
        let theirs = load_or_create_secret_key(&dir.path().join("theirs.key")).expect("theirs");

        let mine = local_endpoint(mine).await;
        let theirs = local_endpoint(theirs).await;
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
