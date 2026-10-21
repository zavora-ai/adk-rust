//! URL grammar, domain matching and private-target classification for the web tools
//! (spec 3.1 §7.20, §9.2 I46).
//!
//! The grammar is the `ArgPredicate::DomainIn` grammar of adk-core: a URL whose host a
//! parser could read two ways is refused rather than guessed at.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why a URL is refused by the grammar (spec 3.1 §7.20 "Resolution").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlRefusal {
    /// The scheme is not `http` or `https`.
    NotHttp,
    /// The authority carries user information (`user@host`).
    UserInfo,
    /// The URL carries a backslash before the path.
    Backslash,
    /// The host is percent-encoded.
    PercentEncodedHost,
    /// The host is empty.
    EmptyHost,
    /// The host carries a character outside `[a-z0-9.-]`, an empty label, or a label that
    /// is too long.
    InvalidHost(String),
    /// The port is not a decimal number below 65536.
    InvalidPort,
    /// The URL carries whitespace or a control character.
    Whitespace,
}

impl fmt::Display for UrlRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHttp => f.write_str("only http and https URLs are fetched"),
            Self::UserInfo => f.write_str("a URL with user information is refused"),
            Self::Backslash => f.write_str("a URL with a backslash in its authority is refused"),
            Self::PercentEncodedHost => f.write_str("a percent-encoded host is refused"),
            Self::EmptyHost => f.write_str("the URL has no host"),
            Self::InvalidHost(host) => write!(
                f,
                "host '{host}' is refused: only lowercase ASCII letters, digits, '.' and '-' are \
                 accepted (write an internationalized name as punycode)"
            ),
            Self::InvalidPort => f.write_str("the URL's port is not a number below 65536"),
            Self::Whitespace => {
                f.write_str("a URL with whitespace or control characters is refused")
            }
        }
    }
}

impl std::error::Error for UrlRefusal {}

/// A URL parsed under the `DomainIn` grammar (spec 3.1 §7.20).
///
/// # Example
///
/// ```rust
/// use adk_harness::web::ParsedUrl;
///
/// let url = ParsedUrl::parse("HTTPS://Docs.rs:443/crate/adk-core?q=1#top").unwrap();
/// assert_eq!(url.host, "docs.rs");
/// assert_eq!(url.port, 443);
/// assert_eq!(url.path, "/crate/adk-core");
/// assert_eq!(url.query.as_deref(), Some("q=1"));
/// assert!(ParsedUrl::parse("https://user@docs.rs/").is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedUrl {
    /// Whether the scheme is `https`.
    pub https: bool,
    /// The lowercase host: a name, a dotted IPv4 literal, or an IPv6 literal without
    /// its brackets.
    pub host: String,
    /// The port, explicit or the scheme's default.
    pub port: u16,
    /// Whether the port was written in the URL.
    pub explicit_port: bool,
    /// The path, `/` when absent.
    pub path: String,
    /// The query without its `?`.
    pub query: Option<String>,
    /// The fragment without its `#`.
    pub fragment: Option<String>,
}

