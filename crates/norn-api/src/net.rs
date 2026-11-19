//! NET-ADDR-1: the one set of address rules for every outbound call a cell
//! makes. A source base URL is checked by its literal host (SYNC-URL-1,
//! [`crate::sources`]); a connection's egress client checks every address a
//! name resolves to, after resolution, and connects only to an address it
//! checked (`norn_cell_do::connections::egress`, EGRESS-PIN-1).

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
            let domain = domain.trim_end_matches('.');
            domain == "localhost" || domain.ends_with(".localhost") || !domain.contains('.')
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
    // the IPv4 part: IPv4-mapped (::ffff:0:0/96), IPv4-compatible (::/96),
    // NAT64 (64:ff9b::/96 and the local-use 64:ff9b:1::/48), 6to4
    // (2002::/16) and Teredo (2001::/32).
    let carries_ipv4 = matches!(segments, [0, 0, 0, 0, 0, 0xffff | 0, _, _])
        || matches!(segments, [0x64, 0xff9b, 0, 0, 0, 0, _, _])
        || (first == 0x64 && second == 0xff9b && third == 1)
        || first == 0x2002
        || (first == 0x2001 && second == 0);
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || carries_ipv4
        // fc00::/7 unique-local, fe80::/10 link-local, fec0::/10 site-local.
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
}
