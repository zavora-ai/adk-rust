//! Network egress policy: which hosts, ports and addresses a connection may reach.
//!
//! An [`EgressPolicy`] is a set of [`EgressRule`]s plus two switches, and [`decide`] is a
//! pure function from a policy and an [`EgressTarget`] to an allow or an [`EgressDenial`].
//! A connection by host name passes two checks: the name and port before resolution, and
//! every address the name resolves to at connect time.
//!
//! | Rule | Example | Admits |
//! |------|---------|--------|
//! | Any host | `*`, `*:443` | Every host name, and IP literals whose address passes the address check |
//! | Exact host | `api.example.com`, `api.example.com:443` | That name |
//! | Subdomains | `*.example.com`, `*.example.com:443` | Every name below `example.com`, not `example.com` itself |
//! | CIDR | `10.0.0.0/8`, `10.0.0.5:5432`, `[fd00::/8]:443` | IP literals in the range on the port, and names that resolve into the range |
//!
//! | Address check | Outcome |
//! |---------------|---------|
//! | [`AddressClass::Metadata`] (cloud metadata and credential endpoints) | Always refused, whatever the rules say |
//! | [`AddressClass::Public`] | Admitted |
//! | Any other class (loopback, private, link-local, ...) | Admitted only inside a CIDR rule, or when [`EgressPolicy::with_private_networks`] is set |
//!
//! A rule without a port admits every port. Plain `http` URLs are refused unless
//! [`EgressPolicy::with_plain_http`] is set. A host name whose last label is numeric, such as
//! `0x7f.1`, is refused because resolvers read it as an IPv4 address.
//!
//! [`EgressPolicy::narrow`] intersects two policies: a destination passes the result only when
//! it passes both. An operator's policy is the ceiling, and every lower layer narrows it.
//!
//! [`GuardedClient`] (feature `egress-client`) is an HTTP client that enforces a policy on
//! every request URL, every resolved address and every redirect hop, ignores the environment's
//! proxy settings, and is never replaced by an unguarded client when it cannot be built.
//!
//! # Limits
//!
//! - **Not data-loss prevention**: an admitted host receives whatever is sent to it.
//! - **Not proof against domain fronting**: a host rule matches the name a client asks for;
//!   a shared CDN can route that connection to another tenant.
//! - **Only checked connections are covered**: a socket opened directly by code that does not
//!   consult the policy, including third-party code in the agent's process, is not contained.
//!
//! # Example
//!
//! ```rust
//! use adk_core::egress::{EgressPolicy, EgressTarget};
//!
//! let operator = EgressPolicy::allowlist(["*.example.com:443", "api.github.com:443"]).unwrap();
//! let tool = EgressPolicy::allowlist(["api.github.com"]).unwrap();
//! let effective = operator.narrow(&tool);
//!
//! assert!(effective.decide(&EgressTarget::Name { host: "api.github.com", port: 443 }).is_ok());
//! assert!(effective.decide(&EgressTarget::Name { host: "api.github.com", port: 80 }).is_err());
//! assert!(effective.decide(&EgressTarget::Name { host: "docs.example.com", port: 443 }).is_err());
//! assert!(
//!     effective
//!         .decide(&EgressTarget::Address {
//!             host: "api.github.com",
//!             address: "10.0.0.7".parse().unwrap(),
//!         })
//!         .is_err()
//! );
//! ```
//!
//! [`decide`]: EgressPolicy::decide

mod address;
#[cfg(feature = "egress-client")]
mod client;

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use address::{AddressClass, classify, is_non_public};
#[cfg(feature = "egress-client")]
pub use client::{EgressClientError, EgressResolver, GuardedClient, GuardedClientBuilder};

/// Which host names a [`EgressRule::Host`] admits.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::HostPattern;
///
/// let pattern: HostPattern = "*.example.com".parse().unwrap();
/// assert!(pattern.matches("api.example.com"));
/// assert!(!pattern.matches("example.com"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HostPattern {
    /// Every host name.
    Any,
    /// One host name, lowercase, without a trailing dot.
    Exact(String),
    /// Every name below this domain (written `*.domain`), not the domain itself.
    Subdomains(String),
}

impl HostPattern {
    /// Whether `host` matches; `host` is compared case-insensitively, without a trailing dot.
    pub fn matches(&self, host: &str) -> bool {
        let host = normalize_host(host);
        match self {
            Self::Any => true,
            Self::Exact(name) => host == *name,
            Self::Subdomains(domain) => {
                host.len() > domain.len() + 1
                    && host.ends_with(domain.as_str())
                    && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
            }
        }
    }
}

impl FromStr for HostPattern {
    type Err = EgressRuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| EgressRuleError::new(s, reason);
        let host = normalize_host(s);
        if host == "*" {
            return Ok(Self::Any);
        }
        let (domain, wildcard) = match host.strip_prefix("*.") {
            Some(domain) => (domain, true),
            None => (host.as_str(), false),
        };
        if domain.is_empty() || domain.len() > 253 {
            return Err(invalid("a host name has 1 to 253 characters"));
        }
        for label in domain.split('.') {
            if label.is_empty() || label.len() > 63 {
                return Err(invalid("every label of a host name has 1 to 63 characters"));
            }
            if !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Err(invalid(
                    "a host name holds letters, digits, '-' and '_' only; write \
                     internationalized names in their xn-- form, and a wildcard only as a \
                     leading '*.'",
                ));
            }
        }
        if is_numeric_host(domain) {
            return Err(invalid("a numeric host name is read as an IPv4 address; write a CIDR"));
        }
        Ok(if wildcard { Self::Subdomains(domain.to_string()) } else { Self::Exact(host) })
    }
}

impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Exact(name) => f.write_str(name),
            Self::Subdomains(domain) => write!(f, "*.{domain}"),
        }
    }
}

/// An IP address range: a network address and a prefix length.
///
/// An IPv4-mapped IPv6 range (`::ffff:a.b.c.d/96+`) is stored as its IPv4 range, and
/// [`contains`](Self::contains) compares IPv4-mapped addresses as IPv4.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::IpCidr;
///
/// let cidr: IpCidr = "10.0.0.0/8".parse().unwrap();
/// assert!(cidr.contains("10.20.30.40".parse().unwrap()));
/// assert!(cidr.contains("::ffff:10.0.0.1".parse().unwrap()));
/// assert!(!cidr.contains("11.0.0.1".parse().unwrap()));
/// assert!("10.0.0.1/8".parse::<IpCidr>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

