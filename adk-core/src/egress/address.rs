//! The one address classifier every egress check uses.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// The kind of network an IP address reaches.
///
/// An IPv6 address that embeds an IPv4 address — IPv4-mapped (`::ffff:0:0/96`), NAT64
/// (`64:ff9b::/96`) or 6to4 (`2002::/16`) — has the class of the address it embeds, so
/// `::ffff:169.254.169.254` is [`Metadata`](Self::Metadata).
///
/// | Class | IPv4 | IPv6 |
/// |-------|------|------|
/// | `Metadata` | `169.254.169.254`, `169.254.170.2`, `169.254.170.23`, `100.100.100.200`, `168.63.129.16` | `fd00:ec2::254`, `fd00:ec2::23` |
/// | `Unspecified` | `0.0.0.0/8` | `::` |
/// | `Loopback` | `127.0.0.0/8` | `::1` |
/// | `Private` | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | `fc00::/7`, `fec0::/10` |
/// | `LinkLocal` | `169.254.0.0/16` | `fe80::/10` |
/// | `SharedAddressSpace` | `100.64.0.0/10` | — |
/// | `Multicast` | `224.0.0.0/4` | `ff00::/8` |
/// | `Reserved` | `240.0.0.0/4` (broadcast included), `198.18.0.0/15`, `192.0.0.0/24` | `100::/64`, IPv4-compatible `::/96` and IPv4-translated `::ffff:0:0:0/96` |
/// | `Tunnel` | — | Teredo `2001::/32`, local-use NAT64 `64:ff9b:1::/48` |
/// | `Public` | everything else | everything else |
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{AddressClass, classify};
///
/// assert_eq!(classify("93.184.216.34".parse().unwrap()), AddressClass::Public);
/// assert_eq!(classify("10.1.2.3".parse().unwrap()), AddressClass::Private);
/// assert_eq!(classify("168.63.129.16".parse().unwrap()), AddressClass::Metadata);
/// assert_eq!(classify("::ffff:169.254.169.254".parse().unwrap()), AddressClass::Metadata);
/// assert_eq!(classify("2002:c0a8:0101::1".parse().unwrap()), AddressClass::Private);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AddressClass {
    /// A globally routed address.
    Public,
    /// A cloud metadata or platform credential endpoint. No egress policy admits it.
    Metadata,
    /// "This network" or the unspecified address, which reaches the local host on common platforms.
    Unspecified,
    /// The local host.
    Loopback,
    /// RFC 1918 private space, IPv6 unique-local and deprecated site-local space.
    Private,
    /// Link-local space.
    LinkLocal,
    /// Carrier-grade NAT shared address space.
    SharedAddressSpace,
    /// Multicast and broadcast.
    Multicast,
    /// Space reserved for future use, benchmarking, protocol assignments or discard.
    Reserved,
    /// A transition tunnel whose far end the address does not name reliably.
    Tunnel,
}

impl AddressClass {
    /// Whether the class is [`Public`](Self::Public).
    pub fn is_public(self) -> bool {
        self == Self::Public
    }
}

impl fmt::Display for AddressClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Public => "public",
            Self::Metadata => "cloud metadata",
            Self::Unspecified => "unspecified",
            Self::Loopback => "loopback",
            Self::Private => "private",
            Self::LinkLocal => "link-local",
            Self::SharedAddressSpace => "shared (carrier-grade NAT)",
            Self::Multicast => "multicast",
            Self::Reserved => "reserved",
            Self::Tunnel => "tunnel",
        })
    }
}

const METADATA_V4: [Ipv4Addr; 5] = [
    // AWS, GCP, Azure, Oracle and DigitalOcean instance metadata.
    Ipv4Addr::new(169, 254, 169, 254),
    // AWS ECS task metadata and credentials.
    Ipv4Addr::new(169, 254, 170, 2),
    // AWS EKS Pod Identity agent.
    Ipv4Addr::new(169, 254, 170, 23),
    // Alibaba Cloud instance metadata.
    Ipv4Addr::new(100, 100, 100, 200),
    // Azure WireServer, which sits in public space.
    Ipv4Addr::new(168, 63, 129, 16),
];