impl ParsedUrl {
    /// Parses `url`, refusing anything the grammar does not accept.
    ///
    /// # Errors
    ///
    /// Returns the [`UrlRefusal`] naming the first rule the URL breaks.
    pub fn parse(url: &str) -> Result<Self, UrlRefusal> {
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(UrlRefusal::Whitespace);
        }
        let (scheme, rest) = url.split_once("://").ok_or(UrlRefusal::NotHttp)?;
        let https = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return Err(UrlRefusal::NotHttp),
        };
        let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
        if rest[end..].starts_with('\\') {
            return Err(UrlRefusal::Backslash);
        }
        let authority = &rest[..end];
        if authority.contains('@') {
            return Err(UrlRefusal::UserInfo);
        }
        if authority.contains('%') {
            return Err(UrlRefusal::PercentEncodedHost);
        }
        if authority.contains('\\') {
            return Err(UrlRefusal::Backslash);
        }
        let (host, port) = split_authority(authority)?;
        if host.is_empty() {
            return Err(UrlRefusal::EmptyHost);
        }
        let host = host.to_ascii_lowercase();
        let host = if host.starts_with('[') && host.ends_with(']') {
            let inner = &host[1..host.len() - 1];
            inner.parse::<Ipv6Addr>().map_err(|_| UrlRefusal::InvalidHost(host.clone()))?;
            inner.to_string()
        } else {
            let host = host.strip_suffix('.').unwrap_or(&host).to_string();
            validate_name(&host)?;
            host
        };
        let (port, explicit_port) = match port {
            Some(port) => (port, true),
            None => (if https { 443 } else { 80 }, false),
        };
        let tail = &rest[end..];
        let (path, after_path) = match tail.find(['?', '#']) {
            Some(i) => (&tail[..i], &tail[i..]),
            None => (tail, ""),
        };
        let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
        let (query, fragment) = match after_path.split_once('#') {
            Some((query, fragment)) => {
                (query.strip_prefix('?').map(str::to_string), Some(fragment.to_string()))
            }
            None => (after_path.strip_prefix('?').map(str::to_string), None),
        };
        Ok(Self { https, host, port, explicit_port, path, query, fragment })
    }

    /// The URL as text, normalized: lowercase scheme and host, default port omitted.
    pub fn as_string(&self) -> String {
        let mut out = String::new();
        out.push_str(if self.https { "https://" } else { "http://" });
        if self.host.contains(':') {
            out.push('[');
            out.push_str(&self.host);
            out.push(']');
        } else {
            out.push_str(&self.host);
        }
        let default_port = if self.https { 443 } else { 80 };
        if self.port != default_port {
            out.push(':');
            out.push_str(&self.port.to_string());
        }
        out.push_str(&self.path);
        if let Some(query) = &self.query {
            out.push('?');
            out.push_str(query);
        }
        if let Some(fragment) = &self.fragment {
            out.push('#');
            out.push_str(fragment);
        }
        out
    }

    /// Bytes outside the path: the query and the fragment (spec 3.1 §9.2 I48).
    pub fn non_path_bytes(&self) -> usize {
        self.query.as_ref().map_or(0, String::len) + self.fragment.as_ref().map_or(0, String::len)
    }

    /// The host as a literal address, when it is one.
    pub fn literal(&self) -> Option<IpAddr> {
        literal_address(&self.host)
    }

    /// Resolves `location` (absolute, scheme-relative, absolute-path or relative) against
    /// this URL, as a redirect target.
    ///
    /// # Errors
    ///
    /// Returns the refusal of the composed URL.
    pub fn join(&self, location: &str) -> Result<Self, UrlRefusal> {
        let location = location.trim();
        if location.contains("://") {
            return Self::parse(location);
        }
        let scheme = if self.https { "https:" } else { "http:" };
        if let Some(rest) = location.strip_prefix("//") {
            return Self::parse(&format!("{scheme}//{rest}"));
        }
        let mut base = Self { path: "/".into(), query: None, fragment: None, ..self.clone() };
        if location.starts_with('/') {
            base.path = String::new();
            return Self::parse(&format!("{}{location}", base.as_string()));
        }
        let dir = match self.path.rfind('/') {
            Some(i) => &self.path[..=i],
            None => "/",
        };
        base.path = String::new();
        Self::parse(&format!("{}{dir}{location}", base.as_string()))
    }
}

fn split_authority(authority: &str) -> Result<(&str, Option<u16>), UrlRefusal> {
    if let Some(end) = authority.find(']') {
        if !authority.starts_with('[') {
            return Err(UrlRefusal::InvalidHost(authority.to_string()));
        }
        let host = &authority[..=end];
        let rest = &authority[end + 1..];
        return match rest.strip_prefix(':') {
            Some(port) => Ok((host, Some(parse_port(port)?))),
            None if rest.is_empty() => Ok((host, None)),
            None => Err(UrlRefusal::InvalidHost(authority.to_string())),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Ok((host, Some(parse_port(port)?))),
        None => Ok((authority, None)),
    }
}

fn parse_port(port: &str) -> Result<u16, UrlRefusal> {
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UrlRefusal::InvalidPort);
    }
    port.parse::<u16>().map_err(|_| UrlRefusal::InvalidPort)
}

fn validate_name(host: &str) -> Result<(), UrlRefusal> {
    let refuse = || UrlRefusal::InvalidHost(host.to_string());
    if host.is_empty() {
        return Err(UrlRefusal::EmptyHost);
    }
    if host.len() > 253 {
        return Err(refuse());
    }
    if !host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(refuse());
    }
    if host.split('.').any(|label| label.is_empty() || label.len() > 63) {
        return Err(refuse());
    }
    Ok(())
}

/// Whether `host` is covered by the allowlist entry `pattern`: exactly, or as a subdomain
/// of a `*.example.com` entry (never `example.com` itself).
///
/// # Example
///
/// ```rust
/// use adk_harness::web::domain_matches;
///
/// assert!(domain_matches("docs.rs", "docs.rs"));
/// assert!(domain_matches("*.rust-lang.org", "doc.rust-lang.org"));
/// assert!(!domain_matches("*.rust-lang.org", "rust-lang.org"));
/// assert!(!domain_matches("rust-lang.org", "evil-rust-lang.org"));
/// ```
pub fn domain_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let pattern = pattern.strip_suffix('.').unwrap_or(&pattern);
    match pattern.strip_prefix("*.") {
        Some(parent) => host.strip_suffix(parent).is_some_and(|sub| sub.ends_with('.')),
        None => pattern == host,
    }
}

