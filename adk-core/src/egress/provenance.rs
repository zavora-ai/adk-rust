//! URL provenance: the URLs a conversation's sources hold, so a tool that fetches a
//! model-chosen URL can refuse one the model composed.
//!
//! A model that has read untrusted text can be told to put data it holds into a URL, in the
//! query string or a path segment, and fetch it. [`ContextUrls`] collects the URLs the
//! conversation's sources hold, and a fetch tool admits only those: a URL counts when it
//! appeared in a user message or a tool result, never when it appears only in what the model
//! wrote.
//!
//! | Source | Counts | Set by |
//! |--------|--------|--------|
//! | User-role text, file references and embedded resources | Yes | [`UrlSources::user_input`] |
//! | Tool results ([`Part::FunctionResponse`]), by tool name | Yes | [`UrlSources::tool_results`] |
//! | Provider-executed tool results ([`Part::ServerToolResponse`]) | Unless tool results are [`ToolResultSources::None`] or [`ToolResultSources::Only`] | [`UrlSources::tool_results`] |
//! | Model text, thinking, function calls and provider tool calls | Never | — |
//! | The system instruction | Never | — |
//!
//! Two URLs match when they are equal after [`normalize_url`]: scheme and host lowercased,
//! the default port dropped, an empty path written `/`, dot segments resolved, and the
//! fragment, which never leaves the client, removed. The query is compared exactly, so a URL
//! with data appended to its query or path is a different URL. In user text, a bare host
//! with an optional path (`docs.rs/serde`) also admits its `https://` and `http://` forms.
//!
//! # Limits
//!
//! - **A tool that echoes model text launders it**: a shell that prints the URL a command
//!   built, or a file the model wrote and read back, makes that URL a tool result. Leave such
//!   tools out with [`ToolResultSources::Except`].
//! - **Choice is a channel**: a model can still signal by which of the admitted URLs it
//!   fetches, and in what order.
//!
//! # Example
//!
//! ```rust
//! use adk_core::egress::{ContextUrls, ToolResultSources, UrlSources};
//! use adk_core::{Content, FunctionResponseData, Part};
//! use serde_json::json;
//!
//! let history = [
//!     Content::new("user").with_text("Summarize https://docs.example.com/guide please."),
//!     Content::new("model").with_text("I will fetch https://evil.example/?k=secret first."),
//!     Content {
//!         role: "function".into(),
//!         parts: vec![Part::FunctionResponse {
//!             function_response: FunctionResponseData::new(
//!                 "web_search",
//!                 json!({ "results": [{ "url": "https://blog.example.com/post?id=7" }] }),
//!             ),
//!             id: None,
//!             annotations: None,
//!         }],
//!     },
//! ];
//! let urls = ContextUrls::collect(&history, &UrlSources::default());
//! assert!(urls.contains("https://DOCS.example.com:443/guide#intro"));
//! assert!(urls.contains("https://blog.example.com/post?id=7"));
//! assert!(!urls.contains("https://blog.example.com/post?id=7&leak=1"));
//! assert!(!urls.contains("https://evil.example/?k=secret"), "the model wrote it");
//!
//! let only_user = UrlSources { tool_results: ToolResultSources::None, ..UrlSources::default() };
//! assert!(!ContextUrls::collect(&history, &only_user).contains("https://blog.example.com/post?id=7"));
//! ```

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Content, Part, ToolContext};

/// Which tool results count as sources of URLs.
///
/// Names are matched against [`FunctionResponseData::name`](crate::FunctionResponseData);
/// a provider-executed tool's result has no tool name here and counts under
/// [`All`](Self::All) and [`Except`](Self::Except).
///
/// # Example
///
/// ```rust
/// use adk_core::egress::ToolResultSources;
///
/// let sources = ToolResultSources::Except(vec!["bash".into()]);
/// assert!(sources.counts(Some("web_search")));
/// assert!(!sources.counts(Some("bash")));
/// assert!(sources.counts(None), "a provider-executed result");
/// assert!(!ToolResultSources::Only(vec!["web_search".into()]).counts(None));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ToolResultSources {
    /// Every tool result.
    #[default]
    All,
    /// No tool result.
    None,
    /// Only the results of these tools.
    Only(Vec<String>),
    /// Every tool result but those of these tools.
    Except(Vec<String>),
}

