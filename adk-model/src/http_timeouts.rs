//! Connect and idle timeouts for the HTTP transport of model clients.

use std::time::Duration;

/// Connect and idle timeouts for a model client's HTTP transport.
///
/// | Bound | Covers |
/// |-------|--------|
/// | `connect` | establishing the connection, TLS included |
/// | `idle` | each wait for data: the response headers, then every gap between body chunks of a streamed or complete response |
///
/// A stream that stops sending data fails after `idle` with a `Timeout` error instead of
/// holding the run, while a long stream that keeps sending data is never cut off. The
/// defaults are 30 seconds to connect and 10 minutes idle, the request timeout of the
/// official OpenAI and Anthropic SDKs, which leaves room for reasoning models that think for
/// minutes before their first token. The OpenAI-compatible, OpenAI Responses, Azure OpenAI,
/// Azure AI, Groq, DeepSeek, OpenRouter, Ollama, and Bedrock clients apply them by default
/// and take others through `with_timeouts`.
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
