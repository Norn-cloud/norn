//! NET-ADDR-1: the one set of address rules for every outbound call a cell
//! makes. A source base URL is checked by its literal host (SYNC-URL-1,
//! [`crate::sources`]); a connection's egress client, and the native host's
//! source sync client (#1710), check every address a name resolves to, after
//! resolution, and connect only to an address they checked
//! (`norn_cell_do::connections::egress`, `norn_cell_rivet::egress`,
//! EGRESS-PIN-1).

/// Whether `ip` is internal by [`internal_ipv4`] or [`internal_ipv6`].
#[must_use]
pub fn internal_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => internal_ipv4(ip),
        std::net::IpAddr::V6(ip) => internal_ipv6(ip),
    }
}

/// A host that names the cell's own machine or network rather than a public
/// service: `localhost`, a single-label name, or an internal IP literal.
#[must_use]
pub fn internal_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(domain) => {
            const DOT_LOCALHOST: &[u8] = b".localhost";
            let domain = domain.trim_end_matches('.');
            let is_localhost_sub = domain.len() >= DOT_LOCALHOST.len()
                && domain
                    .as_bytes()
                    .get(domain.len().saturating_sub(DOT_LOCALHOST.len())..)
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(DOT_LOCALHOST));
            domain.eq_ignore_ascii_case("localhost") || is_localhost_sub || !domain.contains('.')
        }
        url::Host::Ipv4(ip) => internal_ipv4(*ip),
        url::Host::Ipv6(ip) => internal_ipv6(*ip),
    }
}

/// An IPv4 address no outbound call may reach: unspecified, loopback,
/// private, link-local (cloud metadata), broadcast, multicast, this-network,
/// shared (CGNAT), protocol-assignment, benchmarking or reserved.
#[must_use]
pub fn internal_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        // 0.0.0.0/8 ("this network") and 100.64.0.0/10 (shared address space).
        || a == 0
        || (a == 100 && (b & 0xc0) == 64)
        // 192.0.0.0/24 (protocol assignments), 198.18.0.0/15 (benchmarking)
        // and 240.0.0.0/4 (reserved).
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b & 0xfe) == 18)
        || a >= 240
}

/// An IPv6 address no outbound call may reach: unspecified, loopback,
/// multicast, unique-local, link-local, site-local, or one that carries or
/// reaches an IPv4 destination (mapped, compatible, NAT64, 6to4, Teredo).
#[must_use]
pub fn internal_ipv6(ip: std::net::Ipv6Addr) -> bool {
    let segments = ip.segments();
    let [first, second, third, ..] = segments;
    // Addresses that carry or reach an IPv4 destination are refused whatever
    // the IPv4 part: IPv4-mapped (::ffff:0:0/96), IPv4-translated
    // (::ffff:0:0:0/96), IPv4-compatible (::/96), NAT64 (64:ff9b::/96 and the
    // local-use 64:ff9b:1::/48), 6to4 (2002::/16) and Teredo (2001::/32).
    let carries_ipv4 = matches!(segments, [0, 0, 0, 0, 0, 0xffff | 0, _, _])
        || matches!(segments, [0, 0, 0, 0, 0xffff, 0, _, _])
        || matches!(segments, [0x64, 0xff9b, 0, 0, 0, 0, _, _])
        || (first == 0x64 && second == 0xff9b && third == 1)
        || first == 0x2002
        || (first == 0x2001 && second == 0);
    // Non-globally-reachable special-purpose IPv6 ranges: discard-only
    // (100::/64), benchmarking (2001:2::/48), ORCHID / ORCHIDv2
    // (2001:10::/28, 2001:20::/28), and documentation (2001:db8::/32,
    // 3fff::/20).
    let special_reserved = matches!(segments, [0x0100, 0, 0, 0, ..])
        || (first == 0x2001 && second == 0x0002 && third == 0)
        || (first == 0x2001 && matches!(second & 0xfff0, 0x0010 | 0x0020))
        || (first == 0x2001 && second == 0x0db8)
        || (first == 0x3fff && (second & 0xf000) == 0);
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || carries_ipv4
        || special_reserved
        // fc00::/7 unique-local, fe80::/10 link-local, fec0::/10 site-local.
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "literal test IP addresses")]
mod tests {
    use super::*;

    #[test]
    fn internal_host_and_ipv6_cover_case_and_reserved_ranges() {
        for host in [
            "LOCALHOST",
            "Localhost.",
            ".localhost",
            "api.LOCALHOST",
            "sub.LocalHost.",
        ] {
            assert!(internal_host(&url::Host::Domain(host)), "{host}");
        }
        assert!(!internal_host(&url::Host::Domain("api.example.com")));
        for addr in [
            "::ffff:0:127.0.0.1",
            "100::1",
            "2001:2::1",
            "2001:10::1",
            "2001:20::1",
            "2001:db8::1",
            "3fff::1",
        ] {
            let ip: std::net::Ipv6Addr = addr.parse().expect("valid IPv6 literal");
            assert!(internal_ipv6(ip), "{addr}");
        }
        let public: std::net::Ipv6Addr = "2606:2800:220:1::1".parse().expect("valid IPv6");
        assert!(!internal_ipv6(public));
    }
}
