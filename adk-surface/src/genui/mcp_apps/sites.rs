//! The sites an app may reach (spec 3.3 §6.1 `genui-apps`): each declared site checked, then
//! the app's content security policy built from what passed. A site on this computer, its
//! network or the home itself is refused in every list; adk-ui's `is_allowed_domain`, which
//! let `http://localhost` and `http://127.0.0.1` through, is not ported.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use adk_tool::mcp::McpUiCsp;

use crate::genui::AppBlockReason;

/// A declared site that passed: `https://api.weather.example`, `wss://*.example.com:8443`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    scheme: String,
    /// The host, lowercase; a wildcard keeps its `*.`.
    host: String,
    port: Option<u16>,
}

impl Site {
    fn source(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{port}", self.scheme, self.host),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }

    /// Whether this declaration covers `url`'s origin.
    fn covers(&self, url: &Parsed) -> bool {
        let scheme = match (self.scheme.as_str(), url.scheme.as_str()) {
            ("https", "https") | ("wss", "wss") => true,
            // A connect source with `https` also covers its secure socket in CSP 3.
            ("https", "wss") => true,
            _ => false,
        };
        let host = match self.host.strip_prefix("*.") {
            Some(suffix) => url.host.ends_with(&format!(".{suffix}")),
            None => url.host == self.host,
        };
        scheme && host && (self.port.is_none() || self.port == url.port)
    }
}

/// A URL's origin, as the checks read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    scheme: String,
    host: String,
    port: Option<u16>,
}

/// `text` as `scheme://host[:port]`, with nothing after the host but an optional path when
/// `path` allows one, or `None`.
pub(crate) fn parse(text: &str, path: bool) -> Option<Parsed> {
    let (scheme, rest) = text.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+') {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if !path && end != rest.len() && &rest[end..] != "/" {
        return None;
    }
    let authority = &rest[..end];
    // Credentials in a URL hide its real host from a reader.
    if authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        // Only an IPv6 address is bracketed; anything else could end a policy's directive.
        let address: Ipv6Addr = host.parse().ok()?;
        (format!("[{address}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port.parse().ok()?)),
            None => (authority.to_string(), None),
        }
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let named = host.strip_prefix("*.").unwrap_or(&host);
    // A wildcard covers the subdomains of a name of two labels or more, never a top-level
    // domain; the public suffix list is not consulted.
    let wildcard_ok = !host.starts_with('*') || named.contains('.');
    let valid = !host.is_empty()
        && wildcard_ok
        && (host.starts_with('[')
            || named.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'));
    valid.then_some(Parsed { scheme, host, port })
}

impl Parsed {
    /// The host, lowercase, an IPv6 address in brackets.
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    /// Whether this is a single origin a person can open, not a wildcard.
    pub(crate) fn is_plain(&self) -> bool {
        !self.host.starts_with('*')
    }
}

/// Whether `host` is this computer, its network, or a name only it resolves.
pub(crate) fn is_this_computer(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return local_ip(ip);
    }
    // Decimal, octal and hex forms such as `2130706433` and `0x7f.1` reach 127.0.0.1 in a
    // browser, so a host of digits and dots that is not a plain address is refused too.
    if bare.chars().all(|c| c.is_ascii_digit() || c == '.')
        || bare.split('.').any(|label| label.starts_with("0x"))
    {
        return true;
    }
    // A single-label name resolves on the local network, and a tailnet's names resolve to its
    // devices.
    !bare.contains('.')
        || [".localhost", ".local", ".internal", ".home.arpa", ".lan", ".ts.net"]
            .iter()
            .any(|suffix| bare.ends_with(suffix))
}

fn local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => local_v4(ip),
        IpAddr::V6(ip) => {
            ip.to_ipv4_mapped().is_some_and(local_v4)
                || ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || ip == Ipv6Addr::LOCALHOST
        }
    }
}

fn local_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        // Shared address space (RFC 6598), which carrier networks route locally.
        || (a == 100 && (64..128).contains(&b))
}

