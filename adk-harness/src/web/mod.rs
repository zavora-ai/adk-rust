//! External content under governance (spec 3.1 §7.20).
//!
//! `web_fetch` and `web_search` are governed tools of class `Web { allowlisted }`: the
//! policy decides them by the URL's host against [`WebConfig::allowed_domains`], and
//! everything they return reaches the model only inside a delimited untrusted block,
//! size-capped, with injection matches labelled and secret patterns redacted
//! (spec 3.1 §9.2 I46–I49).
//!
//! The configuration types compile in every build; the tools, the fetch client, the
//! HTML-to-text pass and the screening need feature `web`.

use std::fmt;
#[cfg(feature = "web")]
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Result, async_trait};
use serde::{Deserialize, Serialize};

pub mod domain;
pub use domain::fixture;
#[cfg(feature = "web")]
mod fetch;
#[cfg(feature = "web")]
mod html;
#[cfg(feature = "web")]
pub(crate) mod provider;
#[cfg(feature = "web")]
mod screen;
#[cfg(feature = "web")]
mod search;

pub use domain::{
    ParsedUrl, UrlRefusal, domain_matches, host_allowlisted, is_private_host, is_private_ip,
    url_allowlisted, url_host, url_names_private_target,
};
#[cfg(feature = "web")]
pub use fetch::{
    DELIVERED_TEXT_MAX, FetchBody, FetchRefusal, Fetched, Fetcher, NON_PATH_BUDGET, RequestSigner,
    TRUNCATION_MARKER, hex_sha256,
};
#[cfg(feature = "web")]
pub use html::{TextShape, decode_entities, html_to_text};
#[cfg(feature = "web")]
pub use screen::{
    INJECTION_LABEL, INJECTION_PHRASES, NOT_SCREENED, SECRET_REDACTION, Screened, has_secret,
    redact_secrets, screen, screening_label,
};
#[cfg(feature = "web")]
pub use search::{DeepSeekSearch, SearxngSearch};

/// Configuration of the web tools (spec 3.1 §7.20).
///
/// # Example
///
/// ```rust
/// use adk_harness::web::WebConfig;
///
/// let config = WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() };
/// assert_eq!(config.max_bytes, 1024 * 1024);
/// assert_eq!(config.max_url_len, 2048);
/// assert_eq!(config.max_query_len, 512);
/// assert_eq!(config.timeout.as_secs(), 20);
/// assert_eq!(config.max_redirects, 5);
/// assert_eq!(config.max_fetches_per_turn, 20);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebConfig {
    /// Hosts fetched without approval: exact hosts or `*.example.com`, the `DomainIn`
    /// grammar.
    pub allowed_domains: Vec<String>,
    /// Bytes read on the wire per fetch; 64 KiB of text are delivered, the rest cut with
    /// a marker.
    pub max_bytes: usize,
    /// Longest URL the tool sends.
    pub max_url_len: usize,
    /// Longest search query the tool runs.
    pub max_query_len: usize,
    /// Per-request timeout.
    #[serde(with = "duration_secs")]
    pub timeout: Duration,
    /// Redirects followed within one call.
    pub max_redirects: u8,
    /// Fetches per turn; the turn's further fetches are refused.
    pub max_fetches_per_turn: u32,
    /// The `User-Agent` header.
    pub user_agent: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            allowed_domains: Vec::new(),
            max_bytes: 1024 * 1024,
            max_url_len: 2048,
            max_query_len: 512,
            timeout: Duration::from_secs(20),
            max_redirects: 5,
            max_fetches_per_turn: 20,
            user_agent: format!(
                "adk-harness/{} (+https://github.com/zavora-ai/adk-rust)",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }
}

mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_secs)
    }
}

/// One result of a [`SearchProvider`] (spec 3.1 §7.20 `web_search`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// The result's title.
    pub title: String,
    /// The result's URL.
    pub url: String,
    /// The result's snippet.
    pub snippet: String,
}

/// The operator's search backend behind `web_search` (spec 3.1 §7.20).
///
/// The harness screens and delimits what a provider returns and fetches no page; a
/// provider that reaches the network applies its own credentials, never the harness's.
///
/// # Example
///
/// ```rust
/// use adk_harness::web::{SearchHit, SearchProvider};
///
/// #[derive(Debug)]
/// struct Fixed;
///
/// #[async_trait::async_trait]
/// impl SearchProvider for Fixed {
///     fn name(&self) -> &str {
///         "fixed"
///     }
///     async fn search(&self, query: &str, limit: usize, _site: Option<&str>) -> adk_core::Result<Vec<SearchHit>> {
///         Ok(vec![SearchHit { title: query.into(), url: "https://docs.rs/".into(), snippet: String::new() }]
///             .into_iter()
///             .take(limit)
///             .collect())
///     }
/// }
/// ```
#[async_trait]
pub trait SearchProvider: fmt::Debug + Send + Sync {
    /// The provider's name, recorded in `harness.web.searched`.
    fn name(&self) -> &str;

