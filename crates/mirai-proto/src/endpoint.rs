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
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') {
        return Err(AddressError::new(url, "a mirai URL has no path"));
    }
    if rest.contains(['?', '#', '@']) {
        return Err(AddressError::new(
            url,
            "a mirai URL has no query, fragment or userinfo",
        ));
    }
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
        let (addr, zone) = host
            .split_once('%')
            .map_or((host, None), |(a, z)| (a, Some(z)));
        if addr.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(AddressError::new(url, "not an IPv6 address inside []"));
        }
        if zone == Some("") {
            return Err(AddressError::new(url, "empty zone after %"));
        }
        let port = if after.is_empty() {
            DEFAULT_PORT
        } else {
            let Some(port) = after.strip_prefix(':') else {
                return Err(AddressError::new(url, "trailing junk"));
            };
            parse_port(url, port)?
        };
        return Ok((host.to_string(), port));
    }
    let (host, port) = rest
        .rsplit_once(':')
        .map_or((rest, None), |(host, port)| (host, Some(port)));
    if host.is_empty() {
        return Err(AddressError::new(url, "empty host"));
    }
    if host.contains(['[', ']']) {
        return Err(AddressError::new(url, "unmatched bracket"));
    }
    // `fe80::1` would otherwise split into host `fe80:` and port 1. The grammar allows an
    // IPv6 literal only inside `[]`.
    if host.contains(':') {
        return Err(AddressError::new(url, "IPv6 literals must be bracketed"));
    }
    if host.contains('%') {
        return Err(AddressError::new(
            url,
            "a zone is allowed only on a bracketed IPv6 literal",
        ));
    }
    let port = match port {
        Some(port) => parse_port(url, port)?,
        None => DEFAULT_PORT,
    };
    Ok((host.to_string(), port))
}

/// `1*DIGIT` within `u16`. `str::parse` alone would also take a leading `+`.
fn parse_port(url: &str, port: &str) -> Result<u16, AddressError> {
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AddressError::new(url, "bad port"));
    }
    port.parse().map_err(|_| AddressError::new(url, "bad port"))
}

/// The SNI name to present for `host`.
///
/// A self-signed certificate is generated for its hostname and MRP verifies the pin, not
/// the name, but a TLS client still needs a syntactically valid server name and an IP
/// literal is not one. Neither is a link-local literal with its zone (`fe80::1%eth0`),
/// which resolves as an address but would otherwise be sent as a name.
pub fn sni_for(host: &str) -> String {
    let ip = host.split_once('%').map_or(host, |(ip, _zone)| ip);
    if ip.parse::<std::net::IpAddr>().is_ok() {
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
        // The zone of a link-local literal stays with the host: resolving needs it.
        assert_eq!(
            parse_url("mirai://[fe80::1%eth0]:9678").unwrap(),
            ("fe80::1%eth0".to_string(), 9678)
        );
    }

    /// What the grammar does not produce is refused while the URL is still at hand, not
    /// passed on to fail in the resolver with a message that does not say why.
    #[test]
    fn a_host_outside_the_grammar_is_refused_by_name() {
        for (url, reason) in [
            ("[box.local]", "not an IPv6 address inside []"),
            ("mirai://[10.0.0.1]:9678", "not an IPv6 address inside []"),
            ("mirai://[fe80::1%]", "empty zone after %"),
            (
                "box%eth0",
                "a zone is allowed only on a bracketed IPv6 literal",
            ),
            (
                "mirai://10.0.0.1%eth0:9678",
                "a zone is allowed only on a bracketed IPv6 literal",
            ),
            ("mirai://[::1", "unmatched bracket"),
            ("box]:9678", "unmatched bracket"),
            ("box:+80", "bad port"),
            ("box:", "bad port"),
        ] {
            let err = parse_url(url).expect_err(url);
            assert_eq!(err.reason, reason, "{url}");
        }
    }

    #[test]
    fn a_url_ends_with_at_most_one_slash_and_has_no_path() {
        assert_eq!(parse_url("mirai://box:1/").unwrap(), ("box".into(), 1));
        assert_eq!(parse_url("[::1]/").unwrap(), ("::1".into(), DEFAULT_PORT));
        for url in [
            "mirai://box//",
            "mirai://box:1//",
            "mirai://box/path",
            "[::1]/x",
        ] {
            let err = parse_url(url).expect_err(url);
            assert_eq!(err.reason, "a mirai URL has no path", "{url}");
        }
    }

    #[test]
    fn credentials_queries_and_fragments_are_not_hosts_or_zones() {
        for url in [
            "mirai://token@box",
            "box?token=secret",
            "mirai://box#fragment",
            "mirai://[fe80::1%eth0?query]",
            "mirai://[fe80::1%eth0#fragment]",
            "mirai://[fe80::1%user@eth0]",
        ] {
            assert!(parse_url(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn ip_literals_fall_back_to_a_syntactically_valid_sni() {
        assert_eq!(sni_for("192.168.1.10"), "localhost");
        assert_eq!(sni_for("fe80::1"), "localhost");
        assert_eq!(sni_for("fe80::1%eth0"), "localhost");
        assert_eq!(sni_for("box.local"), "box.local");
    }
}