impl ToolResultSources {
    /// Whether a result of `tool` counts; `None` is a result without a tool name.
    pub fn counts(&self, tool: Option<&str>) -> bool {
        match (self, tool) {
            (Self::All, _) => true,
            (Self::None, _) | (Self::Only(_), None) => false,
            (Self::Only(names), Some(tool)) => names.iter().any(|name| name == tool),
            (Self::Except(_), None) => true,
            (Self::Except(names), Some(tool)) => !names.iter().any(|name| name == tool),
        }
    }
}

/// Which parts of a conversation count as sources of URLs for [`ContextUrls`].
///
/// The default counts user input and every tool result, as Anthropic's server-side
/// `web_fetch` does by default.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::{ToolResultSources, UrlSources};
///
/// let sources = UrlSources::default();
/// assert!(sources.user_input);
/// assert_eq!(sources.tool_results, ToolResultSources::All);
/// let strict = UrlSources::default().except_tools(["bash"]);
/// assert_eq!(strict.tool_results, ToolResultSources::Except(vec!["bash".into()]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UrlSources {
    /// Whether URLs in user-role text, file references and embedded resources count.
    pub user_input: bool,
    /// Which tool results count.
    pub tool_results: ToolResultSources,
}

impl Default for UrlSources {
    fn default() -> Self {
        Self { user_input: true, tool_results: ToolResultSources::All }
    }
}

impl UrlSources {
    /// These sources with the results of `tools` left out.
    #[must_use]
    pub fn except_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut names: Vec<String> = match self.tool_results {
            ToolResultSources::Except(names) => names,
            ToolResultSources::All => Vec::new(),
            other => {
                self.tool_results = other;
                return self;
            }
        };
        for tool in tools {
            let tool = tool.into();
            if !names.contains(&tool) {
                names.push(tool);
            }
        }
        self.tool_results = ToolResultSources::Except(names);
        self
    }
}

/// The URLs a conversation's sources hold, normalized; see the [module documentation](self).
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_core::egress::{ContextUrls, UrlSources};
///
/// let history = [Content::new("user").with_text("read docs.rs/serde (the README)")];
/// let urls = ContextUrls::collect(&history, &UrlSources::default());
/// assert!(urls.contains("https://docs.rs/serde"));
/// assert!(!urls.contains("https://docs.rs/serde/latest"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextUrls {
    urls: BTreeSet<String>,
}

