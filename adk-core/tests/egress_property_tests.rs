//! Property tests for `adk_core::egress`: `decide` over random policies and targets.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use adk_core::egress::{
    AddressClass, EgressPolicy, EgressRule, EgressTarget, HostPattern, classify,
};
use proptest::prelude::*;

const LABELS: &[&str] = &["api", "www", "example", "test", "internal", "a"];
const SUFFIXES: &[&str] = &["com", "test", "org"];
const PORTS: &[u16] = &[22, 80, 443, 5432, 8080];
const ADDRESSES: &[&str] = &[
    "93.184.216.34",
    "8.8.8.8",
    "10.0.0.5",
    "10.1.2.3",
    "127.0.0.1",
    "192.168.1.10",
    "169.254.169.254",
    "168.63.129.16",
    "100.64.0.1",
    "198.18.0.1",
    "::1",
    "fd00:ec2::254",
    "fd12::1",
    "2606:4700::1111",
    "::ffff:10.0.0.5",
    "::ffff:93.184.216.34",
];
const CIDRS: &[&str] =
    &["10.0.0.0/8", "10.0.0.0/24", "127.0.0.0/8", "0.0.0.0/0", "fd00::/8", "::/0"];

fn host() -> impl Strategy<Value = String> {
    (prop::collection::vec(prop::sample::select(LABELS), 0..3), prop::sample::select(SUFFIXES))
        .prop_map(|(labels, suffix)| {
            let mut parts: Vec<&str> = labels;
            parts.push(suffix);
            parts.join(".")
        })
}

fn port() -> impl Strategy<Value = Option<u16>> {
    prop::option::of(prop::sample::select(PORTS))
}

fn rule() -> impl Strategy<Value = EgressRule> {
    prop_oneof![
        port().prop_map(|port| EgressRule::Host { pattern: HostPattern::Any, port }),
        (host(), port())
            .prop_map(|(host, port)| EgressRule::Host { pattern: HostPattern::Exact(host), port }),
        (host(), port()).prop_map(|(host, port)| EgressRule::Host {
            pattern: HostPattern::Subdomains(host),
            port
        }),
        (prop::sample::select(CIDRS), port())
            .prop_map(|(cidr, port)| EgressRule::Cidr { cidr: cidr.parse().unwrap(), port }),
    ]
}

fn policy() -> impl Strategy<Value = EgressPolicy> {
    (prop::collection::vec(rule(), 0..5), any::<bool>(), any::<bool>()).prop_map(
        |(rules, private, http)| {
            let mut policy = rules.into_iter().fold(EgressPolicy::deny_all(), EgressPolicy::allow);
            if private {
                policy = policy.with_private_networks();
            }
            if http {
                policy = policy.with_plain_http();
            }
            policy
        },
    )
}

fn address() -> impl Strategy<Value = IpAddr> {
    prop_oneof![
        prop::sample::select(ADDRESSES).prop_map(|a| a.parse().unwrap()),
        any::<u32>().prop_map(|bits| IpAddr::V4(Ipv4Addr::from(bits))),
        any::<u128>().prop_map(|bits| IpAddr::V6(Ipv6Addr::from(bits))),
    ]
}

#[derive(Debug, Clone)]
enum Target {
    Name(String, u16),
    Address(String, IpAddr),
    Literal(SocketAddr),
}

impl Target {
    fn as_target(&self) -> EgressTarget<'_> {
        match self {
            Self::Name(host, port) => EgressTarget::Name { host, port: *port },
            Self::Address(host, address) => EgressTarget::Address { host, address: *address },
            Self::Literal(address) => EgressTarget::Literal(*address),
        }
    }
}

fn target() -> impl Strategy<Value = Target> {
    prop_oneof![
        (host(), prop::sample::select(PORTS)).prop_map(|(h, p)| Target::Name(h, p)),
        (host(), address()).prop_map(|(h, a)| Target::Address(h, a)),
        (address(), prop::sample::select(PORTS))
            .prop_map(|(a, p)| Target::Literal(SocketAddr::new(a, p))),
    ]
}

