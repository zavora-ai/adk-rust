//! Connect and idle timeouts for the HTTP transport of model clients.

use std::time::Duration;

/// Connect and idle timeouts for a model client's HTTP transport.
///
/// | Bound | Covers |
/// |-------|--------|
/// | `connect` | establishing the connection, TLS included |
/// | `idle` | each wait for data: the response headers, then every gap between body chunks of a streamed or complete response |
///
/// A request that gets no response headers within `idle`, a stream that stops sending data
/// for `idle`, and a complete response whose body stalls for `idle` all fail with
/// `ErrorCategory::Timeout` instead of holding the run, as does a connection not established
/// within `connect`; a long stream that keeps sending data is never cut off. The defaults are
/// 30 seconds to connect and 10 minutes idle, the request timeout of the official OpenAI and
/// Anthropic SDKs, which leaves room for reasoning models that think for minutes before
/// their first token. The OpenAI-compatible, OpenAI Responses, Azure OpenAI, Azure AI, Groq,
/// DeepSeek, OpenRouter, Ollama, and Bedrock clients apply them by default and take others
/// through `with_timeouts`. Bedrock applies them as the SDK's connect and read timeouts plus
/// a bound on each stream event; an SDK client passed to `BedrockClient::from_client` keeps
/// its own connect and read timeouts until `with_timeouts` replaces them.
///
/// # Example
///
/// ```
/// use std::time::Duration;
///
/// use adk_model::HttpTimeouts;
///
/// // Fail a stream that sends nothing for two minutes; pass to a client's `with_timeouts`.
/// let timeouts = HttpTimeouts::new(Duration::from_secs(10), Duration::from_secs(120));
/// assert_eq!(timeouts.idle, Duration::from_secs(120));
/// assert_eq!(HttpTimeouts::default(), HttpTimeouts::new(Duration::from_secs(30), Duration::from_secs(600)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpTimeouts {
    /// Bound on establishing a connection.
    pub connect: Duration,
    /// Bound on each wait for response data, headers included.
    pub idle: Duration,
}

impl HttpTimeouts {
    /// Timeouts with the given connect and idle bounds.
    pub const fn new(connect: Duration, idle: Duration) -> Self {
        Self { connect, idle }
    }
}

impl Default for HttpTimeouts {
    fn default() -> Self {
        Self::new(Duration::from_secs(30), Duration::from_secs(600))
    }
}

#[cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "ollama",
    feature = "openrouter"
))]
impl HttpTimeouts {
    /// Applies both bounds to a `reqwest` client builder.
    pub(crate) fn apply(self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        builder.connect_timeout(self.connect).read_timeout(self.idle)
    }
}

/// The category of a transport failure: `Timeout` when a bound expired, before the response
/// headers or between body chunks, otherwise `Unavailable`.
#[cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "openrouter"
))]
pub(crate) fn transport_category(error: &reqwest::Error) -> adk_core::ErrorCategory {
    if error.is_timeout() {
        adk_core::ErrorCategory::Timeout
    } else {
        adk_core::ErrorCategory::Unavailable
    }
}
