//! External content under governance (spec 3.1 §7.20).
//!
//! `web_fetch` and `web_search` are governed tools of class `Web { allowlisted }`: the
//! policy decides them by the URL's host against [`WebConfig::allowed_domains`], and
//! everything they return reaches the model only inside a delimited untrusted block,
//! size-capped, with injection matches labelled and secret patterns redacted
//! (spec 3.1 §9.2 I46–I49). The fetch allowlist is never a search filter: what a search
//! covers is [`WebConfig::search_domains`] (spec 3.1 §7.32, I73).
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

/// Configuration of the web tools (spec 3.1 §7.20, §7.32).
///
/// | Setting | Meaning |
/// |---|---|
/// | `allowed_domains` | hosts `web_fetch` and the browser pack's navigation reach without approval; never forwarded to a search |
/// | `search_domains` | the search filter: forwarded to the provider's search where it takes one, applied to the governed `web_search`'s results; `None` searches every domain |
///
/// # Example
///
/// ```rust
/// use adk_harness::web::{SearchDomains, WebConfig};
///
/// let config = WebConfig {
///     allowed_domains: vec!["docs.rs".into()],
///     search_domains: Some(SearchDomains::Allowed(vec!["docs.rs".into(), "rust-lang.org".into()])),
///     ..WebConfig::default()
/// };
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
    /// Hosts `web_fetch` and the browser pack reach without approval: exact hosts or
    /// `*.example.com`, the `DomainIn` grammar. Never forwarded to a search (I73).
    pub allowed_domains: Vec<String>,
    /// The search filter (spec 3.1 §7.32): forwarded where the provider's search takes one
    /// (Anthropic both lists, OpenAI an allowed list), applied to the governed
    /// `web_search`'s results, and named in the banner where the provider takes none
    /// (Gemini, DeepSeek). `None` searches every domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_domains: Option<SearchDomains>,
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
            search_domains: None,
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

/// What a web search may return, by domain (spec 3.1 §7.32): a filter separate from the
/// fetch allowlist.
///
/// Each entry is a domain such as `docs.rs`; it covers the domain and its subdomains, as
/// Anthropic's and OpenAI's search filters read it, and a leading `*.` is dropped. A
/// workspace file may only [`narrow`](Self::narrow) the filter the operator or user set.
///
/// # Example
///
/// ```rust
/// use adk_harness::web::SearchDomains;
///
/// let only = SearchDomains::Allowed(vec!["rust-lang.org".into()]);
/// assert!(only.permits("doc.rust-lang.org"));
/// assert!(!only.permits("evil-rust-lang.org"));
/// assert_eq!(only.to_string(), "only rust-lang.org");
///
/// let blocked = SearchDomains::Blocked(vec!["pinterest.com".into()]);
/// assert!(!blocked.permits("www.pinterest.com"));
/// assert_eq!(blocked.to_string(), "every domain except pinterest.com");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchDomains {
    /// Only these domains and their subdomains.
    Allowed(Vec<String>),
    /// Every domain but these and their subdomains.
    Blocked(Vec<String>),
}

/// `domain` lowercased, without a leading `*.` or a trailing dot.
fn search_domain(domain: &str) -> String {
    let domain = domain.trim().to_ascii_lowercase();
    let domain = domain.strip_prefix("*.").unwrap_or(&domain);
    domain.strip_suffix('.').unwrap_or(domain).to_string()
}

/// Whether `host` is `domain` or one of its subdomains.
fn covered(host: &str, domain: &str) -> bool {
    let (host, domain) = (search_domain(host), search_domain(domain));
    !domain.is_empty()
        && (host == domain || host.strip_suffix(&domain).is_some_and(|sub| sub.ends_with('.')))
}

impl SearchDomains {
    /// The domains the filter lists.
    pub fn domains(&self) -> &[String] {
        match self {
            Self::Allowed(domains) | Self::Blocked(domains) => domains,
        }
    }

    /// Whether a result from `host` passes the filter.
    pub fn permits(&self, host: &str) -> bool {
        match self {
            Self::Allowed(domains) => domains.iter().any(|domain| covered(host, domain)),
            Self::Blocked(domains) => !domains.iter().any(|domain| covered(host, domain)),
        }
    }

    /// Whether the filter lets no domain through: an empty allowed list.
    pub fn permits_none(&self) -> bool {
        matches!(self, Self::Allowed(domains) if domains.is_empty())
    }