/// Why a site is refused, with the technical reason.
pub(crate) type Refusal = (AppBlockReason, String);

/// The declared sites, checked: every list's sites, or why the app is blocked. `refused`
/// are origins no list may name: the home's and the apps origin.
pub(crate) fn checked(csp: &McpUiCsp, refused: &[String]) -> Result<Declared, Refusal> {
    let refused: Vec<Parsed> = refused.iter().filter_map(|origin| parse(origin, true)).collect();
    let list = |name: &str, entries: &[String]| -> Result<Vec<Site>, Refusal> {
        entries
            .iter()
            .map(|entry| {
                let entry = entry.trim();
                let Some(parsed) = parse(entry, false) else {
                    return Err((
                        AppBlockReason::UndeclaredSites,
                        format!("{name} lists '{entry}', which is not an origin"),
                    ));
                };
                // A wildcard over a refused host names it too.
                let names = |host: &str| match parsed.host.strip_prefix("*.") {
                    Some(suffix) => host.ends_with(&format!(".{suffix}")),
                    None => host == parsed.host,
                };
                if is_this_computer(&parsed.host)
                    || refused.iter().any(|origin| names(&origin.host))
                {
                    return Err((
                        AppBlockReason::ThisComputer,
                        format!("{name} lists '{entry}', on this computer or the home"),
                    ));
                }
                if !matches!(parsed.scheme.as_str(), "https" | "wss") {
                    return Err((
                        AppBlockReason::UndeclaredSites,
                        format!("{name} lists '{entry}'; only https and wss sites are allowed"),
                    ));
                }
                Ok(Site { scheme: parsed.scheme, host: parsed.host, port: parsed.port })
            })
            .collect()
    };
    Ok(Declared {
        connect: list("connectDomains", &csp.connect_domains)?,
        resource: list("resourceDomains", &csp.resource_domains)?,
        frame: list("frameDomains", &csp.frame_domains)?,
        base: list("baseUriDomains", &csp.base_uri_domains)?,
    })
}

/// The sites an app declared, checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Declared {
    connect: Vec<Site>,
    resource: Vec<Site>,
    frame: Vec<Site>,
    base: Vec<Site>,
}

impl Declared {
    /// Whether some declared site covers `url`.
    pub(crate) fn covers(&self, url: &Parsed) -> bool {
        self.connect.iter().chain(&self.resource).chain(&self.frame).any(|site| site.covers(url))
    }