/// The admission rules of `adk_core::egress`, written out case by case.
fn reference(rules: &[EgressRule], private: bool, target: &Target) -> bool {
    let port_ok = |allowed: &Option<u16>, port: u16| allowed.is_none_or(|allowed| allowed == port);
    let address_ok = |address: IpAddr| {
        let class = classify(address);
        class != AddressClass::Metadata
            && (class == AddressClass::Public
                || private
                || rules.iter().any(
                    |rule| matches!(rule, EgressRule::Cidr { cidr, .. } if cidr.contains(address)),
                ))
    };
    match target {
        Target::Name(host, port) => rules.iter().any(|rule| {
            matches!(rule, EgressRule::Host { pattern, port: allowed }
                if pattern.matches(host) && port_ok(allowed, *port))
        }),
        Target::Address(_, address) => address_ok(*address),
        Target::Literal(address) => {
            let ip = address.ip();
            let in_range = rules.iter().any(|rule| {
                matches!(rule, EgressRule::Cidr { cidr, port: allowed }
                    if cidr.contains(ip) && port_ok(allowed, address.port()))
            });
            let any_host = rules.iter().any(|rule| {
                matches!(rule, EgressRule::Host { pattern: HostPattern::Any, port: allowed }
                    if port_ok(allowed, address.port()))
            });
            classify(ip) != AddressClass::Metadata && (in_range || (any_host && address_ok(ip)))
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `narrow` is an exact intersection: a target passes the result iff it passes both.
    #[test]
    fn narrow_is_the_intersection(a in policy(), b in policy(), t in target()) {
        let target = t.as_target();
        let both = a.decide(&target).is_ok() && b.decide(&target).is_ok();
        prop_assert_eq!(a.narrow(&b).decide(&target).is_ok(), both);
        prop_assert_eq!(b.narrow(&a).decide(&target).is_ok(), both);
    }

    /// Builder methods on a narrowed policy never admit what either input refused.
    #[test]
    fn widening_a_narrowed_policy_stays_inside_the_ceiling(
        ceiling in policy(),
        lower in policy(),
        extra in prop::collection::vec(rule(), 0..4),
        t in target(),
    ) {
        let target = t.as_target();
        let widened = extra
            .into_iter()
            .fold(ceiling.narrow(&lower), EgressPolicy::allow)
            .with_private_networks()
            .with_plain_http();
        if widened.decide(&target).is_ok() {
            prop_assert!(lower.decide(&target).is_ok());
            prop_assert!(ceiling.decide(&target).is_ok());
        }
    }

    /// No policy admits a cloud metadata address, however it is reached.
    #[test]
    fn metadata_is_never_admitted(p in policy(), h in host(), port in prop::sample::select(PORTS)) {
        let p = p.with_private_networks().allow("*".parse().unwrap()).allow("::/0".parse().unwrap());
        for metadata in ["169.254.169.254", "168.63.129.16", "fd00:ec2::254", "::ffff:169.254.169.254"] {
            let address: IpAddr = metadata.parse().unwrap();
            let resolved = EgressTarget::Address { host: &h, address };
            let literal = EgressTarget::Literal(SocketAddr::new(address, port));
            prop_assert!(p.decide(&resolved).is_err());
            prop_assert!(p.decide(&literal).is_err());
        }
    }

    /// A single-layer policy decides exactly as the reference model in the module docs.
    #[test]
    fn decide_matches_the_reference_model(
        rules in prop::collection::vec(rule(), 0..5),
        private in any::<bool>(),
        t in target(),
    ) {
        let mut policy = rules.iter().cloned().fold(EgressPolicy::deny_all(), EgressPolicy::allow);
        if private {
            policy = policy.with_private_networks();
        }
        prop_assert_eq!(policy.decide(&t.as_target()).is_ok(), reference(&rules, private, &t));
    }

    /// `*.domain` admits exactly the names that end in `.domain`.
    #[test]
    fn subdomain_wildcards_match_by_label(domain in host(), candidate in host()) {
        let pattern = HostPattern::Subdomains(domain.clone());
        prop_assert_eq!(pattern.matches(&candidate), candidate.ends_with(&format!(".{domain}")));
    }

    /// Rules survive their text form.
    #[test]
    fn rules_round_trip_through_text(r in rule()) {
        prop_assert_eq!(r.to_string().parse::<EgressRule>().unwrap(), r);
    }

    /// IPv6 forms that embed an IPv4 address classify as that address.
    #[test]
    fn embedded_ipv4_classifies_as_itself(bits in any::<u32>()) {
        let v4 = Ipv4Addr::from(bits);
        let [hi, lo] = [(bits >> 16) as u16, bits as u16];
        let mapped = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, hi, lo);
        let nat64 = Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, hi, lo);
        let six_to_four = Ipv6Addr::new(0x2002, hi, lo, 0, 0, 0, 0, 1);
        let class = classify(IpAddr::V4(v4));
        prop_assert_eq!(classify(IpAddr::V6(mapped)), class);
        prop_assert_eq!(classify(IpAddr::V6(nat64)), class);
        prop_assert_eq!(classify(IpAddr::V6(six_to_four)), class);
    }
}
