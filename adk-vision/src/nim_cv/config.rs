//! [`NimCvConfig`]: where the NVIDIA CV endpoints are and how they are called.

use std::collections::BTreeMap;
use std::time::Duration;

/// The default client timeout. NVIDIA's gateway gives up after 120 s with a 504, so the
/// client waits longer than that to see the gateway's answer.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// The default client-side rate: 40 requests per minute, the free-tier figure NVIDIA's
/// forum users report for the hosted endpoints.
pub const DEFAULT_REQUESTS_PER_MINUTE: u32 = 40;

/// The default ceiling for one inline image: the asset upload path is retired, so every
/// image travels as a data URI in the request body.
pub const DEFAULT_MAX_INLINE_BYTES: usize = 8 * 1024 * 1024;

/// Where NVIDIA's CV endpoints are and how the client calls them.
///
/// Nothing ships a default URL (spec 3.7 §7.7): the caller names the hosted endpoint, under
/// NVIDIA's API Trial terms and suited to prototypes, or a self-hosted NIM. Each model is
/// called at `<base_url>/<org>/<model>`; a self-hosted NIM that serves one model at its own
/// URL is named with [`with_model_url`](Self::with_model_url).
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use adk_vision::NimCvConfig;
///
/// let config = NimCvConfig::new("https://ai.api.nvidia.com/v1/cv")
///     .with_api_key("nvapi-...")
///     .with_model_url("nvidia/nemotron-ocr-v1", "http://ocr.internal:8000/v1/infer")
///     .with_requests_per_minute(40);
/// assert_eq!(config.url_for("nvidia/nemotron-page-elements-v3"),
///     "https://ai.api.nvidia.com/v1/cv/nvidia/nemotron-page-elements-v3");
/// assert_eq!(config.url_for("nvidia/nemotron-ocr-v1"), "http://ocr.internal:8000/v1/infer");
/// assert_eq!(config.timeout(), Duration::from_secs(180));
/// // The key never appears in debug output.
/// assert!(!format!("{config:?}").contains("nvapi"));
/// ```
#[derive(Clone)]
pub struct NimCvConfig {
    base_url: String,
    api_key: Option<String>,
    timeout: Duration,
    requests_per_minute: u32,
    max_inline_bytes: usize,
    model_urls: BTreeMap<String, String>,
    pub(crate) http: Option<reqwest::Client>,
}

impl std::fmt::Debug for NimCvConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NimCvConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("requests_per_minute", &self.requests_per_minute)
            .field("max_inline_bytes", &self.max_inline_bytes)
            .field("model_urls", &self.model_urls)
            .finish_non_exhaustive()
    }
}

impl NimCvConfig {
    /// Endpoints under `base_url`, such as `https://ai.api.nvidia.com/v1/cv`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            timeout: DEFAULT_TIMEOUT,
            requests_per_minute: DEFAULT_REQUESTS_PER_MINUTE,
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            model_urls: BTreeMap::new(),
            http: None,
        }
    }

    /// Sends `Authorization: Bearer <key>`; a self-hosted NIM may need none.
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Reads the key from `NVIDIA_API_KEY` when it is set and not empty.
    pub fn with_api_key_from_env(mut self) -> Self {
        if let Some(key) = std::env::var("NVIDIA_API_KEY").ok().filter(|k| !k.trim().is_empty()) {
            self.api_key = Some(key);
        }
        self
    }

    /// Sets the client timeout; keep it above the gateway's 120 s.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the client-side rate limit, in requests per minute (at least one).
    pub fn with_requests_per_minute(mut self, rpm: u32) -> Self {
        self.requests_per_minute = rpm.max(1);
        self
    }

    /// Sets the largest image, in encoded bytes, sent inline.
    pub fn with_max_inline_bytes(mut self, bytes: usize) -> Self {
        self.max_inline_bytes = bytes;
        self
    }

    /// Calls `model` (`org/name`) at `url` instead of under the base URL.
    pub fn with_model_url(mut self, model: impl Into<String>, url: impl Into<String>) -> Self {
        self.model_urls.insert(model.into(), url.into());
        self
    }

    /// Uses `client` for every request: a proxy, custom roots, or a test resolver. Its own
    /// timeout applies instead of [`with_timeout`](Self::with_timeout)'s.
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// The URL `model` is called at.
    pub fn url_for(&self, model: &str) -> String {
        self.model_urls
            .get(model)
            .cloned()
            .unwrap_or_else(|| format!("{}/{model}", self.base_url.trim_end_matches('/')))
    }

    /// The client timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The client-side rate limit.
    pub fn requests_per_minute(&self) -> u32 {
        self.requests_per_minute
    }

    /// The inline image ceiling in bytes.
    pub fn max_inline_bytes(&self) -> usize {
        self.max_inline_bytes
    }

    pub(crate) fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }
}