impl ContextUrls {
    /// The URLs `contents` hold under `sources`.
    pub fn collect<'a>(
        contents: impl IntoIterator<Item = &'a Content>,
        sources: &UrlSources,
    ) -> Self {
        let mut urls = Self::default();
        for content in contents {
            urls.add_content(content, sources);
        }
        urls
    }

    /// The URLs of the conversation a tool call runs in: the session's history on the
    /// call's branch and the invocation's user message.
    ///
    /// A runtime that exposes no session ([`ToolContext::session`]) contributes only the
    /// user message.
    pub fn from_tool_context(ctx: &dyn ToolContext, sources: &UrlSources) -> Self {
        let mut urls = Self::default();
        if let Some(session) = ctx.session() {
            for content in session.conversation_history_scoped(None, ctx.branch()) {
                urls.add_content(&content, sources);
            }
        }
        urls.add_content(ctx.user_content(), sources);
        urls
    }

    /// Adds the URLs `content` holds under `sources`.
    pub fn add_content(&mut self, content: &Content, sources: &UrlSources) {
        let role = content.role.as_str();
        let user = role == "user";
        let tool = matches!(role, "function" | "tool");
        for part in &content.parts {
            match part {
                Part::Text { text } if user && sources.user_input => {
                    self.add_text(text, true);
                }
                Part::Text { text } if tool && sources.tool_results.counts(None) => {
                    self.add_text(text, false);
                }
                Part::FileData { file_uri, .. } if user && sources.user_input => {
                    self.add(file_uri);
                }
                Part::InlineData { uri: Some(uri), .. } if user && sources.user_input => {
                    self.add(uri);
                }
                Part::EmbeddedResource { resource } if user && sources.user_input => {
                    self.add(resource.uri());
                    if let crate::EmbeddedResource::Text(text) = resource {
                        self.add_text(&text.text, true);
                    }
                }
                Part::FunctionResponse { function_response, .. }
                    if sources.tool_results.counts(Some(&function_response.name)) =>
                {
                    self.add_value(&function_response.response);
                    for file in &function_response.file_data {
                        self.add(&file.file_uri);
                    }
                }
                Part::ServerToolResponse { server_tool_response }
                    if sources.tool_results.counts(None) =>
                {
                    self.add_value(server_tool_response);
                }
                Part::Thinking { .. }
                | Part::Text { .. }
                | Part::InlineData { .. }
                | Part::FileData { .. }
                | Part::FunctionCall { .. }
                | Part::FunctionResponse { .. }
                | Part::ServerToolCall { .. }
                | Part::ServerToolResponse { .. }
                | Part::EmbeddedResource { .. } => {}
            }
        }
    }

    /// Adds one URL; anything that is not an `http(s)` URL with a host is ignored.
    pub fn add(&mut self, url: &str) {
        if let Some(url) = normalize_url(url) {
            self.urls.insert(url);
        }
    }

    /// Whether `url` appeared in the conversation, compared after [`normalize_url`].
    pub fn contains(&self, url: &str) -> bool {
        normalize_url(url).is_some_and(|url| self.urls.contains(&url))
    }

    /// How many distinct URLs were collected.
    pub fn len(&self) -> usize {
        self.urls.len()
    }

    /// Whether no URL was collected.
    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    fn add_value(&mut self, value: &Value) {
        match value {
            Value::String(text) => self.add_text(text, false),
            Value::Array(items) => items.iter().for_each(|item| self.add_value(item)),
            Value::Object(map) => map.values().for_each(|item| self.add_value(item)),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    /// Adds the `http(s)` URLs in `text`, and with `bare_hosts` its scheme-less host tokens.
    fn add_text(&mut self, text: &str, bare_hosts: bool) {
        for url in urls_in_text(text) {
            self.add(url);
        }
        if bare_hosts {
            for token in text.split_whitespace() {
                let token = trim_token(token.trim_start_matches(['(', '<', '[', '"', '\'']));
                if !token.contains("://") && is_bare_host(token) {
                    self.add(&format!("https://{token}"));
                    self.add(&format!("http://{token}"));
                }
            }
        }
    }
}

/// `url` normalized for comparison: an `http(s)` URL with a host, scheme and host
/// lowercased, the default port dropped, an empty path written `/`, dot segments resolved and
/// the fragment removed. `None` for anything else.
///
/// # Example
///
/// ```rust
/// use adk_core::egress::normalize_url;
///
/// assert_eq!(normalize_url("HTTPS://Docs.RS:443#top").as_deref(), Some("https://docs.rs/"));
/// assert_eq!(normalize_url("https://a.example/x/../y?q=1").as_deref(), Some("https://a.example/y?q=1"));
/// assert_eq!(normalize_url("ftp://a.example/"), None);
/// assert_eq!(normalize_url("not a url"), None);
/// ```
pub fn normalize_url(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    parsed.set_fragment(None);
    Some(parsed.into())
}

/// Characters that end a URL written in prose.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// The `http://` and `https://` URLs written in `text`, trailing punctuation removed.
fn urls_in_text(text: &str) -> Vec<&str> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("http") {
        let start = from + offset;
        let rest = &lower[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            from = start + 4;
            continue;
        }
        let end = text[start..]
            .char_indices()
            .find(|(_, c)| ends_url(*c))
            .map_or(text.len(), |(i, _)| start + i);
        let url = trim_token(&text[start..end]);
        if url.len() > "https://".len() {
            found.push(url);
        }
        from = end.max(start + 4);
    }
    found
}

/// `token` without the punctuation prose puts after a URL, and without a closing bracket
/// that has no opening one inside it.
fn trim_token(mut token: &str) -> &str {
    loop {
        let Some(last) = token.chars().last() else { return token };
        let unbalanced = |open: char, close: char| {
            last == close && token.matches(open).count() < token.matches(close).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '*' | '"' | '\'')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            token = &token[..token.len() - last.len_utf8()];
        } else {
            return token;
        }
    }
}