impl IpCidr {
    /// A range of `prefix` leading bits of `network`.
    ///
    /// # Errors
    ///
    /// Returns an error when `prefix` is longer than the address or `network` has bits set
    /// after the prefix.
    pub fn new(network: IpAddr, prefix: u8) -> Result<Self, EgressRuleError> {
        let (network, prefix) = match network {
            IpAddr::V6(v6) if prefix >= 96 => match v6.to_ipv4_mapped() {
                Some(v4) => (IpAddr::V4(v4), prefix - 96),
                None => (network, prefix),
            },
            IpAddr::V4(_) | IpAddr::V6(_) => (network, prefix),
        };
        let text = format!("{network}/{prefix}");
        let bits = match network {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix > bits {
            return Err(EgressRuleError::new(&text, "the prefix is longer than the address"));
        }
        let cidr = Self { network, prefix };
        if cidr.masked(network) != address_bits(network) {
            return Err(EgressRuleError::new(
                &text,
                "the address has bits set after the prefix; write the network address",
            ));
        }
        Ok(cidr)
    }

    /// The network address.
    pub fn network(&self) -> IpAddr {
        self.network
    }

    /// The prefix length in bits.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// Whether `ip` is inside the range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        let same_family = matches!(
            (self.network, ip),
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
        );
        same_family && self.masked(ip) == self.masked(self.network)
    }

    fn masked(&self, ip: IpAddr) -> u128 {
        let width: u32 = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let host_bits = width - u32::from(self.prefix).min(width);
        let bits = address_bits(ip);
        if host_bits >= 128 { 0 } else { (bits >> host_bits) << host_bits }
    }
}

fn address_bits(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(v4) => u128::from(u32::from(v4)),
        IpAddr::V6(v6) => u128::from(v6),
    }
}

impl FromStr for IpCidr {
    type Err = EgressRuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| EgressRuleError::new(s, reason);
        let (address, prefix) = match s.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (s, None),
        };
        let network: IpAddr = address.parse().map_err(|_| invalid("not an IP address"))?;
        let prefix = match prefix {
            Some(prefix) => prefix.parse().map_err(|_| invalid("the prefix is not a number"))?,
            None if network.is_ipv4() => 32,
            None => 128,
        };
        Self::new(network, prefix).map_err(|e| EgressRuleError { rule: s.to_string(), ..e })
    }
}

impl fmt::Display for IpCidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// One rule of an [`EgressPolicy`].
///
/// Rules parse from and display as text; a rule without a port admits every port.
///
/// | Text | Rule |
/// |------|------|
/// | `*`, `*:443` | Any host name |
/// | `api.example.com`, `api.example.com:443` | One host name |
/// | `*.example.com`, `*.example.com:443` | Every subdomain |
/// | `10.0.0.0/8`, `10.0.0.5:5432`, `fd00::/8`, `[fd00::/8]:443`, `[::1]:8080` | An address range |
///
/// # Example
///
/// ```rust
/// use adk_core::egress::EgressRule;
///
/// let rule: EgressRule = "*.example.com:443".parse().unwrap();
/// assert_eq!(rule.to_string(), "*.example.com:443");
/// assert_eq!(EgressRule::host("API.Example.com.").unwrap().on_port(8443).to_string(), "api.example.com:8443");
/// assert!("exa mple.com".parse::<EgressRule>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EgressRule {
    /// Admits host names.
    Host {
        /// The names admitted.
        pattern: HostPattern,
        /// The one port admitted, or every port.
        port: Option<u16>,
    },
    /// Admits an address range.
    Cidr {
        /// The range admitted.
        cidr: IpCidr,
        /// The one port admitted for IP literals in the range, or every port.
        port: Option<u16>,
    },
}

impl EgressRule {
    /// A rule admitting every host name on every port.
    pub fn any_host() -> Self {
        Self::Host { pattern: HostPattern::Any, port: None }
    }

    /// A rule admitting `host` (a name, or `*.domain` for its subdomains) on every port.
    ///
    /// # Errors
    ///
    /// Returns an error when `host` is not a valid host name or wildcard.
    pub fn host(host: &str) -> Result<Self, EgressRuleError> {
        Ok(Self::Host { pattern: host.parse()?, port: None })
    }

    /// A rule admitting `cidr` on every port.
    pub fn cidr(cidr: IpCidr) -> Self {
        Self::Cidr { cidr, port: None }
    }

    /// Restricts the rule to `port`.
    #[must_use]
    pub fn on_port(self, port: u16) -> Self {
        match self {
            Self::Host { pattern, .. } => Self::Host { pattern, port: Some(port) },
            Self::Cidr { cidr, .. } => Self::Cidr { cidr, port: Some(port) },
        }
    }

    fn admits_port(&self, port: u16) -> bool {
        match self {
            Self::Host { port: allowed, .. } | Self::Cidr { port: allowed, .. } => {
                allowed.is_none_or(|allowed| allowed == port)
            }
        }
    }
}

impl FromStr for EgressRule {
    type Err = EgressRuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| EgressRuleError::new(s, reason);
        let text = s.trim();
        let parse_port = |port: &str| match port.parse::<u16>() {
            Ok(0) | Err(_) => Err(invalid("the port is a number from 1 to 65535")),
            Ok(port) => Ok(port),
        };
        let at_port = |rule: Self, port: Option<&str>| match port {
            Some(port) => Ok(rule.on_port(parse_port(port)?)),
            None => Ok(rule),
        };
        if text.is_empty() {
            return Err(invalid("the rule is empty"));
        }
        if let Some(rest) = text.strip_prefix('[') {
            let (inner, after) =
                rest.split_once(']').ok_or_else(|| invalid("a '[' has no closing ']'"))?;
            let port = match after {
                "" => None,
                after => Some(
                    after.strip_prefix(':').ok_or_else(|| invalid("expected ':port' after ']'"))?,
                ),
            };
            let cidr = inner.parse().map_err(|e: EgressRuleError| invalid(&e.reason))?;
            return at_port(Self::cidr(cidr), port);
        }
        // More than one ':' is a bare IPv6 address or range, which takes no port.
        if text.matches(':').count() > 1 {
            let cidr = text.parse().map_err(|e: EgressRuleError| invalid(&e.reason))?;
            return Ok(Self::cidr(cidr));
        }
        let (host, port) = match text.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        };
        if host.contains('/') || host.parse::<Ipv4Addr>().is_ok() {
            let cidr = host.parse().map_err(|e: EgressRuleError| invalid(&e.reason))?;
            return at_port(Self::cidr(cidr), port);
        }
        let pattern = host.parse().map_err(|e: EgressRuleError| invalid(&e.reason))?;
        at_port(Self::Host { pattern, port: None }, port)
    }
}