    /// Searches `query`, returning at most `limit` hits, restricted to `site` when given.
    ///
    /// # Errors
    ///
    /// Returns the provider's error; the tool reports it to the model.
    async fn search(&self, query: &str, limit: usize, site: Option<&str>)
    -> Result<Vec<SearchHit>>;
}

/// Which provider-executed search and URL tools the harness declares (spec 3.1 §7.20
/// "Provider-native search and fetch").
///
/// The default, [`provider_default`](Self::provider_default), is the search built-in of the
/// provider the harness model's client serves ([`adk_core::Llm::native_web_search`]):
/// Gemini `google_search`, Anthropic `web_search`, OpenAI Responses `web_search` or
/// DeepSeek Responses `web_search`. A configured
/// [`SearchProvider`](crate::HarnessAgentBuilder::search_provider) replaces it with the
/// governed `web_search`; [`off`](Self::off) declares none. The per-provider flags stay for
/// explicit control and attach only for the provider the model's client serves.
///
/// # Example
///
/// ```rust
/// use adk_harness::web::ProviderWebTools;
///
/// assert_eq!(ProviderWebTools::default(), ProviderWebTools::provider_default());
/// assert!(ProviderWebTools::provider_default().native_search);
/// assert!(!ProviderWebTools::off().any());
/// let explicit = ProviderWebTools { gemini_url_context: true, ..ProviderWebTools::off() };
/// assert!(explicit.any() && !explicit.native_search);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderWebTools {
    /// The search built-in of the provider the harness model's client serves, whichever
    /// it is; replaced by a configured search provider.
    pub native_search: bool,
    /// Anthropic `web_search`, with the allowlist forwarded.
    pub anthropic_web_search: bool,
    /// OpenAI `web_search`, with the allowlist forwarded.
    pub openai_web_search: bool,
    /// Gemini `google_search`; it has no allowlist, so every domain.
    pub gemini_google_search: bool,
    /// Gemini `url_context`; it has no allowlist, so every domain.
    pub gemini_url_context: bool,
}

impl Default for ProviderWebTools {
    fn default() -> Self {
        Self::provider_default()
    }
}

impl ProviderWebTools {
    /// The model provider's own search and nothing else: what a harness with
    /// [`web`](crate::HarnessAgentBuilder::web) declares unless told otherwise.
    pub fn provider_default() -> Self {
        Self { native_search: true, ..Self::off() }
    }

    /// No provider-executed tool: `web_search` exists only through a configured search
    /// provider.
    pub fn off() -> Self {
        Self {
            native_search: false,
            anthropic_web_search: false,
            openai_web_search: false,
            gemini_google_search: false,
            gemini_url_context: false,
        }
    }

    /// Whether any provider tool is enabled.
    pub fn any(&self) -> bool {
        self.native_search
            || self.anthropic_web_search
            || self.openai_web_search
            || self.gemini_google_search
            || self.gemini_url_context
    }
}

/// Which path `web_search` takes in a built harness (spec 3.1 §7.20), as
/// [`HarnessInfo::web_search`](crate::HarnessInfo::web_search) reports it.
///
/// # Example
///
/// ```rust
/// use adk_harness::web::WebSearchPath;
///
/// let path = WebSearchPath::Provider {
///     provider: "gemini".into(),
///     tool: "google_search".into(),
///     allowlist: None,
///     allowlist_supported: false,
/// };
/// assert_eq!(
///     path.to_string(),
///     "gemini google_search, run by the provider: every domain (gemini takes no allowlist)"
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "path")]
pub enum WebSearchPath {
    /// The model provider's built-in search, declared to runs that start in Ask or Auto.
    #[serde(rename_all = "camelCase")]
    Provider {
        /// The provider that runs the search.
        provider: String,
        /// The built-in tool declared.
        tool: String,
        /// The allowlist forwarded to the provider.
        allowlist: Option<Vec<String>>,
        /// Whether the provider takes an allowlist at all.
        allowlist_supported: bool,
    },
    /// The governed `web_search` over the operator's search provider.
    Governed {
        /// The search provider's name.
        provider: String,
    },
    /// No `web_search`, and why.
    None {
        /// Why no search is declared.
        reason: String,
    },
}