    /// The app's content security policy: nothing but inline code and the declared sites, no
    /// forms, plugins or workers, framed only by `ancestors`, and sandboxed even when opened on
    /// its own.
    pub(crate) fn policy(&self, ancestors: &[String]) -> String {
        let sources = |sites: &[Site], extra: &str| {
            let mut sources: Vec<String> = Vec::new();
            if !extra.is_empty() {
                sources.push(extra.to_string());
            }
            sources.extend(sites.iter().map(Site::source));
            if sources.is_empty() { "'none'".to_string() } else { sources.join(" ") }
        };
        let ancestors =
            if ancestors.is_empty() { "'none'".to_string() } else { ancestors.join(" ") };
        [
            "default-src 'none'".to_string(),
            format!("script-src {}", sources(&self.resource, "'unsafe-inline'")),
            format!("style-src {}", sources(&self.resource, "'unsafe-inline'")),
            format!("img-src {}", sources(&self.resource, "data: blob:")),
            format!("font-src {}", sources(&self.resource, "data:")),
            format!("media-src {}", sources(&self.resource, "data: blob:")),
            format!("connect-src {}", sources(&self.connect, "")),
            format!("frame-src {}", sources(&self.frame, "")),
            format!("base-uri {}", sources(&self.base, "")),
            "form-action 'none'".to_string(),
            "object-src 'none'".to_string(),
            "worker-src 'none'".to_string(),
            format!("frame-ancestors {ancestors}"),
            "sandbox allow-scripts allow-forms".to_string(),
        ]
        .join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csp(connect: &[&str]) -> McpUiCsp {
        McpUiCsp {
            connect_domains: connect.iter().map(|site| (*site).to_string()).collect(),
            ..McpUiCsp::default()
        }
    }

    #[test]
    fn this_computer_and_the_home_are_refused_in_every_list() {
        let home = ["http://127.0.0.1:8080".to_string(), "http://192.168.1.20:8080".to_string()];
        for site in [
            "http://127.0.0.1",
            "https://localhost:3000",
            "https://[::1]",
            "https://10.0.0.4",
            "https://192.168.1.20",
            "https://169.254.169.254",
            "https://2130706433",
            "https://0x7f.1",
            "https://printer.local",
            "wss://kernel.localhost",
            "https://100.64.0.1",
            "https://[fd00::1]",
            "https://intranet",
        ] {
            assert_eq!(
                checked(&csp(&[site]), &home).unwrap_err().0,
                AppBlockReason::ThisComputer,
                "{site}"
            );
        }
    }

    #[test]
    fn only_plain_https_and_wss_origins_pass() {
        for site in [
            "http://api.weather.example",
            "https://api.weather.example/path",
            "https://a.example; script-src *",
            "'self'",
            "*",
            "https://user@evil.example",
            "https://*",
            "https://*.com",
        ] {
            assert_eq!(
                checked(&csp(&[site]), &[]).unwrap_err().0,
                AppBlockReason::UndeclaredSites,
                "{site}"
            );
        }
        let declared =
            checked(&csp(&["https://api.weather.example", "wss://*.weather.example:8443"]), &[])
                .unwrap();
        assert!(
            declared
                .policy(&[])
                .contains("connect-src https://api.weather.example wss://*.weather.example:8443")
        );
    }

    #[test]
    fn a_policy_without_declarations_reaches_nothing() {
        assert_eq!(
            Declared::default().policy(&["http://127.0.0.1:8080".to_string()]),
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; \
             frame-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; \
             worker-src 'none'; frame-ancestors http://127.0.0.1:8080; \
             sandbox allow-scripts allow-forms"
        );
    }

    #[test]
    fn a_bracketed_host_is_an_ipv6_address_or_nothing() {
        for site in [
            "https://[a.b; connect-src *; x]",
            "https://[a.b\u{1}]",
            "https://[evil.example]",
            "https://[2001:db8::1; x]",
        ] {
            assert_eq!(
                checked(&csp(&[site]), &[]).unwrap_err().0,
                AppBlockReason::UndeclaredSites,
                "{site:?}"
            );
        }
        assert_eq!(
            parse("https://[::ffff:127.0.0.1]:8080/ui/", true).unwrap().host,
            "[::ffff:127.0.0.1]"
        );
        assert!(is_this_computer(&parse("https://[::ffff:127.0.0.1]", false).unwrap().host));
        let declared = checked(&csp(&["https://[2001:db8::1]:8443"]), &[]).unwrap();
        assert!(declared.policy(&[]).contains("connect-src https://[2001:db8::1]:8443;"));
    }

    #[test]
    fn a_wildcard_over_the_home_or_a_tailnet_is_refused() {
        let home = ["https://home.example.net".to_string(), "https://apps.example.org".to_string()];
        for site in [
            "https://home.example.net",
            "https://*.example.net",
            "wss://*.example.org",
            "https://*.tail1234.ts.net",
            "https://nas.tail1234.ts.net",
        ] {
            assert_eq!(
                checked(&csp(&[site]), &home).unwrap_err().0,
                AppBlockReason::ThisComputer,
                "{site}"
            );
        }
        assert!(checked(&csp(&["https://*.weather.example"]), &home).is_ok());
    }

    #[test]
    fn a_wildcard_covers_subdomains_only() {
        let declared = checked(&csp(&["https://*.weather.example"]), &[]).unwrap();
        let url = |text: &str| parse(text, true).unwrap();
        assert!(declared.covers(&url("https://api.weather.example/v1")));
        assert!(!declared.covers(&url("https://weather.example/")));
        assert!(!declared.covers(&url("https://api.weather.example.evil/")));
        assert!(!declared.covers(&url("http://api.weather.example/")));
    }
}
