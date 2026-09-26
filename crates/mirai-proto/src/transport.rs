// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! QUIC transport for MRP.
//!
//! Stream topology:
//!
//! * one client-opened **bidirectional control stream**, created right after the handshake,
//!   carrying [`ClientMsg`](crate::msg::ClientMsg) one way and
//!   [`ServerMsg`](crate::msg::ServerMsg) the other, for the whole session;
//! * one server-opened **unidirectional stream per subscription**, prefixed with the 4-byte
//!   LE subscription id and then carrying [`SubMsg`](crate::msg::SubMsg) frames.
//!
//! A cancelled subscription's stream is reset by the server and `stop_sending` by the
//! client, so buffered stale reports are discarded instead of delivered. That property is
//! the reason this is QUIC and not one TCP socket.
//!
//! TLS pins the server certificate's SHA-256. A token is sent only on a connection
//! pinned to a fingerprint the user has already accepted. [`probe`] learns that
//! fingerprint and closes without opening a stream.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

use crate::sha256::fingerprint;

pub use crate::endpoint::{ALPN, AddressError, DEFAULT_PORT, URL_SCHEME, parse_url, sni_for};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("certificate error: {0}")]
    Cert(String),
    #[error("connection error: {0}")]
    Connect(String),
    #[error("bad address {0:?}: {1}")]
    Address(String, String),
    #[error(
        "server certificate fingerprint {got} does not match the pinned {expected}; \
         refusing to connect"
    )]
    FingerprintMismatch { expected: String, got: String },
}

impl From<AddressError> for TransportError {
    fn from(e: AddressError) -> TransportError {
        TransportError::Address(e.url, e.reason)
    }
}

/// The one crypto provider this crate uses, assembled once per process.
///
/// `ring::default_provider()` builds fresh cipher-suite and key-exchange vectors on every
/// call (`DEFAULT_CIPHER_SUITES.to_vec()` / `DEFAULT_KX_GROUPS.to_vec()`). A client
/// endpoint wants two of them — the `ClientConfig` builder and the [`TofuVerifier`] —
/// and every reconnection builds another endpoint, so handing out clones of one `Arc`
/// turns that into a pointer bump. There is nothing to choose per call either way:
/// `rustls` is pinned to `ring` deliberately, because a second installed provider makes
/// `ClientConfig::builder()` panic at runtime.
static PROVIDER: LazyLock<Arc<rustls::crypto::CryptoProvider>> =
    LazyLock::new(|| Arc::new(rustls::crypto::ring::default_provider()));

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::clone(&PROVIDER)
}

/// Shared QUIC tuning, built once. Keep-alives are short so a dead peer is noticed while
/// a user is still looking at the board.
static TRANSPORT: LazyLock<Arc<quinn::TransportConfig>> =
    LazyLock::new(|| Arc::new(transport_config()));

/// How far a stream may run ahead of what its reader has consumed: the peer's
/// `stream_receive_window`, which every endpoint here advertises.
///
/// A subscription's reports pass through a `watch` that keeps only the newest, so a slow
/// link sheds stale reports — but only once a write blocks, and a write blocks only when
/// this window is spent. quinn's default is 1.25 MB, some 500 live reports: on a slow link
/// every one of them is delivered, late and in order. 16 KiB is a handful of reports:
/// on a link slower than the reports it bounds the lag to 16 KiB over the link rate, and
/// it still allows 50 KB/s per stream at a 300 ms round trip, far above what one
/// subscription produces.
const STREAM_WINDOW: u32 = 16 * 1024;

/// Shared QUIC tuning. Keep-alives are short so a dead peer is noticed while a user is
/// still looking at the board.
pub fn transport_config() -> quinn::TransportConfig {
    let mut tc = quinn::TransportConfig::default();
    tc.keep_alive_interval(Some(Duration::from_secs(5)));
    tc.max_idle_timeout(Some(
        Duration::from_secs(30)
            .try_into()
            .expect("30s is a valid idle timeout"),
    ));
    tc.max_concurrent_uni_streams(quinn::VarInt::from_u32(256));
    tc.stream_receive_window(quinn::VarInt::from_u32(STREAM_WINDOW));
    tc
}

// ---------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------

