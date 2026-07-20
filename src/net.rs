//! Network transport helpers shared across the crate's HTTP clients.

use std::net::{Ipv4Addr, Ipv6Addr};

use url::Host;

/// Return `true` only when `base_url` is a genuine clear-text `http` loopback
/// endpoint for which it is safe to relax the HTTPS-only transport guard.
///
/// Several HTTP clients in this crate allow clear-text `http` for a local dev
/// or mock server by disabling reqwest's `https_only`. The earlier checks did
/// this with `str::starts_with("http://localhost")` / `127.0.0.1` / `[::1]` on
/// the raw URL, which a non-loopback attacker host could satisfy:
/// `http://localhost.evil.com`, `http://127.0.0.1.evil.com`, and the userinfo
/// trick `http://127.0.0.1@evil.com` (the connection's real host is `evil.com`)
/// all matched, which disabled `https_only` and let credentials leave over
/// clear-text HTTP when the caller controlled the base URL. Parse the URL and
/// inspect the *actual* host instead, so only true loopback endpoints qualify:
///
/// - the scheme must be `http` — an `https` URL never needs the guard relaxed;
/// - the URL must carry no userinfo, rejecting the `user@host` smuggle;
/// - the host must be exactly `localhost`, `127.0.0.1`, or `::1`.
pub fn is_loopback_http_url(base_url: &str) -> bool {
    let url = match url::Url::parse(base_url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    // Any userinfo means the host after `@` is the real destination — reject so
    // `http://127.0.0.1@evil.com` cannot masquerade as loopback.
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    // A URL with no host at all (e.g. a `data:` / `mailto:` URL) is not loopback.
    let host = match url.host() {
        Some(h) => h,
        None => return false,
    };
    // Relax the guard only for clear-text http whose host is *exactly* a
    // loopback identity. Compare the parsed host, not the raw string, so a
    // lookalike domain like `localhost.evil.com` cannot match; `Ipv4Addr` /
    // `Ipv6Addr::LOCALHOST` are exactly `127.0.0.1` / `::1`, not the wider
    // loopback ranges.
    url.scheme() == "http"
        && match host {
            Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
            Host::Ipv4(addr) => addr == Ipv4Addr::LOCALHOST,
            Host::Ipv6(addr) => addr == Ipv6Addr::LOCALHOST,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_genuine_loopback() {
        assert!(is_loopback_http_url("http://localhost:4000"));
        assert!(is_loopback_http_url("http://localhost")); // no port
        assert!(is_loopback_http_url("http://127.0.0.1:4000"));
        assert!(is_loopback_http_url("http://[::1]:4000"));
        assert!(is_loopback_http_url("http://LOCALHOST:4000")); // host is case-insensitive
    }

    #[test]
    fn rejects_lookalike_hosts() {
        // A host that merely *starts with* the loopback literal but resolves to
        // the attacker's domain must NOT be treated as loopback.
        assert!(!is_loopback_http_url("http://localhost.evil.com"));
        assert!(!is_loopback_http_url("http://127.0.0.1.evil.com"));
        // The userinfo trick: the connection's real host is evil.com.
        assert!(!is_loopback_http_url("http://127.0.0.1@evil.com"));
        assert!(!is_loopback_http_url("http://localhost@evil.com"));
        // Plain non-loopback host.
        assert!(!is_loopback_http_url("http://evil.com"));
    }

    #[test]
    fn rejects_https_userinfo_and_garbage() {
        // https never needs the guard relaxed (stays https_only).
        assert!(!is_loopback_http_url("https://localhost:4000"));
        // Even a genuine loopback host is rejected when it carries userinfo.
        assert!(!is_loopback_http_url("http://user:pass@127.0.0.1:4000"));
        // A URL with no host component (data:/mailto:) is not loopback.
        assert!(!is_loopback_http_url("data:text/plain,hello"));
        assert!(!is_loopback_http_url("mailto:dev@example.com"));
        // Non-URL input is not loopback.
        assert!(!is_loopback_http_url("not a url"));
    }
}