const METADATA_V6: [Ipv6Addr; 2] = [
    // AWS instance metadata over IPv6.
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
    // AWS EKS Pod Identity agent over IPv6.
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0023),
];

/// Classifies `ip`; see [`AddressClass`] for the ranges.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{AddressClass, classify};
///
/// assert_eq!(classify("127.0.0.1".parse().unwrap()), AddressClass::Loopback);
/// assert_eq!(classify("64:ff9b::a00:1".parse().unwrap()), AddressClass::Private);
/// assert_eq!(classify("2001:0:4136:e378::1".parse().unwrap()), AddressClass::Tunnel);
/// ```
pub fn classify(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

/// Whether `ip` is anything but [`AddressClass::Public`].
///
/// # Example
///
/// ```rust
/// use adk_core::egress::is_non_public;
///
/// assert!(is_non_public("10.1.2.3".parse().unwrap()));
/// assert!(is_non_public("::ffff:127.0.0.1".parse().unwrap()));
/// assert!(is_non_public("198.18.0.1".parse().unwrap()));
/// assert!(!is_non_public("93.184.216.34".parse().unwrap()));
/// ```
pub fn is_non_public(ip: IpAddr) -> bool {
    !classify(ip).is_public()
}

fn classify_v4(v4: Ipv4Addr) -> AddressClass {
    if METADATA_V4.contains(&v4) {
        return AddressClass::Metadata;
    }
    let [a, b, c, _] = v4.octets();
    if a == 0 {
        AddressClass::Unspecified
    } else if v4.is_loopback() {
        AddressClass::Loopback
    } else if v4.is_private() {
        AddressClass::Private
    } else if v4.is_link_local() {
        AddressClass::LinkLocal
    } else if a == 100 && (64..=127).contains(&b) {
        AddressClass::SharedAddressSpace
    } else if v4.is_multicast() {
        AddressClass::Multicast
    } else if a >= 240 || (a == 198 && (b == 18 || b == 19)) || (a == 192 && b == 0 && c == 0) {
        AddressClass::Reserved
    } else {
        AddressClass::Public
    }
}

fn classify_v6(v6: Ipv6Addr) -> AddressClass {
    if METADATA_V6.contains(&v6) {
        return AddressClass::Metadata;
    }
    if v6.is_unspecified() {
        return AddressClass::Unspecified;
    }
    if v6.is_loopback() {
        return AddressClass::Loopback;
    }
    let s = v6.segments();
    let low_v4 = Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7]));
    // IPv4-mapped (::ffff:a.b.c.d) and NAT64 (64:ff9b::a.b.c.d) reach the IPv4 address.
    if s[..6] == [0, 0, 0, 0, 0, 0xffff] || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return classify_v4(low_v4);
    }
    // 6to4 (2002:a.b.c.d::/48) tunnels to the IPv4 address in bits 16..48.
    if s[0] == 0x2002 {
        return classify_v4(Ipv4Addr::from((u32::from(s[1]) << 16) | u32::from(s[2])));
    }
    // IPv4-compatible (::a.b.c.d) and IPv4-translated (::ffff:0:a.b.c.d) forms are not routed
    // as such; they take a non-public embedded class and are reserved otherwise.
    if s[..6] == [0, 0, 0, 0, 0, 0] || s[..6] == [0, 0, 0, 0, 0xffff, 0] {
        return match classify_v4(low_v4) {
            AddressClass::Public => AddressClass::Reserved,
            embedded => embedded,
        };
    }
    if (s[0] == 0x2001 && s[1] == 0) || s[..3] == [0x64, 0xff9b, 1] {
        AddressClass::Tunnel
    } else if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfec0 {
        AddressClass::Private
    } else if (s[0] & 0xffc0) == 0xfe80 {
        AddressClass::LinkLocal
    } else if v6.is_multicast() {
        AddressClass::Multicast
    } else if s[..4] == [0x100, 0, 0, 0] {
        AddressClass::Reserved
    } else {
        AddressClass::Public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_listed_range() {
        let cases: &[(&str, AddressClass)] = &[
            ("93.184.216.34", AddressClass::Public),
            ("8.8.8.8", AddressClass::Public),
            ("203.0.113.1", AddressClass::Public),
            ("2606:4700::1111", AddressClass::Public),
            ("169.254.169.254", AddressClass::Metadata),
            ("169.254.170.2", AddressClass::Metadata),
            ("169.254.170.23", AddressClass::Metadata),
            ("100.100.100.200", AddressClass::Metadata),
            ("168.63.129.16", AddressClass::Metadata),
            ("fd00:ec2::254", AddressClass::Metadata),
            ("fd00:ec2::23", AddressClass::Metadata),
            ("::ffff:169.254.169.254", AddressClass::Metadata),
            ("64:ff9b::a9fe:a9fe", AddressClass::Metadata),
            ("2002:a9fe:a9fe::1", AddressClass::Metadata),
            ("::ffff:168.63.129.16", AddressClass::Metadata),
            ("0.0.0.0", AddressClass::Unspecified),
            ("0.1.2.3", AddressClass::Unspecified),
            ("::", AddressClass::Unspecified),
            ("127.0.0.1", AddressClass::Loopback),
            ("127.255.0.9", AddressClass::Loopback),
            ("::1", AddressClass::Loopback),
            ("::ffff:127.0.0.1", AddressClass::Loopback),
            ("10.0.0.1", AddressClass::Private),
            ("172.16.0.1", AddressClass::Private),
            ("172.31.255.255", AddressClass::Private),
            ("192.168.1.1", AddressClass::Private),
            ("fd12:3456::1", AddressClass::Private),
            ("fec0::1", AddressClass::Private),
            ("2002:c0a8:0101::1", AddressClass::Private),
            ("64:ff9b::a00:1", AddressClass::Private),
            ("169.254.1.1", AddressClass::LinkLocal),
            ("fe80::1", AddressClass::LinkLocal),
            ("100.64.0.1", AddressClass::SharedAddressSpace),
            ("100.127.255.255", AddressClass::SharedAddressSpace),
            ("224.0.0.1", AddressClass::Multicast),
            ("ff02::1", AddressClass::Multicast),
            ("240.0.0.1", AddressClass::Reserved),
            ("255.255.255.255", AddressClass::Reserved),
            ("198.18.0.1", AddressClass::Reserved),
            ("198.19.255.255", AddressClass::Reserved),
            ("192.0.0.8", AddressClass::Reserved),
            ("100::1", AddressClass::Reserved),
            ("::5db8:d822", AddressClass::Reserved),
            ("::a00:1", AddressClass::Private),
            ("::ffff:0:5db8:d822", AddressClass::Reserved),
            ("2001:0:4136:e378:8000:63bf:3fff:fdd2", AddressClass::Tunnel),
            ("64:ff9b:1::1", AddressClass::Tunnel),
        ];
        let wrong: Vec<_> = cases
            .iter()
            .filter(|(ip, class)| classify(ip.parse().unwrap()) != *class)
            .map(|(ip, class)| {
                format!("{ip}: expected {class}, got {}", classify(ip.parse().unwrap()))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn neighbours_of_the_reserved_ranges_stay_public() {
        for ip in ["100.63.255.255", "100.128.0.0", "198.17.255.255", "198.20.0.0", "192.0.1.1"] {
            assert_eq!(classify(ip.parse().unwrap()), AddressClass::Public, "{ip}");
        }
    }
}
