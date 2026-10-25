//! `robots.txt` (RFC 9309): the crawler reads it before any other request to an origin and
//! never fetches what it disallows (spec 3.5 §7.8 "Sources": `Crawl` is robots-respecting).

use std::time::Duration;

use regex::Regex;

/// One `Allow` or `Disallow` rule.
#[derive(Debug, Clone)]
struct Rule {
    allow: bool,
    length: usize,
    matcher: Regex,
}

impl Rule {
    /// RFC 9309 §2.2.3: `*` matches any run of characters, a final `$` anchors the end.
    fn new(allow: bool, pattern: &str) -> Option<Self> {
        let (body, anchored) = match pattern.strip_suffix('$') {
            Some(body) => (body, true),
            None => (pattern, false),
        };
        let mut source = String::from("^");
        source.push_str(&body.split('*').map(regex::escape).collect::<Vec<_>>().join(".*"));
        if anchored {
            source.push('$');
        }
        Regex::new(&source).ok().map(|matcher| Self { allow, length: pattern.len(), matcher })
    }
}

/// A group of rules for one or more user agents.
#[derive(Debug, Clone, Default)]
struct Group {
    agents: Vec<String>,
    rules: Vec<Rule>,
    crawl_delay: Option<Duration>,
}

/// A parsed `robots.txt`.
///
/// Matching follows RFC 9309: the group naming the crawler's product token (case-insensitive)
/// applies, else the `*` group; within it the longest matching pattern wins and `Allow` wins
/// a tie; `*` matches any run of characters and a trailing `$` anchors the end.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::robots::RobotsTxt;
///
/// let robots = RobotsTxt::parse("User-agent: *\nDisallow: /wp-admin/\nAllow: /wp-admin/admin-ajax.php\n");
/// assert!(robots.allows("adk-site", "/about/"));
/// assert!(!robots.allows("adk-site", "/wp-admin/options.php"));
/// assert!(robots.allows("adk-site", "/wp-admin/admin-ajax.php"));
/// ```
#[derive(Debug, Clone, Default)]
pub struct RobotsTxt {
    groups: Vec<Group>,
    sitemaps: Vec<String>,
    disallow_all: bool,
}

impl RobotsTxt {
    /// Everything allowed: the answer when `robots.txt` is absent (4xx).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Everything disallowed: the answer when `robots.txt` cannot be read (5xx or no answer),
    /// as RFC 9309 §2.3.1.3 requires.
    pub fn disallow_all() -> Self {
        Self { disallow_all: true, ..Self::default() }
    }

    /// Parses a `robots.txt` body. Unknown lines are ignored.
    pub fn parse(text: &str) -> Self {
        let mut robots = Self::default();
        let mut current: Option<Group> = None;
        let mut last_was_agent = false;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else { continue };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" => {
                    if !last_was_agent {
                        if let Some(group) = current.take() {
                            robots.groups.push(group);
                        }
                        current = Some(Group::default());
                    }
                    if let Some(group) = current.as_mut() {
                        group.agents.push(value.to_ascii_lowercase());
                    }
                    last_was_agent = true;
                }
                "allow" | "disallow" => {
                    last_was_agent = false;
                    if let Some(group) = current.as_mut()
                        && !value.is_empty()
                        && let Some(rule) = Rule::new(key == "allow", value)
                    {
                        group.rules.push(rule);
                    }
                }
                "crawl-delay" => {
                    last_was_agent = false;
                    if let Some(group) = current.as_mut()
                        && let Ok(seconds) = value.parse::<f64>()
                        && seconds.is_finite()
                        && seconds >= 0.0
                    {
                        group.crawl_delay = Some(Duration::from_secs_f64(seconds.min(60.0)));
                    }
                }
                "sitemap" => robots.sitemaps.push(value.to_string()),
                _ => {}
            }
        }
        if let Some(group) = current {
            robots.groups.push(group);
        }
        robots
    }

    fn group_for(&self, token: &str) -> Option<&Group> {
        let token = token.to_ascii_lowercase();
        self.groups
            .iter()
            .find(|g| g.agents.iter().any(|a| a == &token))
            .or_else(|| self.groups.iter().find(|g| g.agents.iter().any(|a| a == "*")))
    }

    /// Whether the crawler with product token `token` may fetch `path` (path and query).
    pub fn allows(&self, token: &str, path: &str) -> bool {
        if self.disallow_all {
            return false;
        }
        if path == "/robots.txt" {
            return true;
        }
        let Some(group) = self.group_for(token) else { return true };
        let mut best: Option<(usize, bool)> = None;
        for rule in group.rules.iter().filter(|rule| rule.matcher.is_match(path)) {
            best = match best {
                Some((len, allow)) if len > rule.length || (len == rule.length && allow) => {
                    Some((len, allow))
                }
                _ => Some((rule.length, rule.allow)),
            };
        }
        best.is_none_or(|(_, allow)| allow)
    }

    /// The `Crawl-delay` of the group that applies to `token`.
    pub fn crawl_delay(&self, token: &str) -> Option<Duration> {
        self.group_for(token).and_then(|g| g.crawl_delay)
    }

    /// The sitemaps the file lists.
    pub fn sitemaps(&self) -> &[String] {
        &self.sitemaps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_match_wins_and_allow_wins_ties() {
        let robots = RobotsTxt::parse(
            "User-agent: *\nDisallow: /shop\nAllow: /shop/catalogue\nDisallow: /*.pdf$\n\
             Allow: /a\nDisallow: /a\n",
        );
        assert!(!robots.allows("x", "/shop/cart"));
        assert!(robots.allows("x", "/shop/catalogue/page"));
        assert!(!robots.allows("x", "/files/manual.pdf"));
        assert!(robots.allows("x", "/files/manual.pdf?download=1"));
        assert!(robots.allows("x", "/a"), "allow wins an equal-length tie");
    }

    #[test]
    fn the_named_group_beats_the_star_group() {
        let robots = RobotsTxt::parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: adk-site\nUser-agent: other\nDisallow: /private/\nCrawl-delay: 2\nSitemap: https://e.test/sitemap.xml\n",
        );
        assert!(robots.allows("ADK-Site", "/about/"));
        assert!(!robots.allows("adk-site", "/private/x"));
        assert!(!robots.allows("somebot", "/about/"));
        assert_eq!(robots.crawl_delay("adk-site"), Some(Duration::from_secs(2)));
        assert_eq!(robots.sitemaps(), ["https://e.test/sitemap.xml"]);
    }

    #[test]
    fn unreadable_robots_disallows_everything() {
        assert!(!RobotsTxt::disallow_all().allows("x", "/"));
        assert!(RobotsTxt::allow_all().allows("x", "/anything"));
        assert!(RobotsTxt::parse("User-agent: *\nDisallow:\n").allows("x", "/"));
    }
}