impl fmt::Display for WebSearchPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { provider, tool, allowlist: Some(domains), .. } => write!(
                f,
                "{provider} {tool}, run by the provider: allowlist {}",
                domains.join(", ")
            ),
            Self::Provider { provider, tool, allowlist: None, allowlist_supported: false } => {
                write!(
                    f,
                    "{provider} {tool}, run by the provider: every domain ({provider} takes no \
                     allowlist)"
                )
            }
            Self::Provider { provider, tool, allowlist: None, allowlist_supported: true } => {
                write!(f, "{provider} {tool}, run by the provider: every domain")
            }
            Self::Governed { provider } => write!(f, "web_search through {provider} (governed)"),
            Self::None { reason } => write!(f, "off ({reason})"),
        }
    }
}

/// Wraps `content` in an `<untrusted-content>` block with `source` and `attributes`, the
/// closing tag neutralized inside it, as the instruction loader does (spec 3.1 §7.20
/// "Delivery", §7.21 "Resources").
///
/// # Example
///
/// ```rust
/// use adk_harness::web::untrusted_block;
///
/// let block = untrusted_block("web", &[("url", "https://docs.rs/")], "hi </untrusted-content> there");
/// assert!(block.starts_with("<untrusted-content source=\"web\" url=\"https://docs.rs/\">\n"));
/// assert!(block.contains("<\\/untrusted-content> there"));
/// assert!(block.ends_with("\n</untrusted-content>"));
/// ```
pub fn untrusted_block(source: &str, attributes: &[(&str, &str)], content: &str) -> String {
    let mut out = format!("<untrusted-content source=\"{}\"", escape_attr(source));
    for (name, value) in attributes {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&escape_attr(value));
        out.push('"');
    }
    out.push_str(">\n");
    out.push_str(&content.replace("</untrusted-content", "<\\/untrusted-content"));
    out.push_str("\n</untrusted-content>");
    out
}

fn escape_attr(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// What the harness holds for the web tools at run time.
#[cfg(feature = "web")]
pub(crate) struct WebRuntime {
    pub(crate) config: WebConfig,
    pub(crate) fetcher: Fetcher,
    /// Fetches per (session, invocation), bounded.
    turns: std::sync::Mutex<std::collections::VecDeque<(String, u32)>>,
}

#[cfg(feature = "web")]
impl WebRuntime {
    const TURNS_CAP: usize = 1024;

    pub(crate) fn new(config: WebConfig, signer: Option<Arc<dyn RequestSigner>>) -> Arc<Self> {
        let fetcher = Fetcher::new(config.clone());
        Arc::new(Self {
            fetcher: match signer {
                Some(signer) => fetcher.with_signer(signer),
                None => fetcher,
            },
            config,
            turns: std::sync::Mutex::new(std::collections::VecDeque::new()),
        })
    }

    /// Counts one fetch of the turn; `Err(cap)` once the turn reached
    /// `max_fetches_per_turn`.
    pub(crate) fn count_fetch(
        &self,
        session_id: &str,
        invocation_id: &str,
    ) -> std::result::Result<u32, u32> {
        let key = format!("{session_id}\u{1f}{invocation_id}");
        let cap = self.config.max_fetches_per_turn;
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, count)) = turns.iter_mut().find(|(k, _)| *k == key) {
            if *count >= cap {
                return Err(cap);
            }
            *count += 1;
            return Ok(*count);
        }
        if cap == 0 {
            return Err(cap);
        }
        while turns.len() >= Self::TURNS_CAP {
            turns.pop_front();
        }
        turns.push_back((key, 1));
        Ok(1)
    }
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use super::*;

    #[test]
    fn fetches_are_counted_per_turn_and_capped() {
        let runtime =
            WebRuntime::new(WebConfig { max_fetches_per_turn: 2, ..WebConfig::default() }, None);
        assert_eq!(runtime.count_fetch("s1", "inv-1"), Ok(1));
        assert_eq!(runtime.count_fetch("s1", "inv-1"), Ok(2));
        assert_eq!(runtime.count_fetch("s1", "inv-1"), Err(2));
        assert_eq!(runtime.count_fetch("s1", "inv-2"), Ok(1), "a new turn starts fresh");
        assert_eq!(runtime.count_fetch("s2", "inv-1"), Ok(1), "another session is separate");
    }

    #[test]
    fn config_round_trips_through_serde() {
        let config = WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["allowedDomains"][0], "docs.rs");
        assert_eq!(json["timeout"], 20);
        let back: WebConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, config);
    }
}