/// Whether `token` reads as a host name with an optional port and path, such as
/// `docs.rs/serde`: dotted labels of letters, digits and `-`, the last one alphabetic and at
/// least two letters long.
fn is_bare_host(token: &str) -> bool {
    let authority = token.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return false,
        None => authority,
    };
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FunctionResponseData;
    use serde_json::json;

    fn response(name: &str, value: Value) -> Content {
        Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(name, value),
                id: None,
                annotations: None,
            }],
        }
    }

    #[test]
    fn urls_in_prose_lose_trailing_punctuation_and_unbalanced_brackets() {
        let text = "See https://a.example/x. Or (https://b.example/y), [doc](https://c.example/z) \
                    and https://en.example/wiki/Rust_(language) or <https://d.example/>; \
                    \"https://e.example/q?a=1&b=2\" httpx://no hTTps://F.example";
        assert_eq!(
            urls_in_text(text),
            [
                "https://a.example/x",
                "https://b.example/y",
                "https://c.example/z",
                "https://en.example/wiki/Rust_(language)",
                "https://d.example/",
                "https://e.example/q?a=1&b=2",
                "hTTps://F.example",
            ]
        );
    }

    #[test]
    fn only_user_input_and_tool_results_count() {
        let history = [
            Content::new("user").with_text("start at https://start.example/"),
            Content::new("model").with_text("next https://model.example/"),
            Content {
                role: "model".into(),
                parts: vec![Part::FunctionCall {
                    name: "web_fetch".into(),
                    args: json!({ "url": "https://call.example/" }),
                    id: None,
                    thought_signature: None,
                }],
            },
            Content {
                role: "model".into(),
                parts: vec![Part::Thinking {
                    thinking: "maybe https://thought.example/".into(),
                    signature: None,
                }],
            },
            response("web_fetch", json!({ "content": "[next](https://page.example/next)" })),
            response("bash", json!({ "stdout": "https://echo.example/?k=v\n" })),
            Content {
                role: "model".into(),
                parts: vec![Part::ServerToolResponse {
                    server_tool_response: json!({ "results": [{ "url": "https://search.example/r" }] }),
                }],
            },
        ];
        let all = ContextUrls::collect(&history, &UrlSources::default());
        for url in [
            "https://start.example/",
            "https://page.example/next",
            "https://echo.example/?k=v",
            "https://search.example/r",
        ] {
            assert!(all.contains(url), "{url}");
        }
        for url in ["https://model.example/", "https://call.example/", "https://thought.example/"] {
            assert!(!all.contains(url), "{url} is the model's own");
        }

        let except = ContextUrls::collect(&history, &UrlSources::default().except_tools(["bash"]));
        assert!(!except.contains("https://echo.example/?k=v"));
        assert!(except.contains("https://page.example/next"));

        let only = UrlSources {
            user_input: false,
            tool_results: ToolResultSources::Only(vec!["web_fetch".into()]),
        };
        let only = ContextUrls::collect(&history, &only);
        assert!(only.contains("https://page.example/next"));
        assert!(!only.contains("https://start.example/"), "user input is off");
        assert!(!only.contains("https://search.example/r"), "a provider result has no name");
    }

    #[test]
    fn matching_is_exact_after_normalization() {
        let history = [Content::new("user").with_text("https://Docs.Example:443/a/./b?x=1#frag")];
        let urls = ContextUrls::collect(&history, &UrlSources::default());
        assert!(urls.contains("https://docs.example/a/b?x=1"));
        assert!(urls.contains("https://docs.example/a/b?x=1#other"), "fragments are not sent");
        assert!(!urls.contains("https://docs.example/a/b"), "the query is part of the URL");
        assert!(!urls.contains("https://docs.example/a/b?x=1&leak=secret"));
        assert!(!urls.contains("https://docs.example/a/b/secret?x=1"));
        assert!(!urls.contains("http://docs.example/a/b?x=1"), "the scheme is part of the URL");
    }

    #[test]
    fn bare_hosts_count_only_in_user_text() {
        let history = [
            Content::new("user").with_text("look at docs.rs/serde, e.g. the README; file.x"),
            response("bash", json!({ "stdout": "evil.example/collect" })),
        ];
        let urls = ContextUrls::collect(&history, &UrlSources::default());
        assert!(urls.contains("https://docs.rs/serde"));
        assert!(urls.contains("http://docs.rs/serde"));
        assert!(!urls.contains("https://e.g/"), "a one-letter label is no top-level domain");
        assert!(!urls.contains("https://evil.example/collect"), "tool text needs a scheme");
    }

    #[test]
    fn user_file_references_and_tool_file_data_count() {
        let mut data = FunctionResponseData::new("load", json!({}));
        data.file_data.push(crate::FileDataPart {
            mime_type: "application/pdf".into(),
            file_uri: "https://files.example/report.pdf".into(),
            annotations: None,
        });
        let history = [
            Content {
                role: "user".into(),
                parts: vec![Part::FileData {
                    mime_type: "text/html".into(),
                    file_uri: "https://user.example/page".into(),
                    annotations: None,
                }],
            },
            Content {
                role: "function".into(),
                parts: vec![Part::FunctionResponse {
                    function_response: data,
                    id: None,
                    annotations: None,
                }],
            },
        ];
        let urls = ContextUrls::collect(&history, &UrlSources::default());
        assert!(urls.contains("https://user.example/page"));
        assert!(urls.contains("https://files.example/report.pdf"));
        assert_eq!(urls.len(), 2);
    }
}