impl fmt::Display for EgressRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host { pattern, port: None } => write!(f, "{pattern}"),
            Self::Host { pattern, port: Some(port) } => write!(f, "{pattern}:{port}"),
            Self::Cidr { cidr, port: None } => write!(f, "{cidr}"),
            Self::Cidr { cidr, port: Some(port) } if cidr.network.is_ipv6() => {
                write!(f, "[{cidr}]:{port}")
            }
            Self::Cidr { cidr, port: Some(port) } => write!(f, "{cidr}:{port}"),
        }
    }
}

impl Serialize for EgressRule {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EgressRule {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A rule that does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid egress rule '{rule}': {reason}. A rule is a host (api.example.com), a wildcard \
     (*.example.com), '*' for any host, or a CIDR (10.0.0.0/8), each optionally followed by \
     ':port'; write an IPv6 range with a port as [fd00::/8]:443"
)]
pub struct EgressRuleError {
    /// The rule text.
    pub rule: String,
    /// What is wrong with it.
    pub reason: String,
}

impl EgressRuleError {
    fn new(rule: &str, reason: &str) -> Self {
        Self { rule: rule.to_string(), reason: reason.to_string() }
    }
}

/// A destination [`EgressPolicy::decide`] checks.
///
/// A connection by name passes when both its [`Name`](Self::Name) and every
/// [`Address`](Self::Address) the name resolves to pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressTarget<'a> {
    /// A host name and port, before resolution. A host that is an IP address literal,
    /// bracketed or not, is checked as a [`Literal`](Self::Literal).
    Name {
        /// The host name.
        host: &'a str,
        /// The port.
        port: u16,
    },
    /// One address `host` resolved to, at connect time.
    Address {
        /// The host name that resolved to `address`, for the denial message.
        host: &'a str,
        /// The resolved address.
        address: IpAddr,
    },
    /// An IP address literal and port.
    Literal(SocketAddr),
}

/// Why an [`EgressPolicy`] refused a destination.
///
/// The `Display` form is the guidance a caller or a model reads; [`code`](Self::code) is
/// a stable identifier for audit records.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressDenial {
    /// No host rule admits the name and port.
    #[error(
        "egress to {host}:{port} is refused: no rule of the egress policy admits it; add a host \
         rule for it to the policy if it should be reachable"
    )]
    HostNotAllowed {
        /// The host name.
        host: String,
        /// The port.
        port: u16,
    },
    /// An IP address literal no rule admits.
    #[error(
        "egress to the IP address {address} is refused: no CIDR rule of the egress policy admits \
         it; connect by host name, or add a CIDR rule for the address"
    )]
    LiteralNotAllowed {
        /// The address and port.
        address: SocketAddr,
    },
    /// A non-public address outside every CIDR rule.
    #[error(
        "egress to {host} is refused: it reaches {address}, a {class} address; non-public \
         addresses need a CIDR rule for them or private networks allowed in the egress policy"
    )]
    AddressNotAllowed {
        /// The host name or literal.
        host: String,
        /// The address.
        address: IpAddr,
        /// The address's class.
        class: AddressClass,
    },
    /// A cloud metadata or credential endpoint, which no policy admits.
    #[error(
        "egress to {host} is refused: {address} is a cloud metadata or credential endpoint, \
         which is always blocked"
    )]
    MetadataAddress {
        /// The host name or literal.
        host: String,
        /// The address.
        address: IpAddr,
    },
    /// A host name some resolvers read as an IPv4 address.
    #[error(
        "egress to '{host}' is refused: a host name ending in a numeric label is read as an \
         IPv4 address by some resolvers; use the dotted-quad address"
    )]
    AmbiguousHost {
        /// The host name.
        host: String,
    },
    /// Plain `http` under a policy that requires `https`.
    #[error(
        "egress to {host} over plain http is refused; use https, or allow plain http in the \
         egress policy"
    )]
    PlainHttp {
        /// The host.
        host: String,
    },
    /// A URL scheme other than `http` or `https`.
    #[error("egress over the URL scheme '{scheme}' is refused; use https")]
    Scheme {
        /// The scheme.
        scheme: String,
    },
    /// A URL that does not parse or has no host.
    #[error("egress URL is invalid: {reason}")]
    InvalidUrl {
        /// What is wrong with it.
        reason: String,
    },
}

impl EgressDenial {
    /// A stable identifier for audit records, such as `egress.host_not_allowed`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::HostNotAllowed { .. } => "egress.host_not_allowed",
            Self::LiteralNotAllowed { .. } => "egress.literal_not_allowed",
            Self::AddressNotAllowed { .. } => "egress.address_not_allowed",
            Self::MetadataAddress { .. } => "egress.metadata_address",
            Self::AmbiguousHost { .. } => "egress.ambiguous_host",
            Self::PlainHttp { .. } => "egress.plain_http",
            Self::Scheme { .. } => "egress.scheme",
            Self::InvalidUrl { .. } => "egress.invalid_url",
        }
    }
}

/// The first [`EgressDenial`] in `error`'s source chain, such as the refusal inside a failed
/// [`GuardedClient`] request.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{EgressDenial, find_denial};
///
/// let denial = EgressDenial::AmbiguousHost { host: "0x7f.1".into() };
/// assert_eq!(find_denial(&denial), Some(&denial));
/// ```
pub fn find_denial<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a EgressDenial> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(denial) = error.downcast_ref::<EgressDenial>() {
            return Some(denial);
        }
        current = error.source();
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RuleSet {
    rules: Vec<EgressRule>,
    private_networks: bool,
    plain_http: bool,
}

impl RuleSet {
    fn admits_name(&self, host: &str, port: u16) -> bool {
        self.rules.iter().any(|rule| match rule {
            EgressRule::Host { pattern, .. } => pattern.matches(host) && rule.admits_port(port),
            EgressRule::Cidr { .. } => false,
        })
    }

    fn admits_address(&self, address: IpAddr, class: AddressClass) -> bool {
        class.is_public()
            || self.private_networks
            || self.rules.iter().any(|rule| match rule {
                EgressRule::Cidr { cidr, .. } => cidr.contains(address),
                EgressRule::Host { .. } => false,
            })
    }

