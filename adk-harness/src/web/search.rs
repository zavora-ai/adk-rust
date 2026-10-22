//! Search providers behind the governed `web_search` (spec 3.1 §7.20, §13 question 7).
//!
//! An operator who wants search to run on the governed path — policy, screening and a
//! per-call record — configures one of these instead of the model provider's built-in
//! search: a self-hosted SearXNG, or DeepSeek's Search API keyed from `DEEPSEEK_API_KEY`.

use std::fmt;
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, async_trait};
use serde_json::{Value, json};

use super::{SearchHit, SearchProvider};

/// Per-request bound of a search provider's call.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);

fn unavailable(provider: &str, message: impl fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "harness.web.search_provider",
        format!("{provider} search failed: {message}"),
    )
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(SEARCH_TIMEOUT)
        .connect_timeout(SEARCH_TIMEOUT)
        .user_agent(super::WebConfig::default().user_agent)
        .build()
        .map_err(|error| AdkError::config(format!("search client: {error}")))
}

/// The query with `site:` appended when the call restricts results to one host; both
/// SearXNG and DeepSeek read the operator.
fn site_query(query: &str, site: Option<&str>) -> String {
    match site {
        Some(site) => format!("{query} site:{site}"),
        None => query.to_string(),
    }
}

async fn read_json(provider: &str, response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = response.bytes().await.map_err(|error| unavailable(provider, error))?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value.pointer("/error/message").and_then(Value::as_str).map(str::to_string)
            })
            .unwrap_or_default();
        return Err(unavailable(provider, format!("HTTP {status} {detail}").trim_end()));
    }
    serde_json::from_slice(&body).map_err(|error| unavailable(provider, error))
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// A [`SearchProvider`] over a SearXNG instance's JSON API (spec 3.1 §7.20).
///
/// Sends `GET <base>/search?q=…&format=json`; the instance must list `json` under
/// `search.formats` in its `settings.yml`, or it answers 403. No key is sent.
///
/// # Example
///
/// ```rust
/// use adk_harness::web::{SearchProvider, SearxngSearch};
///
/// let search = SearxngSearch::new("http://localhost:8888").unwrap();
/// assert_eq!(search.name(), "searxng");
/// assert!(SearxngSearch::new("localhost:8888").is_err(), "the URL needs a scheme");
/// ```
pub struct SearxngSearch {
    base: reqwest::Url,
    client: reqwest::Client,
}

impl SearxngSearch {
    /// A provider over the SearXNG instance at `base_url`, such as
    /// `https://search.example.org`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when `base_url` is not an `http` or `https` URL.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let raw = base_url.as_ref().trim();
        let base = reqwest::Url::parse(raw)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            .ok_or_else(|| {
                AdkError::config(format!(
                    "'{raw}' is not a SearXNG URL; give its http(s) base, such as \
                     https://search.example.org"
                ))
            })?;
        Ok(Self { base, client: client()? })
    }
}

impl fmt::Debug for SearxngSearch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearxngSearch").field("base", &self.base.as_str()).finish()
    }
}

#[async_trait]
impl SearchProvider for SearxngSearch {
    fn name(&self) -> &str {
        "searxng"
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
        site: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| unavailable("searxng", "the base URL cannot take a path"))?
            .pop_if_empty()
            .push("search");
        url.query_pairs_mut()
            .append_pair("q", &site_query(query, site))
            .append_pair("format", "json");
        let response =
            self.client.get(url).send().await.map_err(|error| unavailable("searxng", error))?;
        let body = read_json("searxng", response).await?;
        Ok(body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|hit| SearchHit {
                title: text(hit, "title"),
                url: text(hit, "url"),
                snippet: text(hit, "content"),
            })
            .filter(|hit| !hit.url.is_empty())
            .take(limit)
            .collect())
    }
}

/// A [`SearchProvider`] over DeepSeek's Search API (spec 3.1 §7.20):
/// `POST https://api.deepseek.com/v1/tools/search` with the account's API key
/// (<https://api-docs.deepseek.com/guides/search>).
///
/// Each hit's snippet is DeepSeek's `summary` when it sent one, else its `snippet`.
/// DeepSeek caps a query at 128 characters and bills each search that returns results.
///
/// # Example
///
/// ```rust
/// use adk_harness::web::{DeepSeekSearch, SearchProvider};
///
/// let search = DeepSeekSearch::new("sk-example").unwrap();
/// assert_eq!(search.name(), "deepseek");
/// assert!(!format!("{search:?}").contains("sk-example"), "the key is never printed");
/// ```
pub struct DeepSeekSearch {
    api_key: String,
    endpoint: String,
    client: reqwest::Client,
}

impl DeepSeekSearch {
    /// DeepSeek's endpoint.
    pub const ENDPOINT: &'static str = "https://api.deepseek.com/v1/tools/search";

    /// A provider calling DeepSeek's Search API with `api_key`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the key is empty or the client cannot be built.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(AdkError::config(
                "DeepSeek search needs an API key; set DEEPSEEK_API_KEY or pass one",
            ));
        }
        Ok(Self { api_key, endpoint: Self::ENDPOINT.to_string(), client: client()? })
    }

    /// A provider keyed from `DEEPSEEK_API_KEY`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the variable is unset or empty.
    pub fn from_env() -> Result<Self> {
        Self::new(std::env::var("DEEPSEEK_API_KEY").unwrap_or_default())
    }

    /// Sends searches to `endpoint` instead of DeepSeek's, for a gateway or a test server.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }
}

impl fmt::Debug for DeepSeekSearch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeepSeekSearch")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

#[async_trait]
impl SearchProvider for DeepSeekSearch {
    fn name(&self) -> &str {
        "deepseek"
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
        site: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&json!({ "query": site_query(query, site), "count": limit.clamp(1, 50) }))
            .send()
            .await
            .map_err(|error| unavailable("deepseek", error))?;
        let body = read_json("deepseek", response).await?;
        Ok(body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|hit| {
                let summary = text(hit, "summary");
                SearchHit {
                    title: text(hit, "title"),
                    url: text(hit, "url"),
                    snippet: if summary.is_empty() { text(hit, "snippet") } else { summary },
                }
            })
            .filter(|hit| !hit.url.is_empty())
            .take(limit)
            .collect())
    }
}
