//! web_fetch must not become a way to reach the user's own machine or network (localhost services, cloud metadata at
//! 169.254.169.254, LAN devices).
use std::net::{IpAddr, ToSocketAddrs};

fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || v.is_broadcast() || v.is_multicast() || v.is_unspecified() || v.octets()[0] == 0 || v.octets()[0] >= 240,
        IpAddr::V6(v) => {
            let seg = v.segments();
            v.is_loopback() || v.is_multicast() || v.is_unspecified() || (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Reason the URL must not be fetched, or None.
pub fn check_url(url: &str, allow_private: bool) -> Option<String> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = hostport.rsplit('@').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.ends_with(']') || (hostport.starts_with('[') && p.chars().all(|c| c.is_ascii_digit()) && h.ends_with(']')) => {
            (h.trim_matches(|c| c == '[' || c == ']'), p.parse::<u16>().ok())
        }
        _ => (hostport.trim_matches(|c| c == '[' || c == ']'), None),
    };
    if !(scheme == "http" || scheme == "https") || host.is_empty() {
        return Some("URL must be http:// or https://".into());
    }
    if allow_private {
        return None;
    }
    let low = host.to_lowercase();
    if low == "localhost" || low.ends_with(".localhost") || low.ends_with(".local") || low.ends_with(".internal") {
        return Some(format!("'{host}' is a local address"));
    }
    // ponytail: resolve-then-fetch leaves a DNS-rebinding window; pin the IP if this ever fronts untrusted input
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    let Ok(addrs) = (host, port).to_socket_addrs() else { return None }; // unresolvable: the request itself will fail clearly
    for a in addrs {
        if is_private(&a.ip()) {
            return Some(format!("'{host}' resolves to a private or local address ({})", a.ip()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_local() {
        assert!(check_url("http://localhost:8000/x", false).is_some());
        assert!(check_url("http://127.0.0.1/", false).is_some());
        assert!(check_url("http://169.254.169.254/latest", false).is_some());
        assert!(check_url("ftp://x.com", false).is_some());
        assert!(check_url("http://127.0.0.1/", true).is_none());
    }
}