    fn admits_literal(&self, address: SocketAddr, class: AddressClass) -> Result<(), EgressDenial> {
        let in_range = self.rules.iter().any(|rule| match rule {
            EgressRule::Cidr { cidr, .. } => {
                cidr.contains(address.ip()) && rule.admits_port(address.port())
            }
            EgressRule::Host { .. } => false,
        });
        let any_host = self.rules.iter().any(|rule| {
            matches!(rule, EgressRule::Host { pattern: HostPattern::Any, .. })
                && rule.admits_port(address.port())
        });
        if in_range || (any_host && self.admits_address(address.ip(), class)) {
            Ok(())
        } else if any_host {
            Err(EgressDenial::AddressNotAllowed {
                host: address.ip().to_string(),
                address: address.ip(),
                class,
            })
        } else {
            Err(EgressDenial::LiteralNotAllowed { address })
        }
    }
}

impl fmt::Display for RuleSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.rules.is_empty() {
            f.write_str("nothing")?;
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{rule}")?;
        }
        if self.private_networks {
            f.write_str(" +private-networks")?;
        }
        if self.plain_http {
            f.write_str(" +plain-http")?;
        }
        Ok(())
    }
}

/// Which hosts, ports and addresses connections may reach; see the [module](self) docs.
///
/// The default is [`public_only`](Self::public_only).
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{EgressPolicy, EgressRule, EgressTarget};
///
/// let policy = EgressPolicy::deny_all()
///     .allow("pypi.org:443".parse().unwrap())
///     .allow("*.pythonhosted.org:443".parse().unwrap())
///     .allow(EgressRule::cidr("10.20.0.0/16".parse().unwrap()).on_port(5432));
///
/// assert!(policy.check_url("https://files.pythonhosted.org/simple/").is_ok());
/// assert!(policy.check_url("http://pypi.org/").is_err());
/// assert!(policy.decide(&EgressTarget::Literal("10.20.1.2:5432".parse().unwrap())).is_ok());
/// assert!(policy.decide(&EgressTarget::Literal("10.20.1.2:22".parse().unwrap())).is_err());
/// assert!(EgressPolicy::default().check_url("https://169.254.169.254/latest").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    // Every layer must admit a destination; `narrow` appends layers, so a narrowed policy
    // never admits more than any policy it was narrowed by.
    layers: Vec<RuleSet>,
    // Set by `narrow`: the builder methods then change nothing, since editing any layer could
    // widen a policy the result was narrowed by.
    narrowed: bool,
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self::public_only()
    }
}

impl EgressPolicy {
    /// A policy that admits nothing until rules are added.
    pub fn deny_all() -> Self {
        Self { layers: vec![RuleSet::default()], narrowed: false }
    }

    /// Any host over `https`, on public addresses only.
    pub fn public_only() -> Self {
        Self::deny_all().allow(EgressRule::any_host())
    }

    /// Any host over `http` or `https`, on any address except cloud metadata endpoints.
    ///
    /// For in-cluster receivers and tests, where an operator configures every endpoint; never
    /// where callers register their own URLs.
    pub fn allow_private_networks() -> Self {
        Self::public_only().with_private_networks().with_plain_http()
    }