    /// The filter `ceiling` (the operator's or user's, `None` for every domain) narrowed by
    /// `workspace`, a workspace file's filter, which may only narrow it (spec 3.1 §7.32,
    /// I3).
    ///
    /// | Ceiling | Workspace | Result |
    /// |---|---|---|
    /// | none | any | the workspace's |
    /// | allowed `a` | allowed `b` | allowed: the entries of each the other covers |
    /// | allowed `a` | blocked `b` | allowed: `a` without the entries that overlap `b` |
    /// | blocked `a` | allowed `b` | allowed: `b` without the entries that overlap `a` |
    /// | blocked `a` | blocked `b` | blocked: both |
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::web::SearchDomains;
    ///
    /// let user = SearchDomains::Allowed(vec!["rust-lang.org".into(), "docs.rs".into()]);
    /// let workspace = SearchDomains::Allowed(vec!["doc.rust-lang.org".into(), "evil.example".into()]);
    /// assert_eq!(
    ///     SearchDomains::narrow(Some(&user), &workspace),
    ///     SearchDomains::Allowed(vec!["doc.rust-lang.org".into()])
    /// );
    /// ```
    pub fn narrow(ceiling: Option<&Self>, workspace: &Self) -> Self {
        let overlaps = |a: &str, b: &str| covered(a, b) || covered(b, a);
        let unique = |domains: Vec<String>| {
            let mut out: Vec<String> = Vec::new();
            for domain in domains {
                if !out.contains(&domain) {
                    out.push(domain);
                }
            }
            out
        };
        match (ceiling, workspace) {
            (None, workspace) => workspace.clone(),
            (Some(Self::Allowed(a)), Self::Allowed(b)) => {
                let within = |x: &String, of: &[String]| of.iter().any(|y| covered(x, y));
                let both =
                    b.iter().filter(|x| within(x, a)).chain(a.iter().filter(|x| within(x, b)));
                Self::Allowed(unique(both.cloned().collect()))
            }
            (Some(Self::Allowed(allowed)), Self::Blocked(blocked))
            | (Some(Self::Blocked(blocked)), Self::Allowed(allowed)) => Self::Allowed(unique(
                allowed
                    .iter()
                    .filter(|x| !blocked.iter().any(|y| overlaps(x, y)))
                    .cloned()
                    .collect(),
            )),
            (Some(Self::Blocked(a)), Self::Blocked(b)) => {
                Self::Blocked(unique(a.iter().chain(b).cloned().collect()))
            }
        }
    }
}

