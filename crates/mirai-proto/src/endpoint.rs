// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Endpoint naming for MRP: ALPN, default port, URL scheme and URL parsing.
//!
//! These are the parts of the transport contract a peer needs even when it does not use
//! this crate's Quinn client — a HarmonyOS build, for instance, drives the platform QUIC
//! stack but must still negotiate `mirai` and resolve `mirai://host:port`. They live
//! outside [`transport`](crate::transport) so they survive `--no-default-features`.

/// Fixed protocol identifier. It carries no version: a mismatch must reach `Hello` /
/// `Welcome` and come back as `BadVersion`, not fail inside the TLS handshake.
pub const ALPN: &[u8] = b"mirai";
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
    // IPv6 literal in brackets. The grammar allows only an optional `:port` after `]`.
    if let Some(inner) = rest.strip_prefix('[')
        && let Some((host, after)) = inner.split_once(']')
    {
        if host.is_empty() {
            return Err(AddressError::new(url, "empty host"));
        }
        let port = if after.is_empty() {
            DEFAULT_PORT
        } else {
            let Some(port) = after.strip_prefix(':') else {
                return Err(AddressError::new(url, "trailing junk"));
            };
            port.parse()
                .map_err(|_| AddressError::new(url, "bad port"))?
        };
        return Ok((host.to_string(), port));
    }
    match rest.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return Err(AddressError::new(url, "empty host"));
            }
            // `fe80::1` would otherwise split into host `fe80:` and port 1. The grammar
            // allows an IPv6 literal only inside `[]`.
            if host.contains(':') {
                return Err(AddressError::new(url, "IPv6 literals must be bracketed"));
            }
            Ok((
                host.to_string(),
                port.parse()
                    .map_err(|_| AddressError::new(url, "bad port"))?,
            ))
        }
        None => Ok((rest.to_string(), DEFAULT_PORT)),
    }
}

/// The SNI name to present for `host`.
///
/// A self-signed certificate is generated for its hostname and MRP verifies the pin, not
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
    fn empty_hosts_and_junk_after_an_ipv6_literal_are_rejected() {
        for url in [
            "[]",
            "mirai://[]",
            "mirai://[]:9678",
            "mirai://[]/",
            "mirai://:9678",
            ":9678",
        ] {
            let err = parse_url(url).expect_err(url);
            assert_eq!(err.reason, "empty host", "{url}");
        }
        for url in ["[::1]1234", "mirai://[::1]1234", "mirai://[::1]1234/"] {
            let err = parse_url(url).expect_err(url);
            assert_eq!(err.reason, "trailing junk", "{url}");
        }
        // Those must not be confused with a well-formed literal.
        assert_eq!(parse_url("[::1]:1234").unwrap(), ("::1".into(), 1234));
        assert_eq!(
            parse_url("mirai://[::1]").unwrap(),
            ("::1".into(), DEFAULT_PORT)
        );
    }

    #[test]
    fn a_bare_ipv6_literal_is_rejected_instead_of_splitting_off_a_port() {
        for url in [
            "fe80::1",
            "mirai://fe80::1",
            "mirai://fe80::1/",
            "mirai://fe80::1:9678",
        ] {
            let err = parse_url(url).expect_err(url);
            assert_eq!(err.reason, "IPv6 literals must be bracketed", "{url}");
        }
        assert_eq!(
            parse_url("mirai://[fe80::1]:9678").unwrap(),
            ("fe80::1".to_string(), 9678)
        );
    }

    #[test]
    fn ip_literals_fall_back_to_a_syntactically_valid_sni() {
        assert_eq!(sni_for("192.168.1.10"), "localhost");
        assert_eq!(sni_for("fe80::1"), "localhost");
        assert_eq!(sni_for("box.local"), "box.local");
    }
}