/// Whether `host` is in `allowlist` under [`domain_matches`].
pub fn host_allowlisted(host: &str, allowlist: &[String]) -> bool {
    allowlist.iter().any(|pattern| domain_matches(pattern, host))
}

/// Whether `url` parses and its host is in `allowlist`; an unparseable URL is never
/// allowlisted.
pub fn url_allowlisted(url: &str, allowlist: &[String]) -> bool {
    ParsedUrl::parse(url).is_ok_and(|parsed| host_allowlisted(&parsed.host, allowlist))
}

/// The host of `url` under the grammar, if it parses.
pub fn url_host(url: &str) -> Option<String> {
    ParsedUrl::parse(url).ok().map(|parsed| parsed.host)
}

fn literal_address(host: &str) -> Option<IpAddr> {
    if let Ok(v6) = host.parse::<Ipv6Addr>() {
        return Some(IpAddr::V6(v6));
    }
    // Only a dotted quad is a literal; `0x7f.1` and friends are refused by the grammar.
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() == 4
        && octets.iter().all(|o| !o.is_empty() && o.bytes().all(|b| b.is_ascii_digit()))
    {
        return host.parse::<Ipv4Addr>().ok().map(IpAddr::V4);
    }
    None
}

/// Whether `ip` is a loopback, link-local, private, unspecified, multicast or
/// IPv4-mapped address of those kinds (spec 3.1 §9.2 I46).
///
/// # Example
///
/// ```rust
/// use adk_harness::web::is_private_ip;
///
/// assert!(is_private_ip("127.0.0.1".parse().unwrap()));
/// assert!(is_private_ip("169.254.169.254".parse().unwrap()));
/// assert!(is_private_ip("10.1.2.3".parse().unwrap()));
/// assert!(is_private_ip("::ffff:192.168.0.1".parse().unwrap()));
/// assert!(is_private_ip("fe80::1".parse().unwrap()));
/// assert!(!is_private_ip("93.184.216.34".parse().unwrap()));
/// ```
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(v4);
            }
            let segments = v6.segments();
            // NAT64 (64:ff9b::/96) and 6to4 (2002::/16) embed an IPv4 address.
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b, c, d] =
                    ((u32::from(segments[6]) << 16) | u32::from(segments[7])).to_be_bytes();
                return is_private_v4(Ipv4Addr::new(a, b, c, d));
            }
            if segments[0] == 0x2002 {
                let [a, b, c, d] =
                    ((u32::from(segments[1]) << 16) | u32::from(segments[2])).to_be_bytes();
                return is_private_v4(Ipv4Addr::new(a, b, c, d));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (segments[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

fn is_private_v4(v4: Ipv4Addr) -> bool {
    let [a, b, _, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // 100.64.0.0/10 shared address space
}

/// Whether `host` names a private target before any resolution: a literal of a private
/// kind, `localhost`, or a `.local` / `.internal` name (spec 3.1 §7.20 "Resolution").
///
/// # Example
///
/// ```rust
/// use adk_harness::web::is_private_host;
///
/// assert!(is_private_host("localhost"));
/// assert!(is_private_host("printer.local"));
/// assert!(is_private_host("vault.internal"));
/// assert!(is_private_host("127.0.0.1"));
/// assert!(is_private_host("::1"));
/// assert!(!is_private_host("docs.rs"));
/// ```
pub fn is_private_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let literal = literal_address(host);
    if fixture_loopback_permitted() && is_loopback_only(host, literal) {
        return false;
    }
    if let Some(ip) = literal {
        return is_private_ip(ip);
    }
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "local"
        || host.ends_with(".local")
        || host == "internal"
        || host.ends_with(".internal")
}

/// Whether `url` names a private target by its literal or name alone.
pub fn url_names_private_target(url: &str) -> bool {
    ParsedUrl::parse(url).is_ok_and(|parsed| is_private_host(&parsed.host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_refuses_ambiguous_urls() {
        for (url, refusal) in [
            ("ftp://docs.rs/", UrlRefusal::NotHttp),
            ("https://user:pw@docs.rs/", UrlRefusal::UserInfo),
            ("https://docs.rs\\evil.com/", UrlRefusal::Backslash),
            ("https://docs%2ers/", UrlRefusal::PercentEncodedHost),
            ("https:///path", UrlRefusal::EmptyHost),
            ("https://docs.rs:99999/", UrlRefusal::InvalidPort),
            ("https://docs.rs:abc/", UrlRefusal::InvalidPort),
            ("https://do cs.rs/", UrlRefusal::Whitespace),
            ("https://docs..rs/", UrlRefusal::InvalidHost("docs..rs".into())),
            ("https://münchen.de/", UrlRefusal::InvalidHost("münchen.de".into())),
            ("https://docs_rs.io/", UrlRefusal::InvalidHost("docs_rs.io".into())),
        ] {
            assert_eq!(ParsedUrl::parse(url).unwrap_err(), refusal, "{url}");
        }
    }

    #[test]
    fn grammar_accepts_punycode_ports_and_ipv6_literals() {
        let idn = ParsedUrl::parse("https://xn--mnchen-3ya.de/a").unwrap();
        assert_eq!(idn.host, "xn--mnchen-3ya.de");
        let port = ParsedUrl::parse("http://docs.rs:8080/x").unwrap();
        assert_eq!((port.port, port.explicit_port), (8080, true));
        assert_eq!(port.as_string(), "http://docs.rs:8080/x");
        let v6 = ParsedUrl::parse("http://[::1]:8000/").unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.literal(), Some("::1".parse().unwrap()));
        assert_eq!(v6.as_string(), "http://[::1]:8000/");
        let trailing = ParsedUrl::parse("https://docs.rs./").unwrap();
        assert_eq!(trailing.host, "docs.rs");
    }

    #[test]
    fn join_resolves_every_redirect_form() {
        let base = ParsedUrl::parse("https://docs.rs/a/b?q=1").unwrap();
        assert_eq!(base.join("https://other.example/x").unwrap().host, "other.example");
        assert_eq!(base.join("//cdn.docs.rs/x").unwrap().as_string(), "https://cdn.docs.rs/x");
        assert_eq!(base.join("/root").unwrap().as_string(), "https://docs.rs/root");
        assert_eq!(base.join("c").unwrap().as_string(), "https://docs.rs/a/c");
    }

    #[test]
    fn private_classification_covers_mapped_and_embedded_addresses() {
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "172.16.5.5",
            "172.31.255.255",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "93.184.216.34", "172.32.0.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn private_names_and_literals_are_known_before_resolution() {
        for url in [
            "http://localhost/",
            "http://api.localhost/",
            "http://printer.local/",
            "http://vault.internal/",
            "http://127.0.0.1:8080/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::ffff:10.0.0.1]/",
        ] {
            assert!(url_names_private_target(url), "{url}");
        }
        assert!(!url_names_private_target("https://docs.rs/"));
        // Not a dotted quad: the grammar accepts the name, resolution decides.
        assert!(!url_names_private_target("http://2130706433/"));
    }

    #[test]
    fn allowlist_matching_follows_the_domain_in_grammar() {
        let allow = vec!["docs.rs".to_string(), "*.rust-lang.org".to_string()];
        assert!(url_allowlisted("https://docs.rs/x", &allow));
        assert!(url_allowlisted("https://DOCS.RS:443/x", &allow));
        assert!(url_allowlisted("https://doc.rust-lang.org/", &allow));
        assert!(!url_allowlisted("https://rust-lang.org/", &allow));
        assert!(!url_allowlisted("https://docs.rs.evil.com/", &allow));
        assert!(!url_allowlisted("https://evildocs.rs/", &allow));
        assert!(!url_allowlisted("https://user@docs.rs/", &allow));
        assert!(!url_allowlisted("https://docs.rs%2e/", &allow));
    }
}

/// Whether a loopback target is admitted for a test fixture (feature `web-fixture`).
pub(crate) fn fixture_loopback_permitted() -> bool {
    fixture::LOOPBACK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Test fixtures (feature `web-fixture`): a loopback HTTP server standing in for the web.
///
/// Off in every build without the feature, and off until `permit_loopback` is called;
/// only loopback addresses are admitted, never link-local, private or mapped ones.
pub mod fixture {
    pub(crate) static LOOPBACK: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Admits loopback targets (`127.0.0.0/8`, `::1`, `localhost`) for this process's
    /// fixtures. Compiled only with feature `web-fixture`.
    #[cfg(feature = "web-fixture")]
    pub fn permit_loopback() {
        LOOPBACK.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

fn is_loopback_only(host: &str, ip: Option<IpAddr>) -> bool {
    match ip {
        Some(ip) => ip.is_loopback(),
        None => host == "localhost" || host.ends_with(".localhost"),
    }
}