    /// A policy admitting exactly `rules`, over `https`, with non-public addresses admitted only
    /// inside CIDR rules.
    ///
    /// # Errors
    ///
    /// Returns the first rule that does not parse.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::egress::EgressPolicy;
    ///
    /// let policy = EgressPolicy::allowlist(["api.github.com:443", "*.githubusercontent.com"]).unwrap();
    /// assert!(policy.check_url("https://raw.githubusercontent.com/a/b").is_ok());
    /// assert!(EgressPolicy::allowlist(["https://api.github.com"]).is_err());
    /// ```
    pub fn allowlist<I, S>(rules: I) -> Result<Self, EgressRuleError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        rules
            .into_iter()
            .try_fold(Self::deny_all(), |policy, rule| Ok(policy.allow(rule.as_ref().parse()?)))
    }

    /// Adds `rule`. A narrowed policy is unchanged: it can only be narrowed further.
    #[must_use]
    pub fn allow(mut self, rule: EgressRule) -> Self {
        if !self.narrowed && !self.layers[0].rules.contains(&rule) {
            self.layers[0].rules.push(rule);
        }
        self
    }

    /// Admits loopback, private and other non-public addresses (never cloud metadata). A
    /// narrowed policy is unchanged: it can only be narrowed further.
    #[must_use]
    pub fn with_private_networks(mut self) -> Self {
        if !self.narrowed {
            self.layers[0].private_networks = true;
        }
        self
    }

    /// Admits plain `http` URLs. A narrowed policy is unchanged: it can only be narrowed
    /// further.
    #[must_use]
    pub fn with_plain_http(mut self) -> Self {
        if !self.narrowed {
            self.layers[0].plain_http = true;
        }
        self
    }

    /// Whether the policy came from [`narrow`](Self::narrow), so the builder methods no longer
    /// change it.
    pub fn is_narrowed(&self) -> bool {
        self.narrowed
    }

    /// The intersection of `self` and `other`: a destination passes only when it passes both.
    ///
    /// The result can only be narrowed further: [`allow`](Self::allow) and the switches leave it
    /// unchanged, so no layer below an operator's ceiling can widen it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::egress::EgressPolicy;
    ///
    /// let ceiling = EgressPolicy::allowlist(["*.example.com"]).unwrap();
    /// let wider = EgressPolicy::allowlist(["*.example.com", "evil.test"]).unwrap();
    /// let narrowed = ceiling.narrow(&wider);
    /// assert!(narrowed.check_url("https://api.example.com/").is_ok());
    /// assert!(narrowed.check_url("https://evil.test/").is_err());
    /// ```
    #[must_use]
    pub fn narrow(&self, other: &Self) -> Self {
        let mut layers = self.layers.clone();
        for layer in &other.layers {
            if !layers.contains(layer) {
                layers.push(layer.clone());
            }
        }
        Self { layers, narrowed: true }
    }

    /// Whether plain `http` URLs are admitted.
    pub fn allows_plain_http(&self) -> bool {
        self.layers.iter().all(|layer| layer.plain_http)
    }

    /// Whether every non-public address (except cloud metadata) is admitted.
    pub fn allows_private_networks(&self) -> bool {
        self.layers.iter().all(|layer| layer.private_networks)
    }

    /// Admits or refuses `target`.
    ///
    /// # Errors
    ///
    /// Returns why `target` is refused.
    pub fn decide(&self, target: &EgressTarget<'_>) -> Result<(), EgressDenial> {
        match *target {
            EgressTarget::Name { host, port } => {
                let unbracketed =
                    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
                if let Ok(ip) = unbracketed.parse::<IpAddr>() {
                    return self.decide(&EgressTarget::Literal(SocketAddr::new(ip, port)));
                }
                let host = normalize_host(host);
                if is_numeric_host(&host) {
                    return Err(EgressDenial::AmbiguousHost { host });
                }
                if !self.layers.iter().all(|layer| layer.admits_name(&host, port)) {
                    return Err(EgressDenial::HostNotAllowed { host, port });
                }
                // localhost names resolve to loopback (RFC 6761) whatever DNS says.
                if host == "localhost" || host.ends_with(".localhost") {
                    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
                    return self.decide(&EgressTarget::Address { host: &host, address: loopback });
                }
                Ok(())
            }
            EgressTarget::Address { host, address } => {
                let class = classify(address);
                if class == AddressClass::Metadata {
                    return Err(EgressDenial::MetadataAddress { host: host.to_string(), address });
                }
                if self.layers.iter().all(|layer| layer.admits_address(address, class)) {
                    Ok(())
                } else {
                    Err(EgressDenial::AddressNotAllowed { host: host.to_string(), address, class })
                }
            }
            EgressTarget::Literal(address) => {
                let class = classify(address.ip());
                if class == AddressClass::Metadata {
                    return Err(EgressDenial::MetadataAddress {
                        host: address.ip().to_string(),
                        address: address.ip(),
                    });
                }
                self.layers.iter().try_for_each(|layer| layer.admits_literal(address, class))
            }
        }
    }

    /// Parses `url` and checks its scheme, host and port; see
    /// [`check_parsed_url`](Self::check_parsed_url).
    ///
    /// # Errors
    ///
    /// Returns why the URL is refused.
    pub fn check_url(&self, url: &str) -> Result<url::Url, EgressDenial> {
        let parsed =
            url::Url::parse(url).map_err(|e| EgressDenial::InvalidUrl { reason: e.to_string() })?;
        self.check_parsed_url(&parsed)?;
        Ok(parsed)
    }

    /// Checks a URL's scheme (`https`, or `http` when admitted), and its host and port: an IP
    /// address literal completely, a host name before resolution. The addresses a name resolves
    /// to are checked by the client that connects, as [`EgressTarget::Address`].
    ///
    /// # Errors
    ///
    /// Returns why the URL is refused.
    pub fn check_parsed_url(&self, url: &url::Url) -> Result<(), EgressDenial> {
        let host = || url.host_str().unwrap_or_default().to_string();
        match url.scheme() {
            "https" => {}
            "http" if self.allows_plain_http() => {}
            "http" => return Err(EgressDenial::PlainHttp { host: host() }),
            other => return Err(EgressDenial::Scheme { scheme: other.to_string() }),
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| EgressDenial::InvalidUrl { reason: "the URL has no port".into() })?;
        match url.host() {
            Some(url::Host::Domain(host)) => self.decide(&EgressTarget::Name { host, port }),
            Some(url::Host::Ipv4(ip)) => {
                self.decide(&EgressTarget::Literal(SocketAddr::new(IpAddr::V4(ip), port)))
            }
            Some(url::Host::Ipv6(ip)) => {
                self.decide(&EgressTarget::Literal(SocketAddr::new(IpAddr::V6(ip), port)))
            }
            None => Err(EgressDenial::InvalidUrl { reason: "the URL has no host".into() }),
        }
    }
}

impl fmt::Display for EgressPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, layer) in self.layers.iter().enumerate() {
            if index > 0 {
                f.write_str("; narrowed by ")?;
            }
            write!(f, "{layer}")?;
        }
        Ok(())
    }
}

fn normalize_host(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// Whether the last label is decimal or `0x` hex, which `inet_aton`-style resolvers read as
/// part of an IPv4 address (`127.1`, `0x7f.1`, `2130706433`).
fn is_numeric_host(host: &str) -> bool {
    let last = host.rsplit('.').next().unwrap_or(host);
    let hex = last.strip_prefix("0x").map(|digits| digits.bytes().all(|b| b.is_ascii_hexdigit()));
    (!last.is_empty() && last.bytes().all(|b| b.is_ascii_digit())) || hex == Some(true)
}

/// What a tool declares about the network destinations a call reaches ([`Tool::egress`]).
///
/// Under a run egress policy ([`RunConfig::egress_policy`](crate::RunConfig::egress_policy)),
/// [`authorize_tool_call`](crate::authorize_tool_call) refuses a call before it runs unless
/// [`EgressPolicy::admits_tool`] admits its declaration. Without a run egress policy the
/// declaration is not consulted. A declaration is the tool author's statement; only the
/// connections of a [`GuardedClient`] or a sandbox's egress proxy are enforced.
///
/// | Declaration | Under a run egress policy |
/// |-------------|---------------------------|
/// | `Undeclared` (the default) | Refused |
/// | `None` | Admitted: the call opens no network connection |
/// | `Configured` | Admitted: the call reaches only endpoints the operator fixed when building the tool (a database, a model provider), never one its arguments choose |
/// | `Enforced` | Admitted: every connection the call makes is checked against the run's policy when it is made |
/// | `Destinations(rules)` | Admitted when the policy covers every rule ([`EgressPolicy::covers`]) |
///
/// [`Tool::egress`]: crate::Tool::egress
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{EgressPolicy, ToolEgress};
///
/// let policy = EgressPolicy::allowlist(["api.github.com:443"]).unwrap();
/// let call = ToolEgress::for_urls(["https://api.github.com/repos/a/b"]);
/// assert!(policy.admits_tool(&call).is_ok());
/// assert!(policy.admits_tool(&ToolEgress::for_urls(["https://example.com/"])).is_err());
/// assert!(policy.admits_tool(&ToolEgress::Undeclared).is_err());
/// assert!(policy.admits_tool(&ToolEgress::None).is_ok());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolEgress {
    /// The tool says nothing about its network use.
    #[default]
    Undeclared,
    /// The call opens no network connection.
    None,
    /// The call reaches only endpoints the operator fixed when building the tool.
    Configured,
    /// Every connection the call makes is checked against the run's egress policy when it is
    /// made, by a [`GuardedClient`] built from it or by a sandbox's egress proxy.
    Enforced,
    /// The call reaches only these destinations.
    Destinations(Vec<EgressRule>),
}

