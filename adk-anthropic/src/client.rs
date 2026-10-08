use std::env;
use std::fs;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client as ReqwestClient, Response, header};
use serde::Deserialize;
use tokio::time::sleep;

use crate::AccumulatingStream;
use crate::backoff::ExponentialBackoff;
use crate::base_url::{InsecureHttp, validate_base_url};
use crate::client_logger::ClientLogger;
use crate::error::{Error, Result};
use crate::observability::{
    CLIENT_REQUEST_DURATION, CLIENT_REQUEST_ERRORS, CLIENT_REQUEST_RETRIES, CLIENT_REQUESTS,
    CLIENT_RETRY_BACKOFF,
};
use crate::sse::{process_json_sse, process_sse};
use crate::types::{
    BatchRequest, BatchResultItem, FileObject, Message, MessageBatch, MessageCountTokensParams,
    MessageCreateParams, MessageStreamEvent, MessageTokensCount, ModelInfo, ModelListParams,
    ModelListResponse, PaginatedList, ServerFallbackMessage, ServerFallbackRequest,
    ServerFallbackStreamEvent, SkillObject, ThinkingConfig,
};

use base64::Engine as _;

/// Simple base64 encoding for skill content.
fn base64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// A stream wrapper that logs events and the final message through a [`ClientLogger`].
///
/// This stream passes through all events from the underlying [`AccumulatingStream`],
/// logging each event as it occurs and logging the final reconstructed message
/// when the stream completes.
pub struct LoggingStream<'a> {
    inner: AccumulatingStream,
    logger: &'a dyn ClientLogger,
    receiver: Option<tokio::sync::oneshot::Receiver<Result<Message>>>,
}

impl<'a> LoggingStream<'a> {
    /// Create a new logging stream wrapper.
    fn new(
        inner: AccumulatingStream,
        receiver: tokio::sync::oneshot::Receiver<Result<Message>>,
        logger: &'a dyn ClientLogger,
    ) -> Self {
        Self { inner, logger, receiver: Some(receiver) }
    }
}

impl Stream for LoggingStream<'_> {
    type Item = Result<MessageStreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let inner = Pin::new(&mut self.inner);
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                self.logger.log_stream_event(&event);
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                // Stream ended - try to get the accumulated message
                if let Some(mut receiver) = self.receiver.take()
                    && let Ok(Ok(ref message)) = receiver.try_recv()
                {
                    self.logger.log_stream_message(message);
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

const DEFAULT_API_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const STRUCTURED_OUTPUTS_BETA: &str = "structured-outputs-2025-11-13";
const SERVER_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Opt-in named when a base URL given in code is rejected for plain HTTP.
const CODE_INSECURE_HTTP_OPT_IN: &str =
    "call `allow_insecure_http()` on the client before setting the base URL";
/// Opt-in named when `ANTHROPIC_BASE_URL` is rejected for plain HTTP.
const ENV_INSECURE_HTTP_OPT_IN: &str =
    "set ANTHROPIC_ALLOW_INSECURE_HTTP=1 alongside ANTHROPIC_BASE_URL";

/// Client for the Anthropic API with performance optimizations.
///
/// # Timeouts
///
/// | Request kind | Bound |
/// |--------------|-------|
/// | Non-streaming | [`with_timeout`](Self::with_timeout) covers the whole request (default 60 seconds) |
/// | Streaming, until response headers | the same timeout |
/// | Streaming body | a 30-second inactivity timeout between chunks, plus the optional total bound from [`with_stream_timeout`](Self::with_stream_timeout) |
///
/// The `Debug` output redacts the API key.
#[derive(Clone)]
pub struct Anthropic {
    api_key: String,
    client: ReqwestClient,
    /// Client without a total timeout, so long streams are not cut off mid-body.
    stream_client: ReqwestClient,
    base_url: String,
    timeout: Duration,
    stream_timeout: Option<Duration>,
    max_retries: usize,
    throughput_ops_sec: f64,
    reserve_capacity: f64,
    /// Cached headers for performance - Arc for cheap cloning
    cached_headers: Arc<HeaderMap>,
    /// Whether `with_base_url` accepts `http://` to a non-loopback host.
    allow_insecure_http: bool,
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic")
            .field("api_key", &"[REDACTED]")
            .field("client", &self.client)
            .field("stream_client", &self.stream_client)
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("stream_timeout", &self.stream_timeout)
            .field("max_retries", &self.max_retries)
            .field("throughput_ops_sec", &self.throughput_ops_sec)
            .field("reserve_capacity", &self.reserve_capacity)
            .field("cached_headers", &self.cached_headers)
            .field("allow_insecure_http", &self.allow_insecure_http)
            .finish()
    }
}

