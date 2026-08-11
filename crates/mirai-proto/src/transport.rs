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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

use crate::sha256::fingerprint;

pub const ALPN: &[u8] = b"mirai/1";
pub const DEFAULT_PORT: u16 = 9678;
pub const URL_SCHEME: &str = "mirai://";

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

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
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
        if let Some(expected) = &self.expected
            && *expected != got
        {
            return Err(rustls::Error::General(format!(
                "certificate fingerprint {got} does not match the pinned {expected}"
            )));
        }
        *self.observed.lock().expect("fingerprint mutex") = Some(got);
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

/// Splits `mirai://host:port` (or a bare `host:port` / `host`) into its parts.
pub fn parse_url(url: &str) -> Result<(String, u16), TransportError> {
    let rest = url.trim().strip_prefix(URL_SCHEME).unwrap_or(url.trim());
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err(TransportError::Address(
            url.to_string(),
            "empty host".into(),
        ));
    }
    // IPv6 literal in brackets.
    if let Some(close) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
        let host = &rest[1..=close];
        let port = match rest[close + 2..].strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| TransportError::Address(url.into(), "bad port".into()))?,
            None => DEFAULT_PORT,
        };
        return Ok((host.to_string(), port));
    }
    match rest.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_string(),
            p.parse()
                .map_err(|_| TransportError::Address(url.into(), "bad port".into()))?,
        )),
        None => Ok((rest.to_string(), DEFAULT_PORT)),
    }
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
    let sni = if host.parse::<std::net::IpAddr>().is_ok() {
        "localhost".to_string()
    } else {
        host
    };

    let conn = endpoint
        .connect(addr, &sni)
        .map_err(|e| TransportError::Connect(e.to_string()))?
        .await
        .map_err(|e| TransportError::Connect(e.to_string()))?;

    let fp = observed
        .lock()
        .expect("fingerprint mutex")
        .clone()
        .unwrap_or_default();
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
    fn urls_parse_with_and_without_scheme_and_port() {
        assert_eq!(
            parse_url("mirai://192.168.1.10:9678").unwrap(),
            ("192.168.1.10".to_string(), 9678)
        );
        assert_eq!(
            parse_url("mirai://box.local").unwrap(),
            ("box.local".to_string(), DEFAULT_PORT)
        );
        assert_eq!(
            parse_url("[::1]:1234").unwrap(),
            ("::1".to_string(), 1234)
        );
        assert_eq!(
            parse_url("mirai://[fe80::1]").unwrap(),
            ("fe80::1".to_string(), DEFAULT_PORT)
        );
        assert!(parse_url("mirai://host:notaport").is_err());
        assert!(parse_url("mirai://").is_err());
    }

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
}