impl ToolEgress {
    /// The destinations of `urls`: each URL's host and port (`https` 443 and `http` 80 by
    /// default). A URL that does not parse, has no host, or names a numeric host makes the
    /// whole declaration [`Undeclared`](Self::Undeclared), which a run egress policy refuses.
    pub fn for_urls<I, S>(urls: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut rules = Vec::new();
        for url in urls {
            let Ok(parsed) = url::Url::parse(url.as_ref()) else { return Self::Undeclared };
            let Some(port) = parsed.port_or_known_default() else { return Self::Undeclared };
            let rule = match parsed.host() {
                Some(url::Host::Domain(host)) => match EgressRule::host(host) {
                    Ok(rule) => rule,
                    Err(_) => return Self::Undeclared,
                },
                Some(url::Host::Ipv4(ip)) => {
                    EgressRule::cidr(IpCidr { network: ip.into(), prefix: 32 })
                }
                Some(url::Host::Ipv6(ip)) => {
                    EgressRule::cidr(IpCidr { network: ip.into(), prefix: 128 })
                }
                None => return Self::Undeclared,
            };
            rules.push(rule.on_port(port));
        }
        Self::Destinations(rules)
    }
}

impl EgressPolicy {
    /// Whether every destination `rule` names passes the policy's host, port and literal
    /// rules. Addresses are not checked here: a name in a covered rule is still refused at
    /// connect time when it resolves to an address the policy refuses.
    ///
    /// Coverage is by a single rule of each layer: a declared `*.example.com` is covered by
    /// `*.example.com` or `*`, not by a list of its subdomains; a declared CIDR only by a CIDR
    /// rule containing it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::egress::{EgressPolicy, EgressRule};
    ///
    /// let policy = EgressPolicy::allowlist(["*.example.com:443"]).unwrap();
    /// assert!(policy.covers(&"api.example.com:443".parse().unwrap()));
    /// assert!(policy.covers(&"*.eu.example.com:443".parse().unwrap()));
    /// assert!(!policy.covers(&"api.example.com".parse().unwrap()));
    /// assert!(!policy.covers(&EgressRule::any_host()));
    /// ```
    pub fn covers(&self, rule: &EgressRule) -> bool {
        self.layers.iter().all(|layer| layer.rules.iter().any(|own| subsumes(own, rule)))
    }

    /// Admits or refuses a tool call by its declaration; see [`ToolEgress`].
    ///
    /// # Errors
    ///
    /// Returns why the declaration is refused.
    pub fn admits_tool(&self, egress: &ToolEgress) -> Result<(), String> {
        match egress {
            ToolEgress::None | ToolEgress::Configured | ToolEgress::Enforced => Ok(()),
            ToolEgress::Undeclared => {
                Err("the run has an egress policy and the tool does not declare the network \
                 destinations it reaches (Tool::egress); declare them, or run it without an \
                 egress policy"
                    .to_string())
            }
            ToolEgress::Destinations(rules) => match rules.iter().find(|rule| !self.covers(rule)) {
                Some(rule) => Err(format!(
                    "the call reaches {rule}, which the run's egress policy ({self}) does not \
                     admit"
                )),
                None => Ok(()),
            },
        }
    }
}

/// Whether `own` admits every destination `declared` names.
fn subsumes(own: &EgressRule, declared: &EgressRule) -> bool {
    let ports = |own: &Option<u16>, declared: &Option<u16>| match (own, declared) {
        (None, _) => true,
        (Some(own), Some(declared)) => own == declared,
        (Some(_), None) => false,
    };
    match (own, declared) {
        (
            EgressRule::Host { pattern: own_pattern, port: own_port },
            EgressRule::Host { pattern, port },
        ) => {
            let names = match (own_pattern, pattern) {
                (HostPattern::Any, _) => true,
                (HostPattern::Exact(own), HostPattern::Exact(name)) => own == name,
                (HostPattern::Subdomains(_), HostPattern::Exact(name)) => own_pattern.matches(name),
                (HostPattern::Subdomains(own), HostPattern::Subdomains(domain)) => {
                    domain == own || own_pattern.matches(domain)
                }
                (HostPattern::Exact(_), HostPattern::Any | HostPattern::Subdomains(_))
                | (HostPattern::Subdomains(_), HostPattern::Any) => false,
            };
            names && ports(own_port, port)
        }
        (EgressRule::Cidr { cidr: own, port: own_port }, EgressRule::Cidr { cidr, port }) => {
            own.prefix <= cidr.prefix && own.contains(cidr.network) && ports(own_port, port)
        }
        (EgressRule::Host { .. }, EgressRule::Cidr { .. })
        | (EgressRule::Cidr { .. }, EgressRule::Host { .. }) => false,
    }
}

/// Resolves host names for an egress check, so tests and proxies can supply their own answers.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{HostResolver, StaticResolver};
///
/// # async fn example() -> std::io::Result<()> {
/// let resolver = StaticResolver::new().with_host("api.test", ["10.0.0.7".parse().unwrap()]);
/// let expected: std::net::IpAddr = "10.0.0.7".parse().unwrap();
/// assert_eq!(resolver.resolve("api.test").await?, vec![expected]);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait HostResolver: Send + Sync + fmt::Debug {
    /// Every address `host` resolves to.
    ///
    /// # Errors
    ///
    /// Returns the lookup failure.
    async fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>>;
}

/// The operating system's resolver (feature `egress-dns`).
#[cfg(feature = "egress-dns")]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

#[cfg(feature = "egress-dns")]
#[async_trait]
impl HostResolver for SystemResolver {
    async fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        Ok(tokio::net::lookup_host((host, 0)).await?.map(|address| address.ip()).collect())
    }
}

/// A fixed map from host names to addresses, for tests and closed deployments.
#[derive(Debug, Clone, Default)]
pub struct StaticResolver {
    hosts: HashMap<String, Vec<IpAddr>>,
}

impl StaticResolver {
    /// An empty map; every lookup fails.
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps `host` to `addresses`.
    #[must_use]
    pub fn with_host(
        mut self,
        host: impl AsRef<str>,
        addresses: impl IntoIterator<Item = IpAddr>,
    ) -> Self {
        self.hosts.insert(normalize_host(host.as_ref()), addresses.into_iter().collect());
        self
    }
}