impl Anthropic {
    /// Build the HTTP client. `total_timeout` of `None` leaves response bodies
    /// unbounded, which the streaming client relies on.
    fn build_http_client(
        total_timeout: Option<Duration>,
        connect_timeout: Duration,
    ) -> Result<ReqwestClient> {
        let mut builder = ReqwestClient::builder()
            .connect_timeout(connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(10) // Connection pooling optimization
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60));
        if let Some(timeout) = total_timeout {
            builder = builder.timeout(timeout);
        }
        builder.build().map_err(|e| {
            Error::http_client(format!("Failed to build HTTP client: {e}"), Some(Box::new(e)))
        })
    }

    /// Resolve an API key value, handling file:// URLs
    fn resolve_api_key(key_value: &str) -> Result<String> {
        if let Some(stripped) = key_value.strip_prefix("file://") {
            // Handle file:// URLs
            let path = if stripped.starts_with('/') {
                // Absolute path: file:///root/.env -> /root/.env
                stripped.to_string()
            } else {
                // Relative path: file://../foo -> ../foo
                stripped.to_string()
            };

            fs::read_to_string(&path).map(|content| content.trim().to_string()).map_err(|e| {
                Error::validation(
                    format!("Failed to read API key from file '{}': {}", path, e),
                    Some("api_key".to_string()),
                )
            })
        } else {
            // Regular API key value
            Ok(key_value.to_string())
        }
    }

    /// Resolve the effective base URL from optional `ANTHROPIC_BASE_URL` and
    /// `ANTHROPIC_ALLOW_INSECURE_HTTP` values.
    ///
    /// A value supplied through the environment is held to the same rule as one
    /// supplied through [`Anthropic::with_base_url`]: every request attaches the
    /// API key, so an unencrypted endpoint would leak the credential. The
    /// acknowledgement for plain HTTP comes from the same source as the URL, so
    /// an `insecure_http_env` of `1` or `true` (case-insensitive) admits a
    /// non-loopback `http://` value. When no URL is supplied the default
    /// Anthropic API URL is used.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the supplied value is not `https://`, not
    /// `http://` with a loopback host, and not acknowledged `http://`.
    fn resolve_base_url(
        env_value: Option<String>,
        insecure_http_env: Option<String>,
    ) -> Result<String> {
        match env_value {
            Some(value) => {
                let acknowledged = insecure_http_env
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|flag| flag == "1" || flag.eq_ignore_ascii_case("true"));
                let insecure_http = if acknowledged {
                    InsecureHttp::Acknowledged
                } else {
                    InsecureHttp::Rejected { opt_in: Some(ENV_INSECURE_HTTP_OPT_IN) }
                };
                validate_base_url(&value, insecure_http)?;
                Ok(value)
            }
            None => Ok(DEFAULT_API_URL.to_string()),
        }
    }

    /// Create a new Anthropic client.
    ///
    /// The API key can be provided directly or read from the `ANTHROPIC_API_KEY`
    /// environment variable. If the value starts with `"file://"`, it will be
    /// treated as a file path and the API key will be read from that file.
    ///
    /// The base URL is resolved from the `ANTHROPIC_BASE_URL` environment
    /// variable. If not set, the default Anthropic API URL is used.
    ///
    /// `ANTHROPIC_BASE_URL` may name a plain `http://` endpoint on a non-loopback
    /// host, such as an internal gateway, only when `ANTHROPIC_ALLOW_INSECURE_HTTP`
    /// is set to `1` or `true` (case-insensitive). The client then logs a warning
    /// and sends the API key unencrypted. `ANTHROPIC_ALLOW_INSECURE_HTTP` applies
    /// to the environment URL only; a URL given in code needs
    /// [`allow_insecure_http`](Self::allow_insecure_http).
    ///
    /// # Errors
    ///
    /// Returns a validation error when `ANTHROPIC_BASE_URL` is set to an
    /// endpoint that would transmit the API key in cleartext — anything other
    /// than `https://`, or `http://` with a loopback host (`localhost`,
    /// `127.0.0.1`, `[::1]`), unless `ANTHROPIC_ALLOW_INSECURE_HTTP`
    /// acknowledges an `http://` endpoint. A misconfigured environment fails
    /// loudly rather than silently falling back to the default URL.
    pub fn new(api_key: Option<String>) -> Result<Self> {
        let api_key = match api_key {
            Some(key) => Self::resolve_api_key(&key)?,
            None => {
                let env_key = env::var("ANTHROPIC_API_KEY").map_err(|_| {
                    Error::authentication(
                        "API key not provided and ANTHROPIC_API_KEY environment variable not set",
                    )
                })?;
                Self::resolve_api_key(&env_key)?
            }
        };

        let base_url = Self::resolve_base_url(
            env::var("ANTHROPIC_BASE_URL").ok(),
            env::var("ANTHROPIC_ALLOW_INSECURE_HTTP").ok(),
        )?;
        Self::from_values(api_key, base_url)
    }

    /// Create a client with an explicit API key and base URL.
    ///
    /// Unlike [`Anthropic::new`], this constructor reads neither `ANTHROPIC_API_KEY`
    /// nor `ANTHROPIC_BASE_URL`. A `file://` API key is read from that file, as in
    /// [`Anthropic::new`].
    ///
    /// The base URL is validated here, so this constructor cannot take a plain
    /// `http://` URL to a non-loopback host. For one, construct the client with an
    /// `https://` URL, then call [`allow_insecure_http`](Self::allow_insecure_http)
    /// followed by [`with_base_url`](Self::with_base_url).
    ///
    /// # Errors
    ///
    /// Returns a validation error when `base_url` is not `https://` and not
    /// `http://` with a loopback host, or when a `file://` API key cannot be read.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_anthropic::Anthropic;
    ///
    /// let client = Anthropic::new_with_base_url("sk-ant-key", "https://proxy.example.com")?;
    /// # Ok::<(), adk_anthropic::Error>(())
    /// ```
    pub fn new_with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self> {
        let base_url = base_url.into();
        validate_base_url(
            &base_url,
            InsecureHttp::Rejected { opt_in: Some(CODE_INSECURE_HTTP_OPT_IN) },
        )?;
        Self::from_values(Self::resolve_api_key(&api_key.into())?, base_url)
    }

    fn from_values(api_key: String, base_url: String) -> Result<Self> {
        let timeout = DEFAULT_TIMEOUT;
        let client = Self::build_http_client(Some(timeout), timeout)?;
        let stream_client = Self::build_http_client(None, timeout)?;

        // Pre-build headers for performance
        let cached_headers = Arc::new(Self::build_default_headers(&api_key)?);

        Ok(Self {
            api_key,
            client,
            stream_client,
            base_url,
            timeout,
            stream_timeout: None,
            max_retries: 3,
            throughput_ops_sec: 1.0 / 60.0,
            reserve_capacity: 1.0 / 60.0,
            cached_headers,
            allow_insecure_http: false,
        })
    }

    /// Create an Anthropic client authenticated with a bearer token.
    ///
    /// Unlike [`Anthropic::new`], this constructor does not require an API key or
    /// the `ANTHROPIC_API_KEY` environment variable. Requests carry
    /// `Authorization: Bearer <token>` and omit `x-api-key`.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the token is empty or cannot be encoded
    /// as an HTTP header value.
    pub fn new_with_auth_token(auth_token: impl Into<String>) -> Result<Self> {
        Self::new(Some(String::new()))?.with_auth_token(auth_token)
    }

    /// Replace API-key authentication with a bearer token.
    ///
    /// The resulting client omits `x-api-key` from every request.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the token is empty or cannot be encoded
    /// as an HTTP header value.
    pub fn with_auth_token(mut self, auth_token: impl Into<String>) -> Result<Self> {
        let auth_token = auth_token.into();
        if auth_token.trim().is_empty() {
            return Err(Error::validation(
                "Auth token cannot be empty".to_string(),
                Some("auth_token".to_string()),
            ));
        }

        let mut headers = (*self.cached_headers).clone();
        headers.remove("x-api-key");
        let mut value =
            HeaderValue::from_str(&format!("Bearer {auth_token}")).map_err(|error| {
                Error::validation(
                    format!("Invalid auth token format: {error}"),
                    Some("auth_token".to_string()),
                )
            })?;
        value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, value);
        self.api_key.clear();
        self.cached_headers = Arc::new(headers);
        Ok(self)
    }

    /// Override the `anthropic-version` header used by this client.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the version is empty or cannot be
    /// encoded as an HTTP header value.
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Result<Self> {
        let api_version = api_version.into();
        if api_version.trim().is_empty() {
            return Err(Error::validation(
                "API version cannot be empty".to_string(),
                Some("api_version".to_string()),
            ));
        }

        let mut headers = (*self.cached_headers).clone();
        let value = HeaderValue::from_str(&api_version).map_err(|error| {
            Error::validation(
                format!("Invalid API version format: {error}"),
                Some("api_version".to_string()),
            )
        })?;
        headers.insert("anthropic-version", value);
        self.cached_headers = Arc::new(headers);
        Ok(self)
    }

    /// Allow a plain `http://` base URL on a non-loopback host.
    ///
    /// Use this for a trusted internal gateway that is reachable only over plain
    /// HTTP. Every request attaches the API key, which then crosses the network
    /// unencrypted. Loopback `http://` URLs and `https://` URLs need no opt-in.
    ///
    /// [`with_base_url`](Self::with_base_url) and
    /// [`with_base_url_and_timeout`](Self::with_base_url_and_timeout) validate the
    /// URL when they are called, so this method must be called **before** them.
    /// Accepting a non-loopback `http://` URL logs a warning with the host.
    ///
    /// The opt-in applies to URLs given in code. A URL from `ANTHROPIC_BASE_URL`
    /// is acknowledged by `ANTHROPIC_ALLOW_INSECURE_HTTP` instead; see
    /// [`new`](Self::new).
    ///
    /// # Example
    ///
    /// ```
    /// use adk_anthropic::Anthropic;
    ///
    /// let client = Anthropic::new(Some("placeholder-api-key".to_string()))?
    ///     .allow_insecure_http()
    ///     .with_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic".to_string())?;
    /// assert_eq!(client.base_url(), "http://10.60.1.20:8080/api/v1/llm/anthropic");
    /// # Ok::<(), adk_anthropic::Error>(())
    /// ```
    pub fn allow_insecure_http(mut self) -> Self {
        self.allow_insecure_http = true;
        self
    }

    /// Set a custom base URL for this client.
    ///
    /// This method allows you to specify a different API endpoint for the client.
    /// The base URL should be the root URL without the `/v1/` suffix - this will
    /// be added automatically when constructing request URLs.
    ///
    /// The URL is validated when this method is called. To use a plain `http://`
    /// URL on a non-loopback host, call
    /// [`allow_insecure_http`](Self::allow_insecure_http) first.
    ///
    /// # Errors
    ///
    /// Every request made by this client attaches the Anthropic API key, so the
    /// base URL must be encrypted. Returns a validation error unless the URL uses
    /// `https://`, or `http://` with a loopback host (`localhost`, `127.0.0.1`,
    /// `[::1]`) for local development, or `http://` after
    /// [`allow_insecure_http`](Self::allow_insecure_http).
    ///
    /// # Examples
    ///
    /// ```
    /// # use adk_anthropic::Anthropic;
    /// // For Anthropic's API (default)
    /// let client = Anthropic::new(Some("placeholder-api-key".to_string()))?
    ///     .with_base_url("https://api.anthropic.com".to_string())?;
    ///
    /// // For Minimax (international)
    /// let client = Anthropic::new(Some("placeholder-api-key".to_string()))?
    ///     .with_base_url("https://api.minimax.io/anthropic".to_string())?;
    ///
    /// // For Minimax (China)
    /// let client = Anthropic::new(Some("placeholder-api-key".to_string()))?
    ///     .with_base_url("https://api.minimaxi.com/anthropic".to_string())?;
    /// # Ok::<(), adk_anthropic::Error>(())
    /// ```
    pub fn with_base_url(mut self, base_url: String) -> Result<Self> {
        let insecure_http = if self.allow_insecure_http {
            InsecureHttp::Acknowledged
        } else {
            InsecureHttp::Rejected { opt_in: Some(CODE_INSECURE_HTTP_OPT_IN) }
        };
        validate_base_url(&base_url, insecure_http)?;
        self.base_url = base_url;
        Ok(self)
    }

    /// Return the effective API base URL used for message requests.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Set a custom timeout for this client.
    ///
    /// The timeout bounds a whole non-streaming request, and the wait for the
    /// response headers of a streaming request. A streamed body is not bounded
    /// by it — see [`with_stream_timeout`](Self::with_stream_timeout).
    ///
    /// # Errors
    ///
    /// Returns an HTTP client error when the underlying client cannot be built.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        self.timeout = timeout;
        self.client = Self::build_http_client(Some(timeout), timeout)?;
        self.stream_client = Self::build_http_client(None, timeout)?;
        Ok(self)
    }

    /// Bound the total duration of each streaming request, body included.
    ///
    /// Streams have no total bound by default: a long generation keeps running
    /// for as long as the server keeps sending data, and a stalled stream fails
    /// after 30 seconds without data. `None` restores that default.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use adk_anthropic::Anthropic;
    ///
    /// let client = Anthropic::new(Some("placeholder-api-key".to_string()))?
    ///     .with_stream_timeout(Some(Duration::from_secs(600)));
    /// # Ok::<(), adk_anthropic::Error>(())
    /// ```
    pub fn with_stream_timeout(mut self, stream_timeout: Option<Duration>) -> Self {
        self.stream_timeout = stream_timeout;
        self
    }

    /// Set the maximum number of retries for this client.
    ///
    /// This method allows you to specify how many times to retry failed requests.
    pub fn with_max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Get the API key being used by this client.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Set the backoff parameters for this client.
    ///
    /// This method allows you to configure the exponential backoff algorithm.
    pub fn with_backoff_params(mut self, throughput_ops_sec: f64, reserve_capacity: f64) -> Self {
        self.throughput_ops_sec = throughput_ops_sec;
        self.reserve_capacity = reserve_capacity;
        self
    }

    /// Set both a custom base URL and timeout for this client.
    ///
    /// This is a convenience method that chains [`with_base_url`](Self::with_base_url)
    /// and [`with_timeout`](Self::with_timeout). The URL is validated when this
    /// method is called, so a plain `http://` URL on a non-loopback host needs
    /// [`allow_insecure_http`](Self::allow_insecure_http) to be called first.
    ///
    /// # Errors
    ///
    /// Returns the validation error of [`with_base_url`](Self::with_base_url), or
    /// an HTTP client error when the underlying client cannot be built.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use adk_anthropic::Anthropic;
    ///
    /// let client = Anthropic::new(Some("placeholder-api-key".to_string()))?
    ///     .allow_insecure_http()
    ///     .with_base_url_and_timeout(
    ///         "http://gateway.corp.internal/anthropic".to_string(),
    ///         Duration::from_secs(30),
    ///     )?;
    /// # let _ = client;
    /// # Ok::<(), adk_anthropic::Error>(())
    /// ```
    pub fn with_base_url_and_timeout(self, base_url: String, timeout: Duration) -> Result<Self> {
        self.with_base_url(base_url)?.with_timeout(timeout)
    }

    /// Build default headers for API requests (static method for initialization).
    fn build_default_headers(api_key: &str) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        let mut api_key_value = HeaderValue::from_str(api_key).map_err(|e| {
            Error::validation(format!("Invalid API key format: {e}"), Some("api_key".to_string()))
        })?;
        // Keeps the key out of `Debug` output of the header map and of request logs.
        api_key_value.set_sensitive(true);
        headers.insert("x-api-key", api_key_value);
        headers.insert("anthropic-version", HeaderValue::from_static(ANTHROPIC_API_VERSION));
        Ok(headers)
    }

    /// Get cached headers for performance (no allocation needed).
    fn default_headers(&self) -> HeaderMap {
        (*self.cached_headers).clone()
    }

    /// Adds default HTTP headers, replacing defaults with the same name.
    ///
    /// Per-request replacement headers still take precedence. Automatically
    /// selected beta headers continue to be derived from message parameters. The
    /// headers are stored on the existing client rather than rebuilding it, so this
    /// method cannot fail and returns `Self`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_anthropic::Anthropic;
    /// use reqwest::header::{HeaderMap, HeaderValue};
    /// let mut headers = HeaderMap::new();
    /// headers.insert("x-session-id", HeaderValue::from_static("conversation-1"));
    /// let client = Anthropic::new(Some("api-key".into()))?.with_default_headers(headers);
    /// # Ok::<(), adk_anthropic::Error>(())
    /// ```
    #[must_use]
    pub fn with_default_headers(mut self, headers: HeaderMap) -> Self {
        Arc::make_mut(&mut self.cached_headers).extend(headers);
        self
    }

    /// Return a copy of the headers this client normally sends.
    ///
    /// Callers can modify this map and pass it to
    /// [`Anthropic::send_with_headers`] or [`Anthropic::stream_with_headers`].
    /// Those methods use replacement semantics, so the supplied map is sent
    /// instead of these defaults.
    pub fn default_headers_for_request(&self) -> HeaderMap {
        self.default_headers()
    }

    /// Build a full endpoint URL from the base URL and endpoint path.
    ///
    /// This method handles trailing slashes gracefully and always inserts `/v1/`
    /// between the base URL and endpoint path. This allows the base URL to be
    /// specified without requiring a specific format (with or without trailing slash,
    /// with or without `/v1/` suffix).
    ///
    /// # Examples
    ///
    /// - Base: `https://api.anthropic.com`, endpoint: `messages` → `https://api.anthropic.com/v1/messages`
    /// - Base: `https://api.minimax.io/anthropic`, endpoint: `messages` → `https://api.minimax.io/anthropic/v1/messages`
    /// - Base: `https://example.com/`, endpoint: `models` → `https://example.com/v1/models`
    fn build_url(&self, endpoint: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/v1/{}", base, endpoint)
    }

    /// Retry wrapper that implements exponential backoff with header-based retry-after
    async fn retry_with_backoff<F, Fut, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let backoff = ExponentialBackoff::new(self.throughput_ops_sec, self.reserve_capacity);
        let mut last_error = None;

        for attempt in 0..=self.max_retries {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(error) => {
                    // Check if error is retryable
                    if !error.is_retryable() {
                        return Err(error);
                    }

                    // Don't sleep on the last attempt
                    if attempt == self.max_retries {
                        last_error = Some(error);
                        break;
                    }

                    // Calculate backoff duration
                    let exp_backoff_duration = backoff.next();

                    // Get retry-after from error if available
                    let header_backoff_duration = match &error {
                        Error::RateLimit { retry_after: Some(seconds), .. } => {
                            Some(Duration::from_secs(*seconds))
                        }
                        Error::ServiceUnavailable { retry_after: Some(seconds), .. } => {
                            Some(Duration::from_secs(*seconds))
                        }
                        _ => None,
                    };

                    // Take the maximum of exponential backoff and header-based backoff
                    let sleep_duration = match header_backoff_duration {
                        Some(header_duration) => exp_backoff_duration.max(header_duration),
                        None => exp_backoff_duration,
                    };

                    CLIENT_REQUEST_RETRIES.click();
                    CLIENT_RETRY_BACKOFF.add(sleep_duration.as_secs_f64());
                    sleep(sleep_duration).await;
                    last_error = Some(error);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| Error::unknown("Failed after retries without capturing error")))
    }

    /// Process API response errors and convert to our Error type
    async fn process_error_response(response: Response) -> Error {
        let status = response.status();
        let status_code = status.as_u16();

        // Get headers we might need for error processing
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|val| val.to_str().ok())
            .map(String::from);

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|val| val.to_str().ok())
            .and_then(|val| val.parse::<u64>().ok());

        // Try to parse error response body
        #[derive(Deserialize)]
        struct ErrorResponse {
            error: Option<ErrorDetail>,
        }

        #[derive(Deserialize)]
        struct ErrorDetail {
            #[serde(rename = "type")]
            error_type: Option<String>,
            message: Option<String>,
            param: Option<String>,
        }

        let error_body = match response.text().await {
            Ok(body) => body,
            Err(e) => {
                return Error::http_client(
                    format!("Failed to read error response: {e}"),
                    Some(Box::new(e)),
                );
            }
        };

        // Try to parse as JSON first
        let parsed_error = serde_json::from_str::<ErrorResponse>(&error_body).ok();
        let error_type =
            parsed_error.as_ref().and_then(|e| e.error.as_ref()).and_then(|e| e.error_type.clone());
        let error_message = parsed_error
            .as_ref()
            .and_then(|e| e.error.as_ref())
            .and_then(|e| e.message.clone())
            .unwrap_or_else(|| error_body.clone());
        let error_param =
            parsed_error.as_ref().and_then(|e| e.error.as_ref()).and_then(|e| e.param.clone());

        // Map HTTP status code to appropriate error type
        match status_code {
            400 => Error::bad_request(error_message, error_param),
            401 => Error::authentication(error_message),
            403 => Error::permission(error_message),
            404 => Error::not_found(error_message, None, None),
            408 => Error::timeout(error_message, None),
            429 => Error::rate_limit(error_message, retry_after),
            500 => Error::internal_server(error_message, request_id),
            502..=504 => Error::service_unavailable(error_message, retry_after),
            529 => Error::rate_limit(error_message, retry_after),
            _ => Error::api(status_code, error_type, error_message, request_id),
        }
    }

    /// Convert reqwest errors to appropriate Error types
    fn map_request_error(&self, e: reqwest::Error) -> Error {
        if e.is_timeout() {
            Error::timeout(format!("Request timed out: {e}"), Some(self.timeout.as_secs_f64()))
        } else if e.is_connect() {
            Error::connection(format!("Connection error: {e}"), Some(Box::new(e)))
        } else {
            Error::http_client(format!("Request failed: {e}"), Some(Box::new(e)))
        }
    }

    /// Execute a POST request with error handling
    async fn execute_post_request<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &impl serde::Serialize,
        headers: Option<HeaderMap>,
    ) -> Result<T> {
        let headers = headers.unwrap_or_else(|| self.default_headers());

        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|e| self.map_request_error(e))?;

        if !response.status().is_success() {
            return Err(Self::process_error_response(response).await);
        }

        response.json::<T>().await.map_err(|e| {
            Error::serialization(format!("Failed to parse response: {e}"), Some(Box::new(e)))
        })
    }

    /// Send a streaming POST request on the client that has no total timeout.
    ///
    /// The configured timeout bounds the wait for the response headers, the SSE
    /// layer bounds inactivity in the body, and `stream_timeout`, when set,
    /// bounds the whole request.
    async fn send_stream_request(
        &self,
        url: &str,
        headers: HeaderMap,
        body: &impl serde::Serialize,
    ) -> Result<Response> {
        let mut request = self.stream_client.post(url).headers(headers).json(body);
        if let Some(stream_timeout) = self.stream_timeout {
            request = request.timeout(stream_timeout);
        }

        let response = tokio::time::timeout(self.timeout, request.send())
            .await
            .map_err(|_| {
                Error::timeout(
                    format!(
                        "Timed out after {} seconds waiting for the streaming response headers",
                        self.timeout.as_secs_f64()
                    ),
                    Some(self.timeout.as_secs_f64()),
                )
            })?
            .map_err(|e| self.map_request_error(e))?;

        if !response.status().is_success() {
            return Err(Self::process_error_response(response).await);
        }
        Ok(response)
    }

    /// Execute a GET request with error handling
    async fn execute_get_request<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        query_params: Option<&[(String, String)]>,
    ) -> Result<T> {
        let mut request = self.client.get(url).headers(self.default_headers());

        if let Some(params) = query_params {
            for (key, value) in params {
                request = request.query(&[(key, value)]);
            }
        }

        let response = request.send().await.map_err(|e| self.map_request_error(e))?;

        if !response.status().is_success() {
            return Err(Self::process_error_response(response).await);
        }

        response.json::<T>().await.map_err(|e| {
            Error::serialization(format!("Failed to parse response: {e}"), Some(Box::new(e)))
        })
    }

    fn append_beta_header(headers: &mut HeaderMap, beta: &str) -> Result<()> {
        if beta.trim().is_empty() {
            return Err(Error::validation(
                "Beta identifier cannot be empty".to_string(),
                Some("anthropic-beta".to_string()),
            ));
        }
        let existing = headers
            .get("anthropic-beta")
            .map(HeaderValue::to_str)
            .transpose()
            .map_err(|error| {
                Error::validation(
                    format!("Invalid existing anthropic-beta header: {error}"),
                    Some("anthropic-beta".to_string()),
                )
            })?
            .unwrap_or_default();
        if existing.split(',').any(|value| value.trim() == beta) {
            return Ok(());
        }

        let combined =
            if existing.is_empty() { beta.to_string() } else { format!("{existing},{beta}") };
        let value = HeaderValue::from_str(&combined).map_err(|error| {
            Error::validation(
                format!("Invalid anthropic-beta header: {error}"),
                Some("anthropic-beta".to_string()),
            )
        })?;
        headers.insert("anthropic-beta", value);
        Ok(())
    }

    fn message_headers(
        &self,
        params: &MessageCreateParams,
        extra_betas: &[&str],
        streaming: bool,
    ) -> Result<HeaderMap> {
        let mut headers = self.default_headers();
        if streaming {
            headers.insert(header::ACCEPT, HeaderValue::from_static("text/event-stream"));
        }
        if params.requires_structured_outputs_beta() {
            Self::append_beta_header(&mut headers, STRUCTURED_OUTPUTS_BETA)?;
        }
        if params.context_management.is_some() {
            Self::append_beta_header(&mut headers, "context-management-2025-06-27")?;
        }
        if params.speed.is_some() {
            Self::append_beta_header(&mut headers, "fast-mode-2026-02-01")?;
        }
        for beta in extra_betas {
            Self::append_beta_header(&mut headers, beta)?;
        }
        Ok(headers)
    }

    async fn send_with_resolved_headers(
        &self,
        mut params: MessageCreateParams,
        replacement_headers: Option<HeaderMap>,
    ) -> Result<Message> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();

        // Validate parameters first
        if let Err(err) = params.validate() {
            CLIENT_REQUEST_ERRORS.click();
            CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
            return Err(err);
        }

        // Ensure stream is disabled
        params.stream = false;

        // Task 8.1: When thinking is Enabled, force temperature to 1.0
        if matches!(params.thinking, Some(ThinkingConfig::Enabled { .. })) {
            params.temperature = Some(1.0);
        }

        let headers = match replacement_headers {
            Some(headers) => headers,
            None => self.message_headers(&params, &[], false)?,
        };

        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages");
                self.execute_post_request(&url, &params, Some(headers.clone())).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Send a message to the API and get a non-streaming response.
    pub async fn send(&self, params: MessageCreateParams) -> Result<Message> {
        self.send_with_resolved_headers(params, None).await
    }

    /// Send a message using an exact replacement header map.
    ///
    /// The supplied headers replace all client defaults and automatically
    /// generated beta headers. This permits per-request bearer authentication,
    /// API versions, beta selection, and deliberate beta suppression. Start
    /// from [`Anthropic::default_headers_for_request`] when only a small change
    /// is needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the parameters are invalid, a header is invalid,
    /// or the request fails.
    pub async fn send_with_headers(
        &self,
        params: MessageCreateParams,
        headers: HeaderMap,
    ) -> Result<Message> {
        self.send_with_resolved_headers(params, Some(headers)).await
    }

    /// Send a message with caller-selected Anthropic beta versions.
    ///
    /// Caller-selected betas are de-duplicated with beta headers required by
    /// the request's typed features. They are sent only as headers and never
    /// serialized into the JSON body.
    ///
    /// # Errors
    ///
    /// Returns an error when a beta identifier or message parameter is invalid,
    /// or the request fails.
    pub async fn send_with_betas(
        &self,
        params: MessageCreateParams,
        betas: &[&str],
    ) -> Result<Message> {
        let headers = self.message_headers(&params, betas, false)?;
        self.send_with_resolved_headers(params, Some(headers)).await
    }

    /// Send a message to the API with logging and get a non-streaming response.
    ///
    /// This method is identical to [`send`](Self::send) but additionally logs
    /// the response through the provided [`ClientLogger`].
    pub async fn send_with_logger(
        &self,
        params: MessageCreateParams,
        logger: &dyn ClientLogger,
    ) -> Result<Message> {
        let result = self.send(params).await;
        if let Ok(ref message) = result {
            logger.log_response(message);
        }
        result
    }

    /// Send a message to the API and get a streaming response.
    ///
    /// Returns a stream of MessageStreamEvent objects that can be processed incrementally.
    async fn stream_with_resolved_headers(
        &self,
        params: &MessageCreateParams,
        replacement_headers: Option<HeaderMap>,
    ) -> Result<impl Stream<Item = Result<MessageStreamEvent>> + use<>> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();

        // Validate parameters first
        if let Err(err) = params.validate() {
            CLIENT_REQUEST_ERRORS.click();
            CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
            return Err(err);
        }

        // Task 8.3: Clone and force stream = true in the request body
        let mut params = params.clone();
        params.stream = true;

        // Task 8.1: When thinking is Enabled, force temperature to 1.0
        if matches!(params.thinking, Some(ThinkingConfig::Enabled { .. })) {
            params.temperature = Some(1.0);
        }

        let headers = match replacement_headers {
            Some(headers) => headers,
            None => self.message_headers(&params, &[], true)?,
        };

        let response = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages");
                self.send_stream_request(&url, headers.clone(), &params).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                CLIENT_REQUEST_ERRORS.click();
                return Err(err);
            }
        };

        // Get the byte stream from the response
        let stream = response.bytes_stream();

        // Create an SSE processor
        Ok(process_sse(stream))
    }

    /// Send a message to the API and stream response events.
    pub async fn stream(
        &self,
        params: &MessageCreateParams,
    ) -> Result<impl Stream<Item = Result<MessageStreamEvent>> + use<>> {
        self.stream_with_resolved_headers(params, None).await
    }

    /// Stream a message using an exact replacement header map.
    ///
    /// The supplied headers replace all defaults, including `Accept`,
    /// authentication, API-version, and generated beta headers. Callers should
    /// normally include `Accept: text/event-stream`.
    ///
    /// # Errors
    ///
    /// Returns an error when the parameters are invalid, a header is invalid,
    /// or the request fails.
    pub async fn stream_with_headers(
        &self,
        params: &MessageCreateParams,
        headers: HeaderMap,
    ) -> Result<impl Stream<Item = Result<MessageStreamEvent>> + use<>> {
        self.stream_with_resolved_headers(params, Some(headers)).await
    }

    /// Stream a message with caller-selected Anthropic beta versions.
    ///
    /// Caller-selected betas are de-duplicated with beta headers required by
    /// the request's typed features. They are sent only as headers and never
    /// serialized into the JSON body.
    ///
    /// # Errors
    ///
    /// Returns an error when a beta identifier or message parameter is invalid,
    /// or the request fails.
    pub async fn stream_with_betas(
        &self,
        params: &MessageCreateParams,
        betas: &[&str],
    ) -> Result<impl Stream<Item = Result<MessageStreamEvent>> + use<>> {
        let headers = self.message_headers(params, betas, true)?;
        self.stream_with_resolved_headers(params, Some(headers)).await
    }

    /// Send a message with Claude server-side refusal fallback enabled.
    ///
    /// The beta API retries only safety-classifier refusals. Rate limits,
    /// overloads, and server failures are returned without fallback.
    ///
    /// # Errors
    ///
    /// Returns an error when the request or fallback configuration is invalid,
    /// or the request fails.
    pub async fn send_with_server_fallbacks(
        &self,
        mut request: ServerFallbackRequest,
    ) -> Result<ServerFallbackMessage> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        if let Err(error) = request.validate() {
            CLIENT_REQUEST_ERRORS.click();
            CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
            return Err(error);
        }

        request.params.stream = false;
        if matches!(request.params.thinking, Some(ThinkingConfig::Enabled { .. })) {
            request.params.temperature = Some(1.0);
        }
        let headers = self.message_headers(&request.params, &[SERVER_FALLBACK_BETA], false)?;
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages");
                self.execute_post_request(&url, &request, Some(headers.clone())).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Stream a message with Claude server-side refusal fallback enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the request or fallback configuration is invalid,
    /// or the request fails.
    pub async fn stream_with_server_fallbacks(
        &self,
        request: &ServerFallbackRequest,
    ) -> Result<impl Stream<Item = Result<ServerFallbackStreamEvent>> + use<>> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        if let Err(error) = request.validate() {
            CLIENT_REQUEST_ERRORS.click();
            CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
            return Err(error);
        }

        let mut request = request.clone();
        request.params.stream = true;
        if matches!(request.params.thinking, Some(ThinkingConfig::Enabled { .. })) {
            request.params.temperature = Some(1.0);
        }
        let headers = self.message_headers(&request.params, &[SERVER_FALLBACK_BETA], true)?;
        let response = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages");
                self.send_stream_request(&url, headers.clone(), &request).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                CLIENT_REQUEST_ERRORS.click();
                return Err(error);
            }
        };
        Ok(process_json_sse(response.bytes_stream()))
    }

    /// Send a message to the API with logging and get a streaming response.
    ///
    /// This method is identical to [`stream`](Self::stream) but additionally logs
    /// each streaming event and the final reconstructed message through the
    /// provided [`ClientLogger`].
    ///
    /// Returns a [`LoggingStream`] that wraps an [`AccumulatingStream`], logging
    /// each event as it passes through and logging the final message when the
    /// stream completes.
    pub async fn stream_with_logger<'a>(
        &self,
        params: &MessageCreateParams,
        logger: &'a dyn ClientLogger,
    ) -> Result<LoggingStream<'a>> {
        let raw_stream = self.stream(params).await?;
        let (accumulating_stream, receiver) = AccumulatingStream::new(raw_stream);
        Ok(LoggingStream::new(accumulating_stream, receiver, logger))
    }

    /// Count tokens for a message.
    ///
    /// This method counts the number of tokens that would be used by a message with the given parameters.
    /// It's useful for estimating costs or making sure your messages fit within the model's context window.
    pub async fn count_tokens(
        &self,
        params: MessageCountTokensParams,
    ) -> Result<MessageTokensCount> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages/count_tokens");
                self.execute_post_request(&url, &params, None).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// List available models from the API.
    ///
    /// Returns a paginated list of all available models. Use the parameters to control
    /// pagination and filter results.
    pub async fn list_models(&self, params: Option<ModelListParams>) -> Result<ModelListResponse> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("models");

                let query_params = params.as_ref().map(|p| {
                    let mut params = Vec::new();
                    if let Some(ref after_id) = p.after_id {
                        params.push(("after_id".to_string(), after_id.clone()));
                    }
                    if let Some(ref before_id) = p.before_id {
                        params.push(("before_id".to_string(), before_id.clone()));
                    }
                    if let Some(limit) = p.limit {
                        params.push(("limit".to_string(), limit.to_string()));
                    }
                    params
                });

                self.execute_get_request(&url, query_params.as_deref()).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Retrieve information about a specific model.
    ///
    /// Returns detailed information about the specified model, including its
    /// ID, creation date, display name, and type.
    pub async fn get_model(&self, model_id: &str) -> Result<ModelInfo> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("models/{}", model_id));
                self.execute_get_request(&url, None).await
            })
            .await;

        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    // --- Helper methods for DELETE and multipart requests ---

    /// Execute a DELETE request with error handling.
    async fn execute_delete_request(&self, url: &str) -> Result<()> {
        let response = self
            .client
            .delete(url)
            .headers(self.default_headers())
            .send()
            .await
            .map_err(|e| self.map_request_error(e))?;

        if !response.status().is_success() {
            return Err(Self::process_error_response(response).await);
        }

        Ok(())
    }

    /// Build standard pagination query params.
    fn pagination_params(
        before_id: Option<&str>,
        after_id: Option<&str>,
        limit: Option<u32>,
    ) -> Option<Vec<(String, String)>> {
        let mut params = Vec::new();
        if let Some(before) = before_id {
            params.push(("before_id".to_string(), before.to_string()));
        }
        if let Some(after) = after_id {
            params.push(("after_id".to_string(), after.to_string()));
        }
        if let Some(lim) = limit {
            params.push(("limit".to_string(), lim.to_string()));
        }
        if params.is_empty() { None } else { Some(params) }
    }

    // --- Batches API (Req 13) ---

    /// Create a message batch for asynchronous processing.
    pub async fn create_batch(&self, requests: Vec<BatchRequest>) -> Result<MessageBatch> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let body = serde_json::json!({ "requests": requests });
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages/batches");
                self.execute_post_request(&url, &body, None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Get the status of a message batch.
    pub async fn get_batch(&self, batch_id: &str) -> Result<MessageBatch> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("messages/batches/{batch_id}"));
                self.execute_get_request(&url, None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Get results of a completed batch as newline-delimited JSON.
    pub async fn batch_results(&self, batch_id: &str) -> Result<Vec<BatchResultItem>> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("messages/batches/{batch_id}/results"));
                let response = self
                    .client
                    .get(&url)
                    .headers(self.default_headers())
                    .send()
                    .await
                    .map_err(|e| self.map_request_error(e))?;

                if !response.status().is_success() {
                    return Err(Self::process_error_response(response).await);
                }

                let text = response.text().await.map_err(|e| {
                    Error::serialization(
                        format!("Failed to read batch results: {e}"),
                        Some(Box::new(e)),
                    )
                })?;

                let mut items = Vec::new();
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let item: BatchResultItem = serde_json::from_str(trimmed)?;
                    items.push(item);
                }
                Ok(items)
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Cancel an in-progress batch.
    pub async fn cancel_batch(&self, batch_id: &str) -> Result<MessageBatch> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("messages/batches/{batch_id}/cancel"));
                self.execute_post_request(&url, &serde_json::json!({}), None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Delete a batch.
    pub async fn delete_batch(&self, batch_id: &str) -> Result<()> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("messages/batches/{batch_id}"));
                self.execute_delete_request(&url).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// List message batches with pagination.
    pub async fn list_batches(
        &self,
        before_id: Option<&str>,
        after_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<PaginatedList<MessageBatch>> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("messages/batches");
                let query = Self::pagination_params(before_id, after_id, limit);
                self.execute_get_request(&url, query.as_deref()).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    // --- Files API (Req 21) ---

    /// Upload a file via multipart form upload.
    pub async fn upload_file(
        &self,
        data: Vec<u8>,
        mime_type: &str,
        filename: &str,
        purpose: &str,
    ) -> Result<FileObject> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();

        let mime_type = mime_type.to_string();
        let filename = filename.to_string();
        let purpose = purpose.to_string();

        let result = self
            .retry_with_backoff(|| {
                let data = data.clone();
                let mime_type = mime_type.clone();
                let filename = filename.clone();
                let purpose = purpose.clone();
                async move {
                    let url = self.build_url("files");
                    let part = reqwest::multipart::Part::bytes(data)
                        .file_name(filename)
                        .mime_str(&mime_type)
                        .map_err(|e| {
                            Error::validation(
                                format!("Invalid MIME type: {e}"),
                                Some("mime_type".to_string()),
                            )
                        })?;
                    let form =
                        reqwest::multipart::Form::new().text("purpose", purpose).part("file", part);

                    let response = self
                        .client
                        .post(&url)
                        .headers(self.default_headers())
                        .multipart(form)
                        .send()
                        .await
                        .map_err(|e| self.map_request_error(e))?;

                    if !response.status().is_success() {
                        return Err(Self::process_error_response(response).await);
                    }

                    response.json::<FileObject>().await.map_err(|e| {
                        Error::serialization(
                            format!("Failed to parse file response: {e}"),
                            Some(Box::new(e)),
                        )
                    })
                }
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Get metadata for a file.
    pub async fn get_file(&self, file_id: &str) -> Result<FileObject> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("files/{file_id}"));
                self.execute_get_request(&url, None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Delete a file.
    pub async fn delete_file(&self, file_id: &str) -> Result<()> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("files/{file_id}"));
                self.execute_delete_request(&url).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// List files with pagination.
    pub async fn list_files(
        &self,
        before_id: Option<&str>,
        after_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<PaginatedList<FileObject>> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("files");
                let query = Self::pagination_params(before_id, after_id, limit);
                self.execute_get_request(&url, query.as_deref()).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    // --- Skills API (Req 22) ---

    /// Create a new skill.
    pub async fn create_skill(
        &self,
        name: &str,
        description: &str,
        content: Vec<u8>,
    ) -> Result<SkillObject> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let body = serde_json::json!({
            "name": name,
            "description": description,
            "content": base64_encode(&content),
        });
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("skills");
                self.execute_post_request(&url, &body, None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Get a skill by ID.
    pub async fn get_skill(&self, skill_id: &str) -> Result<SkillObject> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("skills/{skill_id}"));
                self.execute_get_request(&url, None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Update a skill's content.
    pub async fn update_skill(&self, skill_id: &str, content: Vec<u8>) -> Result<SkillObject> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let body = serde_json::json!({
            "content": base64_encode(&content),
        });
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("skills/{skill_id}"));
                self.execute_post_request(&url, &body, None).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// Delete a skill.
    pub async fn delete_skill(&self, skill_id: &str) -> Result<()> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url(&format!("skills/{skill_id}"));
                self.execute_delete_request(&url).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }

    /// List skills with pagination.
    pub async fn list_skills(
        &self,
        before_id: Option<&str>,
        after_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<PaginatedList<SkillObject>> {
        let start = Instant::now();
        CLIENT_REQUESTS.click();
        let result = self
            .retry_with_backoff(|| async {
                let url = self.build_url("skills");
                let query = Self::pagination_params(before_id, after_id, limit);
                self.execute_get_request(&url, query.as_deref()).await
            })
            .await;
        CLIENT_REQUEST_DURATION.add(start.elapsed().as_secs_f64());
        if result.is_err() {
            CLIENT_REQUEST_ERRORS.click();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn debug_output_redacts_api_key_and_bearer_token() {
        let client = Anthropic::new(Some("sk-ant-secret-key-value".to_string())).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("sk-ant-secret-key-value"), "api key leaked: {debug}");
        assert!(debug.contains("[REDACTED]"));

        let client = Anthropic::new_with_auth_token("secret-bearer-token").unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret-bearer-token"), "bearer token leaked: {debug}");
    }

    #[test]
    fn api_key_header_is_marked_sensitive() {
        let headers = Anthropic::build_default_headers("sk-ant-secret-key-value").unwrap();

        assert!(headers.get("x-api-key").unwrap().is_sensitive());
    }

    #[tokio::test]
    async fn retry_logic_with_backoff() {
        let client = Anthropic {
            api_key: "test".to_string(),
            client: ReqwestClient::new(),
            stream_client: ReqwestClient::new(),
            base_url: "http://localhost".to_string(),
            timeout: Duration::from_secs(1),
            stream_timeout: None,
            max_retries: 2,
            throughput_ops_sec: 1.0 / 60.0,
            reserve_capacity: 1.0 / 60.0,
            cached_headers: Arc::new(HeaderMap::new()),
            allow_insecure_http: false,
        };

        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = attempt_counter.clone();

        let result = client
            .retry_with_backoff(|| {
                let counter = counter_clone.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    match attempt {
                        0 | 1 => Err(Error::rate_limit("Rate limited", Some(1))),
                        _ => Ok("success".to_string()),
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_logic_with_non_retryable_error() {
        let client = Anthropic {
            api_key: "test".to_string(),
            client: ReqwestClient::new(),
            stream_client: ReqwestClient::new(),
            base_url: "http://localhost".to_string(),
            timeout: Duration::from_secs(1),
            stream_timeout: None,
            max_retries: 2,
            throughput_ops_sec: 1.0 / 60.0,
            reserve_capacity: 1.0 / 60.0,
            cached_headers: Arc::new(HeaderMap::new()),
            allow_insecure_http: false,
        };

        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = attempt_counter.clone();

        let result: Result<String> = client
            .retry_with_backoff(|| {
                let counter = counter_clone.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err(Error::authentication("Invalid API key"))
                }
            })
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().is_authentication());
        // Should only attempt once since authentication errors are not retryable
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_logic_max_retries_exceeded() {
        let client = Anthropic {
            api_key: "test".to_string(),
            client: ReqwestClient::new(),
            stream_client: ReqwestClient::new(),
            base_url: "http://localhost".to_string(),
            timeout: Duration::from_secs(1),
            stream_timeout: None,
            max_retries: 2,
            throughput_ops_sec: 1.0 / 60.0,
            reserve_capacity: 1.0 / 60.0,
            cached_headers: Arc::new(HeaderMap::new()),
            allow_insecure_http: false,
        };

        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = attempt_counter.clone();

        let result: Result<String> = client
            .retry_with_backoff(|| {
                let counter = counter_clone.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err(Error::rate_limit("Always rate limited", Some(1)))
                }
            })
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().is_rate_limit());
        // Should attempt max_retries + 1 times (3 total: initial + 2 retries)
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn error_529_is_retryable() {
        // Test that 529 errors are properly mapped to rate_limit and are retryable
        let client = Anthropic {
            api_key: "test".to_string(),
            client: ReqwestClient::new(),
            stream_client: ReqwestClient::new(),
            base_url: "http://localhost".to_string(),
            timeout: Duration::from_secs(1),
            stream_timeout: None,
            max_retries: 2,
            throughput_ops_sec: 1.0 / 60.0,
            reserve_capacity: 1.0 / 60.0,
            cached_headers: Arc::new(HeaderMap::new()),
            allow_insecure_http: false,
        };

        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = attempt_counter.clone();

        let result = client
            .retry_with_backoff(|| {
                let counter = counter_clone.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    match attempt {
                        0 | 1 => {
                            // Simulate a 529 overloaded error
                            Err(Error::api(
                                529,
                                Some("overloaded_error".to_string()),
                                "Overloaded".to_string(),
                                None,
                            ))
                        }
                        _ => Ok("success".to_string()),
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
        // Should retry: initial attempt + 2 retries = 3 total
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn error_529_mapped_correctly() {
        // Test that a 529 API error is correctly identified as retryable
        let error =
            Error::api(529, Some("overloaded_error".to_string()), "Overloaded".to_string(), None);
        assert!(error.is_retryable());

        // Test that rate_limit error (which 529 now maps to) is also retryable
        let rate_limit_error = Error::rate_limit("Overloaded", Some(5));
        assert!(rate_limit_error.is_retryable());
    }

    #[test]
    fn explicit_configuration() {
        if std::env::var_os("ADK_EXPLICIT_CONFIG_CHILD").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "client::tests::explicit_configuration"])
                .env("ADK_EXPLICIT_CONFIG_CHILD", "1")
                .env("ANTHROPIC_BASE_URL", "invalid-environment-url")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let key = "sk-explicit-key";
        let client = Anthropic::new_with_base_url(key, "http://127.0.0.1:12345").unwrap();
        assert_eq!(client.api_key, key);
        assert_eq!(client.cached_headers["x-api-key"], key);
        assert_eq!(client.base_url, "http://127.0.0.1:12345");
        assert!(Anthropic::new_with_base_url(key, "invalid-explicit-url").is_err());

        let dir =
            std::env::temp_dir().join(format!("adk_anthropic_explicit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("key.txt");
        std::fs::write(&file, "sk-from-file\n").unwrap();
        let client =
            Anthropic::new_with_base_url(format!("file://{}", file.display()), "https://a.example")
                .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(client.api_key, "sk-from-file");
        assert!(
            Anthropic::new_with_base_url("file:///adk-missing-key-file", "https://a.example")
                .is_err()
        );
    }

    #[test]
    fn resolve_api_key_regular_value() {
        let result = Anthropic::resolve_api_key("sk-test-key-123");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sk-test-key-123");
    }

    #[test]
    fn resolve_api_key_file_url_absolute() {
        let test_dir =
            std::env::temp_dir().join(format!("adk_anthropic_test_{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).unwrap();
        let test_file = test_dir.join("test_api_key.txt");
        std::fs::write(&test_file, "sk-test-from-file-123\n").unwrap();

        let file_url = format!("file://{}", test_file.display());
        let result = Anthropic::resolve_api_key(&file_url);

        std::fs::remove_dir_all(&test_dir).unwrap();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sk-test-from-file-123");
    }

    #[test]
    fn resolve_api_key_file_url_relative() {
        let test_file = "test_relative_key.txt";
        std::fs::write(test_file, "sk-relative-key-456\n").unwrap();

        let file_url = format!("file://{}", test_file);
        let result = Anthropic::resolve_api_key(&file_url);

        std::fs::remove_file(test_file).unwrap();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sk-relative-key-456");
    }

    #[test]
    fn resolve_api_key_file_url_nonexistent() {
        let result = Anthropic::resolve_api_key("file:///nonexistent/path/to/key.txt");
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.is_validation());
        assert!(format!("{}", error).contains("Failed to read API key from file"));
    }

    #[test]
    fn resolve_api_key_file_url_with_whitespace() {
        let test_file = "test_whitespace_key.txt";
        std::fs::write(test_file, "  sk-whitespace-key-789  \n  ").unwrap();

        let file_url = format!("file://{}", test_file);
        let result = Anthropic::resolve_api_key(&file_url);

        std::fs::remove_file(test_file).unwrap();

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sk-whitespace-key-789");
    }

    #[test]
    fn client_builder_methods() {
        let client = Anthropic::new(Some("test_key".to_string())).unwrap();

        // Test builder pattern methods
        let configured_client = client
            .with_base_url("https://custom.api.com".to_string())
            .unwrap()
            .with_max_retries(5)
            .with_backoff_params(2.0, 1.0);

        assert_eq!(configured_client.base_url, "https://custom.api.com");
        assert_eq!(configured_client.max_retries, 5);
        assert_eq!(configured_client.throughput_ops_sec, 2.0);
        assert_eq!(configured_client.reserve_capacity, 1.0);
    }

    #[test]
    fn build_url_default_base() {
        let client = Anthropic::new(Some("test_key".to_string())).unwrap();
        // Default base URL: https://api.anthropic.com
        assert_eq!(client.build_url("messages"), "https://api.anthropic.com/v1/messages");
        assert_eq!(
            client.build_url("messages/count_tokens"),
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
        assert_eq!(client.build_url("models"), "https://api.anthropic.com/v1/models");
    }

    #[test]
    fn build_url_custom_base_without_trailing_slash() {
        let client = Anthropic::new(Some("test_key".to_string()))
            .unwrap()
            .with_base_url("https://api.minimax.io/anthropic".to_string())
            .unwrap();
        assert_eq!(client.build_url("messages"), "https://api.minimax.io/anthropic/v1/messages");
    }

    #[test]
    fn build_url_custom_base_with_trailing_slash() {
        let client = Anthropic::new(Some("test_key".to_string()))
            .unwrap()
            .with_base_url("https://api.minimax.io/anthropic/".to_string())
            .unwrap();
        assert_eq!(client.build_url("messages"), "https://api.minimax.io/anthropic/v1/messages");
    }

    #[test]
    fn build_url_minimax_china() {
        let client = Anthropic::new(Some("test_key".to_string()))
            .unwrap()
            .with_base_url("https://api.minimaxi.com/anthropic".to_string())
            .unwrap();
        assert_eq!(client.build_url("messages"), "https://api.minimaxi.com/anthropic/v1/messages");
        assert_eq!(
            client.build_url(&format!("models/{}", "claude-3-opus")),
            "https://api.minimaxi.com/anthropic/v1/models/claude-3-opus"
        );
    }

    #[test]
    fn with_base_url_accepts_https() {
        let client = Anthropic::new(Some("placeholder-api-key".to_string()))
            .unwrap()
            .with_base_url("https://gateway.example.com/anthropic".to_string())
            .unwrap();
        assert_eq!(client.base_url, "https://gateway.example.com/anthropic");
    }

    #[test]
    fn with_base_url_allows_loopback_http_for_local_dev() {
        for url in ["http://localhost:8080", "http://127.0.0.1:8080", "http://[::1]:8080"] {
            let client = Anthropic::new(Some("placeholder-api-key".to_string()))
                .unwrap()
                .with_base_url(url.to_string())
                .unwrap_or_else(|e| panic!("loopback url {url} should be accepted: {e}"));
            assert_eq!(client.base_url, url);
        }
    }

    #[test]
    fn with_base_url_rejects_cleartext_http() {
        let err = Anthropic::new(Some("placeholder-api-key".to_string()))
            .unwrap()
            .with_base_url("http://gateway.internal.example.com".to_string())
            .expect_err("a non-loopback http base URL must be rejected");

        assert!(err.is_validation(), "expected a validation error, got {err}");
        let message = err.to_string();
        assert!(
            message.contains("unencrypted"),
            "error should explain the cleartext risk, got: {message}"
        );
        assert!(message.contains("'http'"), "error should name the scheme, got: {message}");
        assert!(
            message.contains("allow_insecure_http"),
            "error should name the opt-in, got: {message}"
        );
    }

    #[test]
    fn allow_insecure_http_before_with_base_url_accepts_cleartext_http() {
        let client = Anthropic::new(Some("placeholder-api-key".to_string()))
            .unwrap()
            .allow_insecure_http()
            .with_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic".to_string())
            .unwrap();
        assert_eq!(client.base_url, "http://10.60.1.20:8080/api/v1/llm/anthropic");
        assert_eq!(
            client.build_url("messages"),
            "http://10.60.1.20:8080/api/v1/llm/anthropic/v1/messages"
        );

        let client = Anthropic::new_with_base_url("placeholder-api-key", DEFAULT_API_URL)
            .unwrap()
            .allow_insecure_http()
            .with_base_url_and_timeout(
                "http://gateway.corp.internal".to_string(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(client.base_url, "http://gateway.corp.internal");
        assert_eq!(client.timeout, Duration::from_secs(5));
    }

    #[test]
    fn allow_insecure_http_after_with_base_url_is_too_late() {
        let err = Anthropic::new(Some("placeholder-api-key".to_string()))
            .unwrap()
            .with_base_url("http://10.60.1.20:8080".to_string())
            .map(Anthropic::allow_insecure_http)
            .expect_err("with_base_url validates before the opt-in is set");
        assert!(err.is_validation(), "expected a validation error, got {err}");
        assert!(
            err.to_string().contains("allow_insecure_http"),
            "error should name the opt-in, got: {err}"
        );
    }

    #[test]
    fn allow_insecure_http_admits_http_only() {
        for url in ["ftp://files.example.com", "ws://gateway.example.com", "not-a-url"] {
            let err = Anthropic::new(Some("placeholder-api-key".to_string()))
                .unwrap()
                .allow_insecure_http()
                .with_base_url(url.to_string())
                .expect_err("the opt-in must not admit other schemes");
            assert!(err.is_validation(), "expected a validation error for {url}, got {err}");
        }
    }

    #[test]
    fn new_with_base_url_rejection_names_the_opt_in() {
        let err = Anthropic::new_with_base_url("placeholder-api-key", "http://10.60.1.20:8080")
            .expect_err("a non-loopback http base URL must be rejected");
        assert!(
            err.to_string().contains("allow_insecure_http"),
            "error should name the opt-in, got: {err}"
        );
    }

    #[test]
    fn insecure_http_environment() {
        const GATEWAY: &str = "http://10.60.1.20:8080/api/v1/llm/anthropic";
        if std::env::var_os("ADK_INSECURE_HTTP_ENV_CHILD").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "client::tests::insecure_http_environment"])
                .env("ADK_INSECURE_HTTP_ENV_CHILD", "1")
                .env("ANTHROPIC_BASE_URL", GATEWAY)
                .env("ANTHROPIC_ALLOW_INSECURE_HTTP", "true")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let client = Anthropic::new(Some("placeholder-api-key".to_string()))
            .expect("ANTHROPIC_ALLOW_INSECURE_HTTP admits the ANTHROPIC_BASE_URL gateway");
        assert_eq!(client.base_url(), GATEWAY);

        // The environment acknowledges the environment URL only, not URLs given in code.
        assert!(!client.allow_insecure_http);
        let err = client
            .with_base_url("http://gateway.corp.internal".to_string())
            .expect_err("a URL given in code needs allow_insecure_http()");
        assert!(err.to_string().contains("allow_insecure_http"), "got: {err}");
        assert!(Anthropic::new_with_base_url("placeholder-api-key", GATEWAY).is_err());
    }

    #[test]
    fn with_base_url_rejects_non_http_schemes_and_garbage() {
        for url in ["ftp://files.example.com", "ws://gateway.example.com", "not-a-url"] {
            let err = Anthropic::new(Some("placeholder-api-key".to_string()))
                .unwrap()
                .with_base_url(url.to_string())
                .expect_err("non-https, non-loopback base URL must be rejected");
            assert!(err.is_validation(), "expected a validation error for {url}, got {err}");
        }
    }

    // The `ANTHROPIC_BASE_URL` tests exercise `resolve_base_url` directly instead
    // of mutating the process environment. The environment is process-global, so
    // setting an intentionally-rejected value would race against every other test
    // in this binary that calls `Anthropic::new`. `resolve_base_url` is the exact
    // code path `Anthropic::new` uses for the env value, so nothing is lost.

    #[test]
    fn env_base_url_absent_falls_back_to_default() {
        let resolved = Anthropic::resolve_base_url(None, None).unwrap();
        assert_eq!(resolved, DEFAULT_API_URL);
        let resolved = Anthropic::resolve_base_url(None, Some("1".to_string())).unwrap();
        assert_eq!(resolved, DEFAULT_API_URL);
    }

    #[test]
    fn env_base_url_accepts_https() {
        let resolved = Anthropic::resolve_base_url(
            Some("https://gateway.example.com/anthropic".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(resolved, "https://gateway.example.com/anthropic");
    }

    #[test]
    fn env_base_url_allows_loopback_http_for_local_dev() {
        for url in ["http://localhost:11434", "http://127.0.0.1:11434", "http://[::1]:11434"] {
            let resolved = Anthropic::resolve_base_url(Some(url.to_string()), None)
                .unwrap_or_else(|e| panic!("loopback url {url} should be accepted: {e}"));
            assert_eq!(resolved, url);
        }
    }

    #[test]
    fn env_base_url_rejects_cleartext_http() {
        for flag in [None, Some("0"), Some("false"), Some("yes"), Some("")] {
            let err = Anthropic::resolve_base_url(
                Some("http://gateway.internal.example.com".to_string()),
                flag.map(str::to_string),
            )
            .expect_err("a non-loopback http ANTHROPIC_BASE_URL must be rejected");

            assert!(err.is_validation(), "expected a validation error, got {err}");
            let message = err.to_string();
            assert!(
                message.contains("unencrypted"),
                "error should explain the cleartext risk, got: {message}"
            );
            assert!(message.contains("'http'"), "error should name the scheme, got: {message}");
            assert!(
                message.contains("ANTHROPIC_ALLOW_INSECURE_HTTP=1"),
                "error should name the environment opt-in for {flag:?}, got: {message}"
            );
        }
    }

    #[test]
    fn env_base_url_accepts_cleartext_http_when_acknowledged() {
        for flag in ["1", "true", "TRUE", " True "] {
            let resolved = Anthropic::resolve_base_url(
                Some("http://10.60.1.20:8080/api/v1/llm/anthropic".to_string()),
                Some(flag.to_string()),
            )
            .unwrap_or_else(|e| {
                panic!("ANTHROPIC_ALLOW_INSECURE_HTTP={flag:?} should admit http: {e}")
            });
            assert_eq!(resolved, "http://10.60.1.20:8080/api/v1/llm/anthropic");
        }
    }

    #[test]
    fn env_base_url_rejects_non_http_schemes_and_garbage() {
        for url in ["ftp://files.example.com", "ws://gateway.example.com", "not-a-url"] {
            let err = Anthropic::resolve_base_url(Some(url.to_string()), Some("1".to_string()))
                .expect_err("non-https, non-loopback ANTHROPIC_BASE_URL must be rejected");
            assert!(err.is_validation(), "expected a validation error for {url}, got {err}");
        }
    }

    #[test]
    fn with_base_url_and_timeout_rejects_cleartext_http() {
        let err = Anthropic::new(Some("placeholder-api-key".to_string()))
            .unwrap()
            .with_base_url_and_timeout(
                "http://gateway.internal.example.com".to_string(),
                Duration::from_secs(5),
            )
            .expect_err("a non-loopback http base URL must be rejected");
        assert!(err.is_validation(), "expected a validation error, got {err}");
    }

    #[test]
    fn client_timeout_configuration() {
        let client = Anthropic::new(Some("test_key".to_string())).unwrap();
        let timeout = Duration::from_secs(30);

        let configured_client = client.with_timeout(timeout).unwrap();
        assert_eq!(configured_client.timeout, timeout);
    }

    #[test]
    fn client_cached_headers_performance() {
        let client = Anthropic::new(Some("test_key".to_string())).unwrap();

        // Test that headers are cached and cloning is cheap
        let headers1 = client.default_headers();
        let headers2 = client.default_headers();

        assert_eq!(headers1.len(), headers2.len());
        assert!(headers1.contains_key("x-api-key"));
        assert!(headers1.contains_key("anthropic-version"));
        assert!(headers1.contains_key("content-type"));
    }

    #[test]
    fn request_error_mapping() {
        let client = Anthropic::new(Some("test_key".to_string())).unwrap();

        // Test different types of reqwest errors are mapped correctly
        // Note: These are unit tests for the mapping logic, not integration tests
        let _timeout = Duration::from_secs(30);
        assert_eq!(client.timeout, DEFAULT_TIMEOUT); // Should use default initially
    }

    #[tokio::test]
    async fn concurrent_retry_safety() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::spawn;

        let client = Anthropic {
            api_key: "test".to_string(),
            client: ReqwestClient::new(),
            stream_client: ReqwestClient::new(),
            base_url: "http://localhost".to_string(),
            timeout: Duration::from_secs(1),
            stream_timeout: None,
            max_retries: 1,
            throughput_ops_sec: 1.0,
            reserve_capacity: 1.0,
            cached_headers: Arc::new(HeaderMap::new()),
            allow_insecure_http: false,
        };

        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        // Spawn multiple concurrent retry operations
        for _ in 0..3 {
            let client_clone = client.clone();
            let counter_clone = attempt_counter.clone();

            let handle = spawn(async move {
                client_clone
                    .retry_with_backoff(|| {
                        let counter = counter_clone.clone();
                        async move {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok::<String, Error>("success".to_string())
                        }
                    })
                    .await
            });
            handles.push(handle);
        }

        // Wait for all operations to complete
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }

        // Verify all operations executed
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 3);
    }
}
