//! Redirects in the host's format (spec 3.5 §7.8 "Generator", host conformance "redirect
//! round-trip").
//!
//! A withdrawn page that answers 410 Gone is a redirect with status 410 to the generated
//! [`GONE_PAGE`].

use awp_types::Redirect;
use serde::{Deserialize, Serialize};

use crate::error::{Result, SiteError};

/// The page a 410 rule serves.
pub const GONE_PAGE: &str = "/410.html";
/// The file of [`RedirectsFormat::Json`].
pub const JSON_REDIRECTS_FILE: &str = "_redirects.json";
/// The file of [`RedirectsFormat::Netlify`].
pub const NETLIFY_REDIRECTS_FILE: &str = "_redirects";

/// How a host reads redirects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RedirectsFormat {
    /// `/_redirects.json`: the [`Redirect`] list as JSON, read by `DirectoryHost` and the
    /// agent edge.
    #[default]
    Json,
    /// `/_redirects`: one `from to status` rule per line (Netlify, Cloudflare Pages).
    Netlify,
    /// A page at each source with a meta refresh and a canonical link, for hosts with no
    /// redirect rules; a 410 rule becomes a "removed" page.
    HtmlMeta,
}

fn stub_path(from: &str) -> String {
    let trimmed = from.trim_start_matches('/');
    if trimmed.is_empty() {
        "index.html".into()
    } else if trimmed.ends_with(".html") || trimmed.ends_with(".htm") {
        trimmed.into()
    } else {
        format!("{}/index.html", trimmed.trim_end_matches('/'))
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

impl RedirectsFormat {
    /// The files that carry `redirects` in this format, as `(path, bytes)` with paths
    /// relative to the site root.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_site::redirects::RedirectsFormat;
    /// use awp_types::Redirect;
    ///
    /// let rules = vec![Redirect { from: "/old/".into(), to: "/new/".into(), status: 301 }];
    /// let files = RedirectsFormat::Netlify.render(&rules);
    /// assert_eq!(files[0].0, "_redirects");
    /// assert_eq!(RedirectsFormat::Netlify.parse(&files).unwrap(), rules);
    /// ```
    pub fn render(self, redirects: &[Redirect]) -> Vec<(String, Vec<u8>)> {
        match self {
            Self::Json => {
                let mut text = serde_json::to_string_pretty(redirects).unwrap_or_else(|_| "[]".into());
                text.push('\n');
                vec![(JSON_REDIRECTS_FILE.into(), text.into_bytes())]
            }
            Self::Netlify => {
                let mut text = String::new();
                for rule in redirects {
                    text.push_str(&format!("{} {} {}\n", rule.from, rule.to, rule.status));
                }
                vec![(NETLIFY_REDIRECTS_FILE.into(), text.into_bytes())]
            }
            Self::HtmlMeta => redirects
                .iter()
                .map(|rule| {
                    let marker = format!("{} {}", rule.status, escape(&rule.to));
                    let page = if rule.status == 410 {
                        format!(
                            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                             <meta name=\"awp-redirect\" content=\"{marker}\">\n<meta name=\"robots\" content=\"noindex\">\n\
                             <title>Page removed</title>\n</head>\n<body>\n<main>\n<h1>Page removed</h1>\n\
                             <p>This page was withdrawn and is no longer available.</p>\n</main>\n</body>\n</html>\n"
                        )
                    } else {
                        let to = escape(&rule.to);
                        format!(
                            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                             <meta name=\"awp-redirect\" content=\"{marker}\">\n<meta http-equiv=\"refresh\" content=\"0; url={to}\">\n\
                             <link rel=\"canonical\" href=\"{to}\">\n<title>Moved</title>\n</head>\n<body>\n<main>\n\
                             <h1>Moved</h1>\n<p>This page moved to <a href=\"{to}\">{to}</a>.</p>\n</main>\n</body>\n</html>\n"
                        )
                    };
                    (stub_path(&rule.from), page.into_bytes())
                })
                .collect(),
        }
    }

    /// Reads redirects back from the files [`render`](Self::render) produced.
    ///
    /// # Errors
    ///
    /// [`SiteError::Serde`] when a file is not in this format.
    pub fn parse(self, files: &[(String, Vec<u8>)]) -> Result<Vec<Redirect>> {
        match self {
            Self::Json => {
                let Some((_, bytes)) = files.iter().find(|(path, _)| path == JSON_REDIRECTS_FILE)
                else {
                    return Ok(Vec::new());
                };
                serde_json::from_slice(bytes).map_err(|e| SiteError::serde(JSON_REDIRECTS_FILE, e))
            }
            Self::Netlify => {
                let Some((_, bytes)) =
                    files.iter().find(|(path, _)| path == NETLIFY_REDIRECTS_FILE)
                else {
                    return Ok(Vec::new());
                };
                let text = String::from_utf8_lossy(bytes);
                text.lines()
                    .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
                    .map(|line| {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        match parts.as_slice() {
                            [from, to] => {
                                Ok(Redirect { from: (*from).into(), to: (*to).into(), status: 301 })
                            }
                            [from, to, status, ..] => Ok(Redirect {
                                from: (*from).into(),
                                to: (*to).into(),
                                status: status.trim_end_matches('!').parse().map_err(|_| {
                                    SiteError::serde(
                                        NETLIFY_REDIRECTS_FILE,
                                        format!("bad status in '{line}'"),
                                    )
                                })?,
                            }),
                            _ => Err(SiteError::serde(
                                NETLIFY_REDIRECTS_FILE,
                                format!("bad rule '{line}'"),
                            )),
                        }
                    })
                    .collect()
            }
            Self::HtmlMeta => {
                let mut redirects = Vec::new();
                for (path, bytes) in files {
                    let text = String::from_utf8_lossy(bytes);
                    const MARKER: &str = "<meta name=\"awp-redirect\" content=\"";
                    let Some(start) = text.find(MARKER) else { continue };
                    let rest = &text[start + MARKER.len()..];
                    let content = &rest[..rest.find('"').unwrap_or(0)];
                    let (status, to) = content.split_once(' ').ok_or_else(|| {
                        SiteError::serde(path.clone(), "malformed awp-redirect marker")
                    })?;
                    let from = match path.strip_suffix("index.html") {
                        Some("") => "/".to_string(),
                        Some(dir) => format!("/{dir}"),
                        None => format!("/{path}"),
                    };
                    redirects.push(Redirect {
                        from,
                        to: unescape(to),
                        status: status
                            .parse()
                            .map_err(|_| SiteError::serde(path.clone(), "bad status"))?,
                    });
                }
                Ok(redirects)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<Redirect> {
        vec![
            Redirect { from: "/2019/05/hello/".into(), to: "/posts/hello/".into(), status: 301 },
            Redirect {
                from: "/old.html".into(),
                to: "https://example.org/x?a=1&b=2".into(),
                status: 302,
            },
            Redirect { from: "/gone/".into(), to: GONE_PAGE.into(), status: 410 },
        ]
    }

    #[test]
    fn every_format_round_trips() {
        for format in [RedirectsFormat::Json, RedirectsFormat::Netlify, RedirectsFormat::HtmlMeta] {
            let files = format.render(&rules());
            let mut parsed = format.parse(&files).unwrap();
            parsed.sort_by(|a, b| a.from.cmp(&b.from));
            let mut expected = rules();
            expected.sort_by(|a, b| a.from.cmp(&b.from));
            assert_eq!(parsed, expected, "{format:?}");
        }
    }

    #[test]
    fn stubs_sit_at_their_sources() {
        let files = RedirectsFormat::HtmlMeta.render(&rules());
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["2019/05/hello/index.html", "old.html", "gone/index.html"]);
        assert!(crate::scan::scan_file("gone/index.html", &files[2].1).is_empty());
    }
}