#[async_trait]
impl HostResolver for StaticResolver {
    async fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        self.hosts.get(&normalize_host(host)).cloned().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("{host} has no address"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(host: &str, port: u16) -> EgressTarget<'_> {
        EgressTarget::Name { host, port }
    }

    fn literal(address: &str) -> EgressTarget<'static> {
        EgressTarget::Literal(address.parse().unwrap())
    }

    #[test]
    fn rules_round_trip_through_text() {
        for text in [
            "*",
            "*:443",
            "api.example.com",
            "api.example.com:443",
            "*.example.com",
            "*.example.com:8443",
            "10.0.0.0/8",
            "10.0.0.5/32:5432",
            "fd00::/8",
            "[fd00::/8]:443",
            "[::1/128]:8080",
        ] {
            let rule: EgressRule = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(rule.to_string(), text);
            assert_eq!(rule.to_string().parse::<EgressRule>().unwrap(), rule);
        }
        assert_eq!("10.0.0.5:5432".parse::<EgressRule>().unwrap().to_string(), "10.0.0.5/32:5432");
        assert_eq!("[::1]:8080".parse::<EgressRule>().unwrap().to_string(), "[::1/128]:8080");
        assert_eq!(
            "API.Example.COM.".parse::<EgressRule>().unwrap().to_string(),
            "api.example.com"
        );
        assert_eq!("::ffff:10.0.0.0/104".parse::<EgressRule>().unwrap().to_string(), "10.0.0.0/8");
    }

    #[test]
    fn malformed_rules_are_refused() {
        for text in [
            "",
            "https://api.example.com",
            "api.example.com:0",
            "api.example.com:70000",
            "api.example.com:x",
            "*example.com",
            "a.*.example.com",
            "exa mple.com",
            "example..com",
            "0x7f.1",
            "127.1",
            "10.0.0.1/8",
            "10.0.0.0/33",
            "[fd00::/8",
            "[fd00::/8]443",
            "[api.example.com]:443",
        ] {
            assert!(text.parse::<EgressRule>().is_err(), "{text} must not parse");
        }
    }

    #[test]
    fn subdomain_wildcards_exclude_the_domain_and_lookalikes() {
        let policy = EgressPolicy::allowlist(["*.example.com"]).unwrap();
        assert!(policy.decide(&name("a.example.com", 443)).is_ok());
        assert!(policy.decide(&name("a.b.example.com", 443)).is_ok());
        assert!(policy.decide(&name("A.Example.Com.", 443)).is_ok());
        for host in ["example.com", "badexample.com", "example.com.evil.test"] {
            assert_eq!(
                policy.decide(&name(host, 443)),
                Err(EgressDenial::HostNotAllowed { host: host.to_string(), port: 443 })
            );
        }
    }

    #[test]
    fn ports_restrict_host_and_cidr_rules() {
        let policy = EgressPolicy::allowlist(["api.example.com:443", "10.0.0.0/8:5432"]).unwrap();
        assert!(policy.decide(&name("api.example.com", 443)).is_ok());
        assert!(policy.decide(&name("api.example.com", 80)).is_err());
        assert!(policy.decide(&literal("10.1.2.3:5432")).is_ok());
        assert_eq!(
            policy.decide(&literal("10.1.2.3:22")),
            Err(EgressDenial::LiteralNotAllowed { address: "10.1.2.3:22".parse().unwrap() })
        );
    }

    #[test]
    fn non_public_addresses_need_a_cidr_or_private_networks() {
        let address = |host, ip: &str| EgressTarget::Address { host, address: ip.parse().unwrap() };
        let listed = EgressPolicy::allowlist(["db.internal"]).unwrap();
        assert!(listed.decide(&address("db.internal", "93.184.216.34")).is_ok());
        assert_eq!(
            listed.decide(&address("db.internal", "10.0.0.5")),
            Err(EgressDenial::AddressNotAllowed {
                host: "db.internal".into(),
                address: "10.0.0.5".parse().unwrap(),
                class: AddressClass::Private,
            })
        );
        let ranged = listed.clone().allow("10.0.0.0/24".parse().unwrap());
        assert!(ranged.decide(&address("db.internal", "10.0.0.5")).is_ok());
        assert!(ranged.decide(&address("db.internal", "10.0.1.5")).is_err());
        let private = listed.with_private_networks();
        assert!(private.decide(&address("db.internal", "10.0.1.5")).is_ok());
        assert!(private.decide(&address("db.internal", "::1")).is_ok());
    }

    #[test]
    fn metadata_is_refused_by_every_policy() {
        let policies = [
            EgressPolicy::allow_private_networks(),
            EgressPolicy::allowlist(["*", "169.254.0.0/16", "0.0.0.0/0", "::/0"])
                .unwrap()
                .with_private_networks(),
        ];
        for policy in policies {
            for ip in ["169.254.169.254", "168.63.129.16", "100.100.100.200", "fd00:ec2::254"] {
                let address: IpAddr = ip.parse().unwrap();
                assert!(matches!(
                    policy.decide(&EgressTarget::Literal(SocketAddr::new(address, 80))),
                    Err(EgressDenial::MetadataAddress { .. })
                ));
                assert!(matches!(
                    policy.decide(&EgressTarget::Address { host: "metadata.test", address }),
                    Err(EgressDenial::MetadataAddress { .. })
                ));
            }
            assert!(policy.check_url("http://[::ffff:169.254.169.254]/latest").is_err());
        }
    }

    #[test]
    fn literals_need_a_cidr_unless_any_host_admits_them() {
        let listed = EgressPolicy::allowlist(["api.example.com"]).unwrap();
        assert_eq!(
            listed.decide(&literal("93.184.216.34:443")),
            Err(EgressDenial::LiteralNotAllowed { address: "93.184.216.34:443".parse().unwrap() })
        );
        let public = EgressPolicy::public_only();
        assert!(public.decide(&literal("93.184.216.34:443")).is_ok());
        assert!(matches!(
            public.decide(&literal("127.0.0.1:443")),
            Err(EgressDenial::AddressNotAllowed { class: AddressClass::Loopback, .. })
        ));
        // A literal written as a name is still checked as a literal.
        assert!(listed.decide(&name("[::1]", 443)).is_err());
        assert!(listed.decide(&name("10.0.0.1", 443)).is_err());
    }

    #[test]
    fn localhost_names_are_checked_as_loopback() {
        let public = EgressPolicy::public_only();
        for host in ["localhost", "api.localhost", "LOCALHOST."] {
            assert!(matches!(
                public.decide(&name(host, 80)),
                Err(EgressDenial::AddressNotAllowed { class: AddressClass::Loopback, .. })
            ));
        }
        assert!(EgressPolicy::allow_private_networks().decide(&name("localhost", 80)).is_ok());
    }

    #[test]
    fn numeric_host_names_are_ambiguous() {
        for host in ["0x7f.1", "127.1", "2130706433", "example.123", "a.0xff"] {
            assert_eq!(
                EgressPolicy::public_only().decide(&name(host, 443)),
                Err(EgressDenial::AmbiguousHost { host: host.to_string() })
            );
        }
        assert!(EgressPolicy::public_only().decide(&name("1password.com", 443)).is_ok());
    }

    #[test]
    fn urls_are_checked_for_scheme_host_and_port() {
        let policy = EgressPolicy::default();
        for url in [
            "http://example.com/hook",
            "https://127.0.0.1/hook",
            "https://10.0.0.1/hook",
            "https://169.254.169.254/latest",
            "https://[::1]/hook",
            "https://[::ffff:192.168.1.1]/hook",
            "https://[64:ff9b::a00:1]/hook",
            "https://[2002:c0a8:101::1]/hook",
            "https://localhost/hook",
            "https://api.localhost/hook",
            "https://0x7f.1/hook",
            "https://198.18.0.1/hook",
            "https://240.0.0.1/hook",
            "https://168.63.129.16/machine",
            "ftp://example.com/hook",
            "not a url",
        ] {
            assert!(policy.check_url(url).is_err(), "{url}");
        }
        for url in [
            "https://example.com/hook",
            "https://93.184.216.34/hook",
            "https://[2606:4700::1111]/x",
        ] {
            assert!(policy.check_url(url).is_ok(), "{url}");
        }
        let http = EgressPolicy::allowlist(["api.example.com:8080"]).unwrap().with_plain_http();
        assert!(http.check_url("http://api.example.com:8080/").is_ok());
        assert!(http.check_url("http://api.example.com/").is_err());
    }

    #[test]
    fn narrowing_keeps_switches_only_when_both_set_them() {
        let open = EgressPolicy::allow_private_networks();
        let strict = EgressPolicy::public_only();
        let narrowed = open.narrow(&strict);
        assert!(!narrowed.allows_plain_http());
        assert!(!narrowed.allows_private_networks());
        assert!(narrowed.check_url("http://example.com/").is_err());
        assert!(narrowed.check_url("https://10.0.0.1/").is_err());
        // A narrowed policy cannot be widened, not even past its own first layer.
        let rewidened = strict
            .narrow(&EgressPolicy::allowlist(["*.test"]).unwrap())
            .allow(EgressRule::host("b.test").unwrap())
            .allow(EgressRule::host("evil.example").unwrap())
            .with_private_networks();
        assert!(rewidened.is_narrowed());
        assert!(rewidened.check_url("https://b.test/").is_ok());
        assert!(rewidened.check_url("https://evil.example/").is_err());
        assert!(rewidened.check_url("https://10.0.0.1/").is_err());
        assert_eq!(open.narrow(&open).to_string(), open.to_string());
        assert_eq!(narrowed.to_string(), "* +private-networks +plain-http; narrowed by *");
    }

    #[test]
    fn denials_are_found_through_error_chains() {
        #[derive(Debug, thiserror::Error)]
        #[error("request failed")]
        struct Wrapper(#[source] EgressDenial);
        let denial = EgressDenial::PlainHttp { host: "a.test".into() };
        let wrapped = Wrapper(denial.clone());
        assert_eq!(find_denial(&wrapped), Some(&denial));
        assert_eq!(denial.code(), "egress.plain_http");
        let io = std::io::Error::other("unrelated");
        assert_eq!(find_denial(&io), None);
    }

    #[test]
    fn declarations_are_covered_rule_by_rule() {
        let policy =
            EgressPolicy::allowlist(["*.example.com:443", "api.github.com", "10.0.0.0/8:5432"])
                .unwrap();
        let rule = |text: &str| text.parse::<EgressRule>().unwrap();
        for covered in [
            "a.example.com:443",
            "*.b.example.com:443",
            "api.github.com:22",
            "api.github.com",
            "10.1.0.0/16:5432",
            "10.1.2.3:5432",
        ] {
            assert!(policy.covers(&rule(covered)), "{covered}");
        }
        for refused in [
            "example.com:443",
            "*.example.com",
            "a.example.com:80",
            "*",
            "github.com:443",
            "10.0.0.0/8",
            "11.0.0.1:5432",
            "*.github.com:443",
        ] {
            assert!(!policy.covers(&rule(refused)), "{refused}");
        }
        let narrowed = policy.narrow(&EgressPolicy::allowlist(["api.github.com:443"]).unwrap());
        assert!(narrowed.covers(&rule("api.github.com:443")));
        assert!(!narrowed.covers(&rule("api.github.com:22")));
    }

    #[test]
    fn tool_declarations_are_admitted_by_kind() {
        let policy = EgressPolicy::allowlist(["docs.rs:443"]).unwrap();
        for admitted in [
            ToolEgress::None,
            ToolEgress::Configured,
            ToolEgress::Enforced,
            ToolEgress::for_urls(["https://docs.rs/adk"]),
        ] {
            assert!(policy.admits_tool(&admitted).is_ok(), "{admitted:?}");
        }
        for refused in [
            ToolEgress::Undeclared,
            ToolEgress::for_urls(["https://docs.rs/a", "https://crates.io/b"]),
            ToolEgress::for_urls(["http://docs.rs/"]),
            ToolEgress::for_urls(["not a url"]),
            ToolEgress::for_urls(["https://93.184.216.34/"]),
        ] {
            assert!(policy.admits_tool(&refused).is_err(), "{refused:?}");
        }
        assert_eq!(
            ToolEgress::for_urls(["https://0x7f.1/"]),
            ToolEgress::Destinations(vec!["127.0.0.1:443".parse().unwrap()])
        );
        assert_eq!(
            ToolEgress::for_urls(["https://127.1/"]),
            ToolEgress::Destinations(vec!["127.0.0.1:443".parse().unwrap()])
        );
    }

    #[tokio::test]
    async fn static_resolver_answers_only_known_hosts() {
        let resolver = StaticResolver::new().with_host("API.test.", ["10.0.0.1".parse().unwrap()]);
        assert_eq!(
            resolver.resolve("api.test").await.unwrap(),
            vec!["10.0.0.1".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(
            resolver.resolve("other.test").await.unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
