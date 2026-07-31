use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use thiserror::Error;
use url::Url;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UrlPolicyError {
    #[error("only http:// and https:// URLs are supported")]
    UnsupportedScheme,
    #[error("URL is missing a host")]
    MissingHost,
    #[error("URL points to a blocked host or address: {0}")]
    BlockedHost(String),
    #[error("failed to resolve host {0}: {1}")]
    ResolveFailed(String, String),
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

/// Normalize a user-supplied URL string into an absolute http(s) URL.
pub fn normalize_url_string(raw: &str) -> Result<Url, UrlPolicyError> {
    let trimmed = raw.trim();
    let with_scheme = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    Url::parse(&with_scheme).map_err(|error| UrlPolicyError::InvalidUrl(error.to_string()))
}

/// Canonicalize a URL for cache keys and deduplication.
///
/// - lowercases scheme/host
/// - strips fragments
/// - drops common tracking query params
pub fn canonicalize_url(url: &Url) -> Url {
    let mut canonical = url.clone();
    if let Some(host) = canonical.host_str().map(|host| host.to_ascii_lowercase()) {
        let _ = canonical.set_host(Some(&host));
    }
    canonical.set_fragment(None);

    let tracking = [
        "utm_source",
        "utm_medium",
        "utm_campaign",
        "utm_term",
        "utm_content",
        "utm_id",
        "fbclid",
        "gclid",
        "mc_cid",
        "mc_eid",
        "msclkid",
    ];
    let pairs: Vec<(String, String)> = canonical
        .query_pairs()
        .filter(|(key, _)| {
            !tracking
                .iter()
                .any(|blocked| key.eq_ignore_ascii_case(blocked))
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if pairs.is_empty() {
        canonical.set_query(None);
    } else {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in pairs {
            serializer.append_pair(&key, &value);
        }
        canonical.set_query(Some(&serializer.finish()));
    }
    canonical
}

/// Validate that a URL is safe for agent-initiated public web fetch.
///
/// Blocks private / link-local / loopback / metadata-style addresses for both
/// IP literals and DNS-resolved hosts.
pub fn ensure_public_http_url(url: &Url) -> Result<(), UrlPolicyError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlPolicyError::UnsupportedScheme);
    }
    let Some(host) = url.host() else {
        return Err(UrlPolicyError::MissingHost);
    };

    match host {
        url::Host::Ipv4(ip) => {
            if is_blocked_ip(IpAddr::V4(ip)) {
                return Err(UrlPolicyError::BlockedHost(ip.to_string()));
            }
        }
        url::Host::Ipv6(ip) => {
            if is_blocked_ip(IpAddr::V6(ip)) {
                return Err(UrlPolicyError::BlockedHost(ip.to_string()));
            }
        }
        url::Host::Domain(domain) => {
            let domain_lower = domain.to_ascii_lowercase();
            if is_blocked_hostname(&domain_lower) {
                return Err(UrlPolicyError::BlockedHost(domain_lower));
            }
            resolve_and_check_host(&domain_lower)?;
        }
    }
    Ok(())
}

fn is_blocked_hostname(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host == "metadata.google.internal"
}

pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || octets[0] == 0
        // CGNAT 100.64.0.0/10
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        // 192.0.0.0/24 and TEST-NET-1 192.0.2.0/24
        || (octets[0] == 192 && octets[1] == 0 && matches!(octets[2], 0 | 2))
        // benchmarking 198.18.0.0/15
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        // TEST-NET-2 198.51.100.0/24
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        // TEST-NET-3 203.0.113.0/24
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || is_ipv6_unique_local(ip)
        || is_ipv6_link_local(ip)
}

fn is_ipv6_unique_local(ip: Ipv6Addr) -> bool {
    // fc00::/7
    (ip.octets()[0] & 0xfe) == 0xfc
}

fn is_ipv6_link_local(ip: Ipv6Addr) -> bool {
    // fe80::/10
    ip.segments()[0] & 0xffc0 == 0xfe80
}

fn resolve_and_check_host(host: &str) -> Result<(), UrlPolicyError> {
    let addrs = (host, 0)
        .to_socket_addrs()
        .map_err(|error| UrlPolicyError::ResolveFailed(host.to_string(), error.to_string()))?;
    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_blocked_ip(addr.ip()) {
            return Err(UrlPolicyError::BlockedHost(format!(
                "{host} -> {}",
                addr.ip()
            )));
        }
    }
    if !saw_any {
        return Err(UrlPolicyError::ResolveFailed(
            host.to_string(),
            "no addresses".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_ip_literals() {
        for raw in [
            "http://127.0.0.1/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            let url = Url::parse(raw).unwrap();
            assert!(
                ensure_public_http_url(&url).is_err(),
                "expected block for {raw}"
            );
        }
    }

    #[test]
    fn rejects_localhost_hostname() {
        let url = Url::parse("http://localhost:8080/").unwrap();
        assert!(ensure_public_http_url(&url).is_err());
    }

    #[test]
    fn strips_tracking_params() {
        let url =
            Url::parse("https://Example.COM/path?utm_source=x&id=1&fbclid=abc#frag").unwrap();
        let canonical = canonicalize_url(&url);
        assert_eq!(canonical.host_str(), Some("example.com"));
        assert!(canonical.fragment().is_none());
        assert_eq!(canonical.query(), Some("id=1"));
    }

    #[test]
    fn normalize_adds_https() {
        let url = normalize_url_string("docs.rs/gpui").unwrap();
        assert_eq!(url.as_str(), "https://docs.rs/gpui");
    }
}
