// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Endpoint naming for MRP/1: ALPN, default port, URL scheme and URL parsing.
//!
//! These are the parts of the transport contract a peer needs even when it does not use
//! this crate's Quinn client — a HarmonyOS build, for instance, drives the platform QUIC
//! stack but must still negotiate `mirai/1` and resolve `mirai://host:port`. They live
//! outside [`transport`](crate::transport) so they survive `--no-default-features`.

pub const ALPN: &[u8] = b"mirai/1";
pub const DEFAULT_PORT: u16 = 9678;
pub const URL_SCHEME: &str = "mirai://";

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("bad address {url:?}: {reason}")]
pub struct AddressError {
    pub url: String,
    pub reason: String,
}

impl AddressError {
    fn new(url: &str, reason: &str) -> AddressError {
        AddressError {
            url: url.to_string(),
            reason: reason.to_string(),
        }
    }
}

/// Splits `mirai://host:port` (or a bare `host:port` / `host`) into its parts.
pub fn parse_url(url: &str) -> Result<(String, u16), AddressError> {
    let rest = url.trim().strip_prefix(URL_SCHEME).unwrap_or(url.trim());
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err(AddressError::new(url, "empty host"));
    }
    // IPv6 literal in brackets.
    if let Some(close) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
        let host = &rest[1..=close];
        let port = match rest[close + 2..].strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| AddressError::new(url, "bad port"))?,
            None => DEFAULT_PORT,
        };
        return Ok((host.to_string(), port));
    }
    match rest.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_string(),
            p.parse().map_err(|_| AddressError::new(url, "bad port"))?,
        )),
        None => Ok((rest.to_string(), DEFAULT_PORT)),
    }
}

/// The SNI name to present for `host`.
///
/// A self-signed certificate is generated for its hostname and MRP/1 verifies the pin, not
/// the name, but a TLS client still needs a syntactically valid server name and an IP
/// literal is not one.
pub fn sni_for(host: &str) -> String {
    if host.parse::<std::net::IpAddr>().is_ok() {
        "localhost".to_string()
    } else {
        host.to_string()
    }
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
        assert_eq!(parse_url("[::1]:1234").unwrap(), ("::1".to_string(), 1234));
        assert_eq!(
            parse_url("mirai://[fe80::1]").unwrap(),
            ("fe80::1".to_string(), DEFAULT_PORT)
        );
        assert!(parse_url("mirai://host:notaport").is_err());
        assert!(parse_url("mirai://").is_err());
    }

    #[test]
    fn ip_literals_fall_back_to_a_syntactically_valid_sni() {
        assert_eq!(sni_for("192.168.1.10"), "localhost");
        assert_eq!(sni_for("fe80::1"), "localhost");
        assert_eq!(sni_for("box.local"), "box.local");
    }
}
