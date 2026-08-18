// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! QUIC transport for MRP/1.
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
//! TLS is trust-on-first-use: the client pins the server certificate's SHA-256 and refuses
//! anything else afterwards.

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
    tc
}

// ---------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------

/// Loads `cert_path`/`key_path`, generating a self-signed pair for `hostnames` if either
/// file is missing.
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
        std::fs::write(cert_path, ck.cert.pem())?;
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt;

            let mut key = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(key_path)?;
            key.write_all(ck.signing_key.serialize_pem().as_bytes())?;
        }
        #[cfg(not(unix))]
        std::fs::write(key_path, ck.signing_key.serialize_pem())?;
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
    cfg.transport_config(Arc::new(transport_config()));
    Ok(quinn::Endpoint::server(cfg, listen)?)
}

// ---------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------

/// Trust-on-first-use certificate verification.
///
/// With `expected` set, only that fingerprint is accepted. With `expected` unset, whatever
/// the server offers is accepted **and recorded** in `observed`; the caller is responsible
/// for showing it to the user and persisting it.
#[derive(Debug)]
pub struct TofuVerifier {
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl TofuVerifier {
    pub fn new(expected: Option<String>, observed: Arc<Mutex<Option<String>>>) -> TofuVerifier {
        TofuVerifier {
            expected: expected.map(|s| s.trim().to_ascii_lowercase().replace(':', "")),
            observed,
            provider: provider(),
        }
    }
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
            &self.provider.signature_verification_algorithms,
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
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Builds a client endpoint bound to an ephemeral local port.
pub fn client_endpoint(
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
) -> Result<quinn::Endpoint, TransportError> {
    let verifier = Arc::new(TofuVerifier::new(expected, observed));
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicClientConfig::try_from(crypto)
        .map_err(|e| TransportError::Cert(format!("no initial cipher suite: {e}")))?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(Arc::new(transport_config()));

    let mut ep = quinn::Endpoint::client((std::net::Ipv6Addr::UNSPECIFIED, 0).into())
        .or_else(|_| quinn::Endpoint::client((std::net::Ipv4Addr::UNSPECIFIED, 0).into()))?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

/// Resolves `mirai://host:port` and opens a QUIC connection.
///
/// Returns the connection and the server's certificate fingerprint, which the caller
/// should persist on a first (unpinned) connection.
pub async fn connect(
    url: &str,
    expected_fingerprint: Option<String>,
) -> Result<(quinn::Connection, String), TransportError> {
    let (host, port) = parse_url(url)?;
    let observed = Arc::new(Mutex::new(None));
    let endpoint = client_endpoint(expected_fingerprint.clone(), observed.clone())?;

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
            if let Some(expected) = expected_fingerprint
                && !got.is_empty()
                && expected != got
            {
                return Err(TransportError::FingerprintMismatch { expected, got });
            }
            return Err(TransportError::Connect(e.to_string()));
        }
    };

    // Defence in depth: a handshake that somehow succeeded against the wrong certificate
    // must still not be used.
    let fp = observed_fingerprint();
    if let Some(expected) = expected_fingerprint
        && expected != fp
    {
        return Err(TransportError::FingerprintMismatch { expected, got: fp });
    }
    Ok((conn, fp))
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

    /// Starts a real server on an ephemeral port, accepting connections, and returns its
    /// address and certificate fingerprint.
    ///
    /// The accept loop is the point: without it the handshake never progresses, so the client
    /// never sees a certificate and every failure looks like a timeout.
    fn a_server(tag: &str) -> (std::net::SocketAddr, String) {
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
        let endpoint = server_endpoint(listen, certs, key).expect("server endpoint");
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

    /// A pin mismatch is rejected inside the TLS handshake, so the transport error is a bare
    /// alert. What a user needs is which certificate was offered instead — and a caller that
    /// wants to re-prompt for trust needs to tell this apart from an unreachable server.
    #[tokio::test]
    async fn a_pin_mismatch_is_reported_as_a_mismatch_not_as_a_generic_failure() {
        let (addr, real) = a_server("mismatch");
        let wrong = "b".repeat(64);

        let err = connect(&format!("mirai://{addr}"), Some(wrong.clone()))
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

        let (_conn, fp) = connect(&format!("mirai://{addr}"), Some(real.clone()))
            .await
            .expect("the pinned certificate was refused");
        assert_eq!(fp, real);
    }

    /// An unreachable address must not be dressed up as a certificate problem.
    #[tokio::test]
    async fn an_unreachable_server_is_not_a_mismatch() {
        // Nothing listens on UDP port 1; the handshake times out rather than being rejected.
        let attempt = connect("mirai://127.0.0.1:1", Some("c".repeat(64)));
        match tokio::time::timeout(Duration::from_secs(2), attempt).await {
            Ok(Err(TransportError::FingerprintMismatch { .. })) => {
                panic!("a silent port was reported as a certificate mismatch")
            }
            Ok(Err(_)) | Err(_) => {} // a connection error, or still trying: both are honest
            Ok(Ok(_)) => panic!("connected to a port with no server on it"),
        }
    }
}