/// Loads `cert_path`/`key_path`, generating a self-signed pair for `hostnames` if either
/// file is missing.
///
/// Generation replaces both files. They are staged first, then the stale cert is
/// removed and that directory synced, then the key is renamed and its directory
/// synced, then the cert. A crash or power loss at any of those points leaves either
/// a complete pair or at least one file missing — which regenerates both — and never
/// a new key beside an old cert, even when the two files are in different directories.
pub fn load_or_generate_cert(
    cert_path: &Path,
    key_path: &Path,
    hostnames: &[String],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TransportError> {
    // `OpenOptionsExt::mode` only affects newly-created files; also repair an existing key
    // before it is read or overwritten.
    #[cfg(unix)]
    if key_path.exists() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    if !cert_path.exists() || !key_path.exists() {
        let names: Vec<String> = if hostnames.is_empty() {
            vec!["localhost".to_string()]
        } else {
            hostnames.to_vec()
        };
        let ck = rcgen::generate_simple_self_signed(names)
            .map_err(|e| TransportError::Cert(e.to_string()))?;
        if let Some(dir) = cert_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if let Some(dir) = key_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        install_generated_pair(
            cert_path,
            key_path,
            ck.cert.pem(),
            ck.signing_key.serialize_pem(),
        )?;
    }

    let certs = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| TransportError::Cert(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TransportError::Cert(e.to_string()))?;
    if certs.is_empty() {
        return Err(TransportError::Cert(format!(
            "{} contains no certificate",
            cert_path.display()
        )));
    }
    let key =
        PrivateKeyDer::from_pem_file(key_path).map_err(|e| TransportError::Cert(e.to_string()))?;
    Ok((certs, key))
}

/// Stages both halves, then installs them so a power loss cannot pair a new key with
/// an old cert.
///
/// The stale cert is removed and that directory is synced before the key is renamed.
/// The key directory is synced after that rename and before the cert is renamed. A
/// crash or power loss at any point leaves a complete pair or at least one file
/// missing — which regenerates both — never a new key beside an old cert. The cert
/// and the key may live in different directories.
fn install_generated_pair(
    cert_path: &Path,
    key_path: &Path,
    cert_pem: String,
    key_pem: String,
) -> Result<(), TransportError> {
    #[cfg(unix)]
    let key_mode = Some(0o600);
    #[cfg(not(unix))]
    let key_mode = None;
    let key_tmp = crate::atomic::stage(key_path, key_pem.as_bytes(), key_mode)?;
    let cert_tmp = match crate::atomic::stage(cert_path, cert_pem.as_bytes(), None) {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&key_tmp);
            return Err(error.into());
        }
    };
    let installed = (|| -> std::io::Result<()> {
        remove_if_present(cert_path)?;
        crate::atomic::sync_parent(cert_path)?;
        remove_if_present(key_path)?;
        std::fs::rename(&key_tmp, key_path)?;
        crate::atomic::sync_parent(key_path)?;
        std::fs::rename(&cert_tmp, cert_path)?;
        crate::atomic::sync_parent(cert_path)?;
        Ok(())
    })();
    if let Err(error) = installed {
        let _ = std::fs::remove_file(&cert_tmp);
        let _ = std::fs::remove_file(&key_tmp);
        return Err(error.into());
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The fingerprint clients pin: lowercase hex SHA-256 of the leaf certificate's DER.
pub fn fingerprint_of(certs: &[CertificateDer<'static>]) -> String {
    certs.first().map(|c| fingerprint(c)).unwrap_or_default()
}

pub fn server_endpoint(
    listen: SocketAddr,
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::Endpoint, TransportError> {
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicServerConfig::try_from(crypto)
        .map_err(|e| TransportError::Cert(format!("no initial cipher suite: {e}")))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(Arc::clone(&TRANSPORT));
    Ok(quinn::Endpoint::server(cfg, listen)?)
}

// ---------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------
/// Certificate verification for a pinned connection, or for a probe that only records.
///
/// A pin is normalised first — trim, lowercase, strip `:` — so a value pasted from
/// `openssl x509 -fingerprint` matches the lowercase hex the server prints. The
/// accept-anything mode exists only for [`probe`]: it never opens a stream, so it
/// cannot carry a token. [`connect`] always passes a pin.
#[derive(Debug)]
pub struct TofuVerifier {
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}

impl TofuVerifier {
    /// Accepts only `expected`. An empty pin matches nothing.
    pub fn new(expected: String, observed: Arc<Mutex<Option<String>>>) -> TofuVerifier {
        TofuVerifier {
            expected: Some(normalize_fingerprint(&expected)),
            observed,
        }
    }

    /// Records whatever leaf is offered. Used only by [`probe`].
    fn observe(observed: Arc<Mutex<Option<String>>>) -> TofuVerifier {
        TofuVerifier {
            expected: None,
            observed,
        }
    }
}

/// A user-supplied pin in the form every comparison uses.
///
/// `openssl x509 -fingerprint -sha256` prints uppercase hex with colons, often
/// with surrounding whitespace. The pin itself is lowercase hex, no separators.
fn normalize_fingerprint(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(':', "")
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = fingerprint(end_entity);
        // Recorded before the comparison, and on every outcome: when a pin does not match,
        // this is the only place the certificate that was actually offered is visible, and
        // `connect` needs it to explain the failure. Recording is not trusting — the caller
        // persists a fingerprint only after a human agrees to it.
        *self.observed.lock().expect("fingerprint mutex") = Some(got.clone());
        if let Some(expected) = &self.expected
            && *expected != got
        {
            return Err(rustls::Error::General(format!(
                "certificate fingerprint {got} does not match the pinned {expected}"
            )));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &PROVIDER.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &PROVIDER.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        PROVIDER
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Builds a client endpoint bound to an ephemeral local port.
///
/// `expected` is required: an unpinned endpoint could be used to send a token. A probe
/// uses [`observe_endpoint`] and closes before any stream exists.
pub fn client_endpoint(
    expected: String,
    observed: Arc<Mutex<Option<String>>>,
) -> Result<quinn::Endpoint, TransportError> {
    endpoint_with(TofuVerifier::new(expected, observed))
}

fn observe_endpoint(
    observed: Arc<Mutex<Option<String>>>,
) -> Result<quinn::Endpoint, TransportError> {
    endpoint_with(TofuVerifier::observe(observed))
}

fn endpoint_with(verifier: TofuVerifier) -> Result<quinn::Endpoint, TransportError> {
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicClientConfig::try_from(crypto)
        .map_err(|e| TransportError::Cert(format!("no initial cipher suite: {e}")))?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(Arc::clone(&TRANSPORT));

    let mut ep = quinn::Endpoint::client((std::net::Ipv6Addr::UNSPECIFIED, 0).into())
        .or_else(|_| quinn::Endpoint::client((std::net::Ipv4Addr::UNSPECIFIED, 0).into()))?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

/// How [`open_connection`] decides which certificates to accept.
enum Trust {
    /// Only this pin. The only mode that may be followed by `Hello`.
    Pin(String),
    /// Record the leaf. [`probe`] closes immediately afterwards.
    Observe,
}

/// Resolves `url` and finishes the TLS handshake. Does not open a stream.
async fn open_connection(
    url: &str,
    trust: Trust,
) -> Result<(quinn::Connection, String, quinn::Endpoint), TransportError> {
    let (host, port) = parse_url(url)?;
    let observed = Arc::new(Mutex::new(None));
    let (endpoint, expected) = match trust {
        Trust::Pin(pin) => {
            let pin = normalize_fingerprint(&pin);
            if pin.is_empty() {
                return Err(TransportError::Cert(
                    "a certificate pin is required before the token can be sent".into(),
                ));
            }
            (client_endpoint(pin.clone(), observed.clone())?, Some(pin))
        }
        Trust::Observe => (observe_endpoint(observed.clone())?, None),
    };

    let addr = tokio::task::spawn_blocking({
        let host = host.clone();
        move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|mut it| it.next())
        }
    })
    .await
    .map_err(|e| TransportError::Address(url.into(), e.to_string()))?
    .map_err(|e| TransportError::Address(url.into(), e.to_string()))?
    .ok_or_else(|| TransportError::Address(url.into(), "no address resolved".into()))?;

    // A self-signed certificate is generated for its hostname; the verifier only compares
    // fingerprints, but rustls still needs a syntactically valid SNI name.
    let sni = sni_for(&host);

    let attempt = endpoint
        .connect(addr, &sni)
        .map_err(|e| TransportError::Connect(e.to_string()))?
        .await;

    // The verifier rejects a mismatched pin *inside* the handshake, so what surfaces here is
    // a bare TLS alert. A user cannot act on "error 40"; they can act on being told which
    // certificate was offered instead of the one they trusted.
    let observed_fingerprint = || {
        observed
            .lock()
            .expect("fingerprint mutex")
            .clone()
            .unwrap_or_default()
    };
    let conn = match attempt {
        Ok(conn) => conn,
        Err(e) => {
            let got = observed_fingerprint();
            if let Some(expected) = expected.as_ref()
                && !got.is_empty()
                && expected != &got
            {
                return Err(TransportError::FingerprintMismatch {
                    expected: expected.clone(),
                    got,
                });
            }
            return Err(TransportError::Connect(e.to_string()));
        }
    };

    // Defence in depth: a handshake that somehow succeeded against the wrong certificate
    // must still not be used.
    let fp = observed_fingerprint();
    if let Some(expected) = expected
        && expected != fp
    {
        return Err(TransportError::FingerprintMismatch { expected, got: fp });
    }
    if fp.is_empty() {
        return Err(TransportError::Cert(
            "the server presented no certificate".into(),
        ));
    }
    Ok((conn, fp, endpoint))
}

/// TLS handshake only. Returns the leaf fingerprint and closes with application code 0.
///
/// Opens no stream and writes no frame, so a token cannot be sent on this connection.
/// The caller shows the fingerprint to the user; only a later [`connect`] with that pin
/// may send `Hello`.
pub async fn probe(url: &str) -> Result<String, TransportError> {
    let (conn, fingerprint, endpoint) = open_connection(url, Trust::Observe).await?;
    conn.close(quinn::VarInt::from_u32(0), b"");
    // Dropping the endpoint before the close is flushed would look, to the server, like
    // a crash rather than an orderly probe. A peer that never acks must not stall us.
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
    Ok(fingerprint)
}

/// Resolves `mirai://host:port` and opens a QUIC connection pinned to `pin`.
///
/// `pin` is normalised once here, before the verifier is built, and that value is what
/// every later comparison and error uses. There is no unpinned mode: a caller that does
/// not yet have a pin must [`probe`] and ask the user, not send a token.
pub async fn connect(
    url: &str,
    pin: String,
) -> Result<(quinn::Connection, String), TransportError> {
    let (conn, fingerprint, _endpoint) = open_connection(url, Trust::Pin(pin)).await?;
    Ok((conn, fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_cert_is_reused_and_private_key_is_restricted() {
        let dir = std::env::temp_dir().join(format!("mirai-tp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");

        let (c1, _k1) =
            load_or_generate_cert(&cert, &key, &["mirai.test".into()]).expect("generate");
        let fp1 = fingerprint_of(&c1);
        assert_eq!(fp1.len(), 64);

        let (c2, _k2) = load_or_generate_cert(&cert, &key, &["mirai.test".into()]).expect("reload");
        assert_eq!(fingerprint_of(&c2), fp1, "existing cert must be reused");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
                0o600,
                "a generated key must be private from the moment it is created"
            );
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
            let (_c3, _k3) =
                load_or_generate_cert(&cert, &key, &["mirai.test".into()]).expect("tighten");
            assert_eq!(
                std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A server endpoint on an ephemeral port with a fresh certificate, and that
    /// certificate's fingerprint.
    fn a_server_endpoint(tag: &str) -> (quinn::Endpoint, String) {
        let dir = std::env::temp_dir().join(format!("mirai-tp-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (certs, key) = load_or_generate_cert(
            &dir.join("cert.pem"),
            &dir.join("key.pem"),
            &["localhost".into()],
        )
        .expect("certificate");
        let fp = fingerprint_of(&certs);
        let listen = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
        (
            server_endpoint(listen, certs, key).expect("server endpoint"),
            fp,
        )
    }

    /// Starts a real server on an ephemeral port, accepting connections, and returns its
    /// address and certificate fingerprint.
    ///
    /// The accept loop is the point: without it the handshake never progresses, so the client
    /// never sees a certificate and every failure looks like a timeout.
    fn a_server(tag: &str) -> (std::net::SocketAddr, String) {
        let (endpoint, fp) = a_server_endpoint(tag);
        let addr = endpoint.local_addr().expect("bound");
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                // A rejected handshake resolves to an error here; either way the client has
                // by then seen the certificate, which is all these tests need.
                let _ = incoming.await;
            }
        });
        (addr, fp)
    }

    /// Both ends of one live connection: the server's, the client's, and the server
    /// endpoint, which has to outlive them.
    async fn a_connected_pair(
        tag: &str,
    ) -> (quinn::Connection, quinn::Connection, quinn::Endpoint) {
        let (endpoint, fp) = a_server_endpoint(tag);
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let accepted = tokio::spawn(async move {
            let incoming = accepting.accept().await.expect("an incoming connection");
            incoming.await.expect("server handshake")
        });
        let (client, _) = connect(&format!("mirai://{addr}"), fp)
            .await
            .expect("client handshake");
        let server = accepted.await.expect("accept task");
        (server, client, endpoint)
    }

    /// A server writing to a stream nobody reads must block after one window. That block is
    /// the only thing that lets a subscription's `watch` drop stale reports on a slow link;
    /// without it they queue in the transport and arrive late, in order.
    #[tokio::test]
    async fn a_stream_runs_at_most_one_window_ahead_of_its_reader() {
        let (server, _client, _endpoint) = a_connected_pair("window").await;
        let mut stream = server.open_uni().await.expect("open a stream");

        let chunk = [0u8; 1024];
        let mut written = 0usize;
        while let Ok(result) =
            tokio::time::timeout(Duration::from_millis(500), stream.write_all(&chunk)).await
        {
            result.expect("write");
            written += chunk.len();
            assert!(written <= 4 << 20, "the stream never blocked");
        }

        let window = STREAM_WINDOW as usize;
        assert!(
            window / 2 <= written && written <= window + chunk.len(),
            "the server wrote {written} bytes ahead of a {window}-byte window"
        );
    }

    /// A pin mismatch is rejected inside the TLS handshake, so the transport error is a bare
    /// alert. What a user needs is which certificate was offered instead — and a caller that
    /// wants to re-prompt for trust needs to tell this apart from an unreachable server.
    #[tokio::test]
    async fn a_pin_mismatch_is_reported_as_a_mismatch_not_as_a_generic_failure() {
        let (addr, real) = a_server("mismatch");
        let wrong = "b".repeat(64);

        let err = connect(&format!("mirai://{addr}"), wrong.clone())
            .await
            .expect_err("connected with the wrong pin");

        match err {
            TransportError::FingerprintMismatch { expected, got } => {
                assert_eq!(expected, wrong);
                assert_eq!(got, real, "the offered certificate must be named");
            }
            other => panic!("expected a fingerprint mismatch, got {other}"),
        }
    }

    /// The matching pin must still connect, or the check above would be satisfied by a
    /// client that simply refuses everything.
    #[tokio::test]
    async fn the_pinned_fingerprint_connects() {
        let (addr, real) = a_server("match");

        let (_conn, fp) = connect(&format!("mirai://{addr}"), real.clone())
            .await
            .expect("the pinned certificate was refused");
        assert_eq!(fp, real);
    }

    /// Uppercase hex with colons and surrounding whitespace, as pasted from
    /// `openssl x509 -fingerprint -sha256`.
    fn openssl_fingerprint(fp: &str) -> String {
        let pairs: Vec<_> = fp
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .expect("fingerprint is hex")
                    .to_ascii_uppercase()
            })
            .collect();
        format!(" {}\n", pairs.join(":"))
    }

    /// A pin pasted in openssl's form must connect. The handshake accepts it; the
    /// comparison after the handshake must use that same normalised pin.
    #[tokio::test]
    async fn an_uppercase_colon_separated_pin_connects() {
        let (addr, real) = a_server("openssl-pin");
        let pasted = openssl_fingerprint(&real);
        assert_ne!(
            pasted, real,
            "the pasted form must not already be canonical"
        );

        let (_conn, fp) = connect(&format!("mirai://{addr}"), pasted)
            .await
            .expect("an openssl-style pin of the real certificate was refused");
        assert_eq!(fp, real);
    }

    /// An unreachable address must not be dressed up as a certificate problem.
    #[tokio::test]
    async fn an_unreachable_server_is_not_a_mismatch() {
        // Nothing listens on UDP port 1; the handshake times out rather than being rejected.
        let attempt = connect("mirai://127.0.0.1:1", "c".repeat(64));
        match tokio::time::timeout(Duration::from_secs(2), attempt).await {
            Ok(Err(TransportError::FingerprintMismatch { .. })) => {
                panic!("a silent port was reported as a certificate mismatch")
            }
            Ok(Err(_)) | Err(_) => {} // a connection error, or still trying: both are honest
            Ok(Ok(_)) => panic!("connected to a port with no server on it"),
        }
    }

    /// A probe learns the fingerprint and closes. The server must not see a stream: that
    /// is the only place a token could be written.
    #[tokio::test]
    async fn a_probe_returns_the_fingerprint_and_opens_no_stream() {
        let (endpoint, real) = a_server_endpoint("probe");
        let addr = endpoint.local_addr().expect("bound");
        let accepting = endpoint.clone();
        let served = tokio::spawn(async move {
            let incoming = accepting.accept().await.expect("an incoming connection");
            let conn = incoming.await.expect("server handshake");
            if conn.accept_bi().await.is_ok() {
                panic!("a probe opened a stream");
            }
        });

        let got = probe(&format!("mirai://{addr}")).await.expect("probe");
        assert_eq!(got, real);
        served.await.expect("server task");
    }
}
