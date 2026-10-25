//! Internal link resolution against a release's files (spec 3.5 §7.8.1 "Links").

use std::collections::BTreeSet;

/// Where a URL in a page points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkTarget {
    /// Another origin, a mail or phone link, or a data URL: not checked.
    External,
    /// A fragment on the same page: not checked.
    Fragment,
    /// A file path in this site, `/`-separated, without a leading slash.
    Internal(String),
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Normalises a `/`-separated path: drops `.` segments and resolves `..` without leaving
/// the root.
pub(crate) fn normalize(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut joined = segments.join("/");
    if path.ends_with('/') && !joined.is_empty() {
        joined.push('/');
    }
    joined
}

/// Resolves `url` found in the file at `page` (a release path such as `about/index.html`)
/// for a site served at `domain`.
pub(crate) fn resolve(page: &str, url: &str, domain: &str) -> LinkTarget {
    let url = url.trim();
    if url.is_empty() {
        return LinkTarget::Fragment;
    }
    if url.starts_with('#') {
        return LinkTarget::Fragment;
    }
    let lower = url.to_ascii_lowercase();
    let path_part = if let Some(rest) = lower.strip_prefix("//").map(|_| &url[2..]) {
        let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
        if !same_site(host, domain) {
            return LinkTarget::External;
        }
        format!("/{path}")
    } else if let Some(scheme_end) = lower.find(':').filter(|&i| {
        lower[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            && !lower[..i].contains('/')
    }) {
        let scheme = &lower[..scheme_end];
        if scheme != "http" && scheme != "https" {
            return LinkTarget::External;
        }
        let rest = url[scheme_end + 1..].trim_start_matches('/');
        let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
        if !same_site(host, domain) {
            return LinkTarget::External;
        }
        format!("/{path}")
    } else {
        url.to_string()
    };
    let path_only = path_part.split(['?', '#']).next().unwrap_or_default();
    let decoded = percent_decode(path_only);
    let joined = if decoded.starts_with('/') {
        decoded
    } else {
        let dir = page.rsplit_once('/').map_or("", |(d, _)| d);
        format!("{dir}/{decoded}")
    };
    LinkTarget::Internal(normalize(&joined))
}

fn same_site(host: &str, domain: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    host == domain || host.strip_prefix("www.") == Some(domain.as_str())
}

/// Whether an internal path resolves to a file of the release or a redirect source.
pub(crate) fn exists(
    path: &str,
    files: &BTreeSet<String>,
    redirect_sources: &BTreeSet<String>,
) -> bool {
    let trimmed = path.trim_end_matches('/');
    let candidates = [
        path.to_string(),
        trimmed.to_string(),
        if trimmed.is_empty() { "index.html".to_string() } else { format!("{trimmed}/index.html") },
        format!("{trimmed}.html"),
    ];
    candidates.iter().any(|c| files.contains(c))
        || redirect_sources.contains(&format!("/{trimmed}"))
        || redirect_sources.contains(&format!("/{trimmed}/"))
        || (trimmed.is_empty() && redirect_sources.contains("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_resolve_against_the_page() {
        let d = "example.com";
        assert_eq!(
            resolve("blog/a/index.html", "../b/", d),
            LinkTarget::Internal("blog/b/".into())
        );
        assert_eq!(
            resolve("blog/a/index.html", "index.md", d),
            LinkTarget::Internal("blog/a/index.md".into())
        );
        assert_eq!(
            resolve("index.html", "/about/?x=1#top", d),
            LinkTarget::Internal("about/".into())
        );
        assert_eq!(
            resolve("index.html", "https://www.example.com/a%20b", d),
            LinkTarget::Internal("a b".into())
        );
        assert_eq!(
            resolve("index.html", "//example.com/x.png", d),
            LinkTarget::Internal("x.png".into())
        );
        assert_eq!(resolve("index.html", "https://other.org/", d), LinkTarget::External);
        assert_eq!(resolve("index.html", "mailto:a@example.com", d), LinkTarget::External);
        assert_eq!(resolve("index.html", "#main", d), LinkTarget::Fragment);
        assert_eq!(resolve("a/index.html", "../../../etc", d), LinkTarget::Internal("etc".into()));
    }

    #[test]
    fn existence_follows_directory_indexes_and_redirects() {
        let files: BTreeSet<String> = ["index.html", "about/index.html", "x.png", "terms.html"]
            .into_iter()
            .map(String::from)
            .collect();
        let redirects: BTreeSet<String> = ["/old-page"].into_iter().map(String::from).collect();
        for path in ["", "about/", "about", "x.png", "terms", "old-page", "old-page/"] {
            assert!(exists(path, &files, &redirects), "{path}");
        }
        assert!(!exists("missing/", &files, &redirects));
    }
}
