//! A best-effort static scan of an app's HTML before it is framed (spec 3.3 §6.1
//! `genui-apps`): a decision's words anywhere a person could read them, and sites the app
//! loads or connects to that it did not declare. An app that builds its page in code passes
//! the scan; what it can do is still bounded by the sandbox, the CSP and the bridge.

use std::sync::LazyLock;

use regex::Regex;

use super::sites::{self, Declared, Refusal};
use crate::genui::AppBlockReason;
use crate::genui::reserved;

/// Text a person reads: between tags.
static TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)>([^<]+)<").expect("the text pattern compiles"));
/// Attributes a page shows as text.
static SHOWN_ATTRIBUTES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(?:value|title|aria-label|placeholder|alt|label|content)\s*=\s*("[^"]*"|'[^']*')"#,
    )
    .expect("the attribute pattern compiles")
});
/// String literals in scripts and styles.
static LITERALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""((?:[^"\\\n]|\\.)*)"|'((?:[^'\\\n]|\\.)*)'|`((?:[^`\\]|\\.)*)`"#)
        .expect("the literal pattern compiles")
});
/// URLs a page loads from or connects to.
static LOADS: [LazyLock<Regex>; 5] = [
    LazyLock::new(|| {
        Regex::new(r#"(?i)\b(?:src|srcset|data|poster|action|formaction|background)\s*=\s*["']?\s*([^"'\s>]+)"#)
            .expect("the source pattern compiles")
    }),
    LazyLock::new(|| {
        Regex::new(r#"(?i)<link\b[^>]*?\bhref\s*=\s*["']?\s*([^"'\s>]+)"#)
            .expect("the link pattern compiles")
    }),
    LazyLock::new(|| {
        Regex::new(r#"(?i)(?:url\(\s*["']?|@import\s+["'])\s*([^"')\s]+)"#)
            .expect("the style pattern compiles")
    }),
    LazyLock::new(|| {
        Regex::new(
            r#"(?i)\b(?:fetch|EventSource|WebSocket|import|sendBeacon|importScripts)\s*\(\s*["'`]([^"'`\s]+)"#,
        )
        .expect("the script pattern compiles")
    }),
    LazyLock::new(|| {
        Regex::new(r#"(?i)\.open\s*\(\s*["'`][A-Za-z]+["'`]\s*,\s*["'`]([^"'`\s]+)"#)
            .expect("the request pattern compiles")
    }),
];

/// Decodes the entities a page could hide a decision's words behind.
fn decoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        // Bytes, not characters: the window may end inside a character, and ';' is ASCII.
        let window = &rest.as_bytes()[..rest.len().min(12)];
        let Some(end) = window.iter().position(|byte| *byte == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let character = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match character {
            Some(character) => {
                out.push(character);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The first reserved word or phrase in `html`'s text, shown attributes or literals, with the
/// text it was found in.
fn decision_words(html: &str) -> Option<(String, &'static str)> {
    let texts = TEXT
        .captures_iter(html)
        .filter_map(|found| found.get(1))
        .chain(SHOWN_ATTRIBUTES.captures_iter(html).filter_map(|found| found.get(1)))
        .chain(
            LITERALS
                .captures_iter(html)
                .filter_map(|found| found.get(1).or(found.get(2)).or(found.get(3))),
        )
        .map(|found| decoded(&found.as_str().replace("\\'", "'").replace("\\\"", "\"")));
    for text in texts {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(word) = reserved(text) {
            return Some((text.chars().take(80).collect(), word));
        }
        // A label split by markup across one line of text reads as one label.
        for line in text.lines() {
            if let Some(word) = reserved(line.trim()) {
                return Some((line.trim().chars().take(80).collect(), word));
            }
        }
    }
    None
}

/// Why `html` is blocked, or `None` when the scan finds nothing.
pub(crate) fn scan(html: &str, declared: &Declared, refused_hosts: &[String]) -> Option<Refusal> {
    if let Some((text, word)) = decision_words(html) {
        return Some((
            AppBlockReason::ApprovalButtons,
            format!("its page shows \"{text}\", which matches the reserved words \"{word}\""),
        ));
    }
    for pattern in &LOADS {
        for found in pattern.captures_iter(html).filter_map(|found| found.get(1)) {
            let url = decoded(found.as_str());
            let absolute = match url.strip_prefix("//") {
                Some(rest) => format!("https://{rest}"),
                None => url.clone(),
            };
            let Some(parsed) = sites::parse(&absolute, true) else {
                continue;
            };
            let host = parsed_host(&absolute);
            if sites::is_this_computer(&host) || refused_hosts.contains(&host) {
                return Some((
                    AppBlockReason::ThisComputer,
                    format!("its page loads '{url}', on this computer or the home"),
                ));
            }
            if !declared.covers(&parsed) {
                return Some((
                    AppBlockReason::UndeclaredSites,
                    format!("its page loads '{url}', which its declaration does not list"),
                ));
            }
        }
    }
    None
}

/// The host of `url`, which [`sites::parse`] accepted.
fn parsed_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host = if authority.starts_with('[') {
        authority.split_once(']').map_or(authority.to_string(), |(host, _)| format!("{host}]"))
    } else {
        authority.rsplit_once(':').map_or(authority, |(host, _)| host).to_string()
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_tool::mcp::McpUiCsp;

    fn declared(connect: &[&str]) -> Declared {
        sites::checked(
            &McpUiCsp {
                connect_domains: connect.iter().map(|site| (*site).to_string()).collect(),
                ..McpUiCsp::default()
            },
            &[],
        )
        .unwrap()
    }

    #[test]
    fn a_decisions_words_block_the_app_however_they_are_written() {
        for html in [
            "<div class=card>Approved ✓ Allow once</div>",
            "<button>Approve</button>",
            "<input type=button value='Allow once'>",
            "<div>Allow&#32;once</div>",
            "<div>A&#x6c;ways allow for this site</div>",
            "<button aria-label=\"Allow once\"></button>",
            "<script>const label = 'Confirm it\\'s you and approve';</script>",
            "<b>Waiting for you</b>",
            "<p>Stop all</p>",
        ] {
            assert_eq!(
                scan(html, &Declared::default(), &[]).map(|refusal| refusal.0),
                Some(AppBlockReason::ApprovalButtons),
                "{html}"
            );
        }
    }

    #[test]
    fn an_entity_before_text_in_any_script_is_read_whole() {
        for html in [
            "<p>Q&A: 天気予報です</p>",
            "<p>&amp;日本語のテキスト</p>",
            "<p>Rain & 🌧🌧🌧 later</p>",
            "<img src=\"https://api.weather.example/x?a=1&b=é\">",
        ] {
            assert_eq!(
                scan(html, &declared(&["https://api.weather.example"]), &[]),
                None,
                "{html}"
            );
        }
        assert_eq!(decoded("&#65;&日本"), "A&日本");
    }

    #[test]
    fn ordinary_words_pass() {
        let html = "<h1>Nairobi</h1><p>24° · Light rain from 15:00</p><button>Alert me if it \
                    rains</button><button>Edit address</button>\
                    <svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(scan(html, &Declared::default(), &[]), None);
    }

    #[test]
    fn a_site_it_did_not_declare_or_this_computer_blocks_the_app() {
        let weather = declared(&["https://api.weather.example"]);
        let cases = [
            ("<script>fetch('https://api.weather.example/v1')</script>", None),
            (
                "<script>fetch('https://tracker.example/x')</script>",
                Some(AppBlockReason::UndeclaredSites),
            ),
            ("<img src=\"//cdn.example/pixel.png\">", Some(AppBlockReason::UndeclaredSites)),
            (
                "<script>new WebSocket(\"ws://127.0.0.1:8080/api\")</script>",
                Some(AppBlockReason::ThisComputer),
            ),
            (
                "<script>x.open('POST', 'http://localhost:7070/api/surface/commands')</script>",
                Some(AppBlockReason::ThisComputer),
            ),
            (
                "<link rel=stylesheet href='https://home.example/x.css'>",
                Some(AppBlockReason::ThisComputer),
            ),
        ];
        for (html, expected) in cases {
            assert_eq!(
                scan(html, &weather, &["home.example".to_string()]).map(|refusal| refusal.0),
                expected,
                "{html}"
            );
        }
    }
}