impl fmt::Display for SearchDomains {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allowed(domains) if domains.is_empty() => f.write_str("no domain"),
            Self::Allowed(domains) => write!(f, "only {}", domains.join(", ")),
            Self::Blocked(domains) if domains.is_empty() => f.write_str("every domain"),
            Self::Blocked(domains) => write!(f, "every domain except {}", domains.join(", ")),
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
    /// Anthropic `web_search`, with the search filter forwarded.
    pub anthropic_web_search: bool,
    /// OpenAI `web_search`, with an allowed search filter forwarded.
    pub openai_web_search: bool,
    /// Gemini `google_search`; it takes no search filter, so every domain.
    pub gemini_google_search: bool,
    /// Gemini `url_context`; it reads the URLs the prompt names.
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
/// use adk_harness::web::SearchDomains;
///
/// let path = WebSearchPath::Provider {
///     provider: "anthropic".into(),
///     tool: "web_search".into(),
///     filter: Some(SearchDomains::Allowed(vec!["docs.rs".into()])),
///     filter_applied: true,
/// };
/// assert_eq!(path.to_string(), "anthropic web_search, run by the provider: only docs.rs");
///
/// let path = WebSearchPath::Provider {
///     provider: "gemini".into(),
///     tool: "google_search".into(),
///     filter: Some(SearchDomains::Allowed(vec!["docs.rs".into()])),
///     filter_applied: false,
/// };
/// assert_eq!(
///     path.to_string(),
///     "gemini google_search, run by the provider: every domain (gemini takes no search \
///      filter; search_domains is not applied)"
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
        /// The configured search filter ([`WebConfig::search_domains`]); never the fetch
        /// allowlist.
        filter: Option<SearchDomains>,
        /// Whether the provider takes the filter, so it was forwarded.
        filter_applied: bool,
    },
    /// The governed `web_search` over the operator's search provider.
    Governed {
        /// The search provider's name.
        provider: String,
        /// The search filter applied to its results.
        filter: Option<SearchDomains>,
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
            Self::Provider { provider, tool, filter: Some(filter), filter_applied: true } => {
                write!(f, "{provider} {tool}, run by the provider: {filter}")
            }
            Self::Provider { provider, tool, filter: Some(_), filter_applied: false } => write!(
                f,
                "{provider} {tool}, run by the provider: every domain ({provider} takes no search \
                 filter; search_domains is not applied)"
            ),
            Self::Provider { provider, tool, filter: None, .. } => {
                write!(f, "{provider} {tool}, run by the provider: every domain")
            }
            Self::Governed { provider, filter: Some(filter) } => {
                write!(f, "web_search through {provider} (governed): {filter}")
            }
            Self::Governed { provider, filter: None } => {
                write!(f, "web_search through {provider} (governed): every domain")
            }
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
    fn search_domains_cover_subdomains_and_never_lookalikes() {
        let only = SearchDomains::Allowed(vec!["*.Rust-Lang.org".into(), "docs.rs".into()]);
        assert!(only.permits("rust-lang.org") && only.permits("doc.rust-lang.org"));
        assert!(only.permits("docs.rs") && !only.permits("evil-docs.rs"));
        assert!(!only.permits("example.com"));
        let blocked = SearchDomains::Blocked(vec!["pinterest.com".into()]);
        assert!(!blocked.permits("pinterest.com") && blocked.permits("example.com"));
        assert!(SearchDomains::Allowed(vec![]).permits_none());
        assert!(!SearchDomains::Blocked(vec![]).permits_none());
    }

    #[test]
    fn a_workspace_filter_only_narrows_the_ceiling() {
        let allowed =
            |d: &[&str]| SearchDomains::Allowed(d.iter().map(|d| d.to_string()).collect());
        let blocked =
            |d: &[&str]| SearchDomains::Blocked(d.iter().map(|d| d.to_string()).collect());
        // No ceiling: the workspace's filter.
        assert_eq!(SearchDomains::narrow(None, &blocked(&["a.io"])), blocked(&["a.io"]));
        // Allowed and allowed: what both cover.
        assert_eq!(
            SearchDomains::narrow(Some(&allowed(&["docs.rs"])), &allowed(&["evil.example"])),
            allowed(&[])
        );
        assert_eq!(
            SearchDomains::narrow(
                Some(&allowed(&["doc.rust-lang.org"])),
                &allowed(&["rust-lang.org"])
            ),
            allowed(&["doc.rust-lang.org"])
        );
        // Allowed and blocked: the allowed list without what the blocked list touches.
        assert_eq!(
            SearchDomains::narrow(
                Some(&allowed(&["docs.rs", "rust-lang.org"])),
                &blocked(&["blog.rust-lang.org"])
            ),
            allowed(&["docs.rs"])
        );
        assert_eq!(
            SearchDomains::narrow(
                Some(&blocked(&["pinterest.com"])),
                &allowed(&["docs.rs", "pinterest.com"])
            ),
            allowed(&["docs.rs"])
        );
        // Blocked and blocked: both lists.
        assert_eq!(
            SearchDomains::narrow(Some(&blocked(&["a.io"])), &blocked(&["b.io", "a.io"])),
            blocked(&["a.io", "b.io"])
        );
    }

    #[test]
    fn the_search_path_names_the_filter_never_the_fetch_allowlist() {
        let path = |filter, filter_applied| WebSearchPath::Provider {
            provider: "anthropic".into(),
            tool: "web_search".into(),
            filter,
            filter_applied,
        };
        assert_eq!(
            path(None, true).to_string(),
            "anthropic web_search, run by the provider: every domain"
        );
        assert_eq!(
            path(Some(SearchDomains::Blocked(vec!["pinterest.com".into()])), true).to_string(),
            "anthropic web_search, run by the provider: every domain except pinterest.com"
        );
        let governed = WebSearchPath::Governed {
            provider: "searxng".into(),
            filter: Some(SearchDomains::Allowed(vec!["docs.rs".into()])),
        };
        assert_eq!(governed.to_string(), "web_search through searxng (governed): only docs.rs");
    }

    #[test]
    fn config_round_trips_through_serde() {
        let config = WebConfig {
            allowed_domains: vec!["docs.rs".into()],
            search_domains: Some(SearchDomains::Blocked(vec!["pinterest.com".into()])),
            ..WebConfig::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["allowedDomains"][0], "docs.rs");
        assert_eq!(json["searchDomains"]["blocked"][0], "pinterest.com");
        assert_eq!(json["timeout"], 20);
        let back: WebConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, config);
    }
}
