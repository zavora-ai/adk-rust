//! Span helpers for common ADK operations
//!
//! Provides pre-configured spans for instrumenting agent, model, and tool operations.

use tracing::Span;

/// Create a span for agent execution
///
/// # Arguments
/// * `agent_name` - Name of the agent being executed
/// * `invocation_id` - Unique ID for this invocation
///
/// # Example
/// ```
/// use adk_telemetry::agent_run_span;
/// let span = agent_run_span("my-agent", "inv-123");
/// let _enter = span.enter();
/// // Agent execution code here
/// ```
pub fn agent_run_span(agent_name: &str, invocation_id: &str) -> Span {
    tracing::info_span!(
        "agent.run",
        agent.name = agent_name,
        invocation.id = invocation_id,
        otel.kind = "internal"
    )
}

/// Create a span for model API calls
///
/// # Arguments
/// * `model_name` - Name of the LLM model being called
///
/// # Example
/// ```
/// use adk_telemetry::model_call_span;
/// let span = model_call_span("gemini-2.5-flash");
/// let _enter = span.enter();
/// // Model call code here
/// ```
pub fn model_call_span(model_name: &str) -> Span {
    tracing::info_span!("model.call", model.name = model_name, otel.kind = "client")
}

/// Create a span for LLM generate_content calls with pre-declared token usage fields.
///
/// This span follows [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/)
/// and pre-declares all `gen_ai.*` fields so they can be recorded after the response arrives.
///
/// # Arguments
/// * `provider` - Provider name (e.g., "gemini", "openai", "anthropic")
/// * `model_name` - Model identifier (e.g., "gemini-2.5-flash", "gpt-5-mini")
/// * `stream` - Whether this is a streaming request
///
/// # Example
/// ```
/// use adk_telemetry::llm_generate_span;
/// let span = llm_generate_span("openai", "gpt-5-mini", true);
/// let _enter = span.enter();
/// // After response: adk_telemetry::record_llm_usage(&usage_metadata);
/// ```
pub fn llm_generate_span(provider: &str, model_name: &str, stream: bool) -> Span {
    tracing::info_span!(
        "gen_ai.generate",
        gen_ai.system = %provider,
        gen_ai.request.model = %model_name,
        gen_ai.request.stream = stream,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
        gen_ai.usage.total_tokens = tracing::field::Empty,
        gen_ai.usage.cache_read_tokens = tracing::field::Empty,
        gen_ai.usage.cache_creation_tokens = tracing::field::Empty,
        gen_ai.usage.thinking_tokens = tracing::field::Empty,
        gen_ai.usage.audio_input_tokens = tracing::field::Empty,
        gen_ai.usage.audio_output_tokens = tracing::field::Empty,
        otel.kind = "client",
    )
}

/// Token usage data for recording on tracing spans.
///
/// Mirrors the token count fields from `adk_core::UsageMetadata` without
/// depending on `adk-core`. Callers in `adk-model` convert from `UsageMetadata`
/// to this struct before recording.
///
/// # Example
/// ```
/// use adk_telemetry::LlmUsage;
/// let usage = LlmUsage {
///     input_tokens: 100,
///     output_tokens: 50,
///     total_tokens: 150,
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct LlmUsage {
    /// Prompt / input token count.
    pub input_tokens: i32,
    /// Completion / output token count.
    pub output_tokens: i32,
    /// Total token count.
    pub total_tokens: i32,
    /// Tokens read from cache.
    pub cache_read_tokens: Option<i32>,
    /// Tokens used to create cache.
    pub cache_creation_tokens: Option<i32>,
    /// Tokens used for chain-of-thought reasoning.
    pub thinking_tokens: Option<i32>,
    /// Audio input token count.
    pub audio_input_tokens: Option<i32>,
    /// Audio output token count.
    pub audio_output_tokens: Option<i32>,
}

/// Record LLM token usage on the current span.
///
/// Call this after receiving the final `LlmResponse` to populate the
/// `gen_ai.usage.*` fields declared by [`llm_generate_span`].
///
/// Fields are only recorded when present (non-zero / Some). This is safe to call
/// even if the current span was not created by `llm_generate_span` — unknown
/// fields are silently ignored by `tracing`.
///
/// # Example
/// ```
/// use adk_telemetry::{LlmUsage, record_llm_usage};
/// let usage = LlmUsage {
///     input_tokens: 100,
///     output_tokens: 50,
///     total_tokens: 150,
///     cache_read_tokens: Some(80),
///     ..Default::default()
/// };
/// record_llm_usage(&usage);
/// ```
pub fn record_llm_usage(usage: &LlmUsage) {
    let span = Span::current();
    span.record("gen_ai.usage.input_tokens", i64::from(usage.input_tokens));
    span.record("gen_ai.usage.output_tokens", i64::from(usage.output_tokens));
    span.record("gen_ai.usage.total_tokens", i64::from(usage.total_tokens));
    if let Some(v) = usage.cache_read_tokens {
        span.record("gen_ai.usage.cache_read_tokens", i64::from(v));
    }
    if let Some(v) = usage.cache_creation_tokens {
        span.record("gen_ai.usage.cache_creation_tokens", i64::from(v));
    }
    if let Some(v) = usage.thinking_tokens {
        span.record("gen_ai.usage.thinking_tokens", i64::from(v));
    }
    if let Some(v) = usage.audio_input_tokens {
        span.record("gen_ai.usage.audio_input_tokens", i64::from(v));
    }
    if let Some(v) = usage.audio_output_tokens {
        span.record("gen_ai.usage.audio_output_tokens", i64::from(v));
    }
}

/// Create a span for tool execution
///
/// # Arguments
/// * `tool_name` - Name of the tool being executed
///
/// # Example
/// ```
/// use adk_telemetry::tool_execute_span;
/// let span = tool_execute_span("weather_tool");
/// let _enter = span.enter();
/// // Tool execution code here
/// ```
pub fn tool_execute_span(tool_name: &str) -> Span {
    tracing::info_span!("tool.execute", tool.name = tool_name, otel.kind = "internal")
}

/// Create a span for callback execution
///
/// # Arguments
/// * `callback_type` - Type of callback (e.g., "before_model", "after_agent")
///
/// # Example
/// ```
/// use adk_telemetry::callback_span;
/// let span = callback_span("before_model");
/// let _enter = span.enter();
/// // Callback code here
/// ```
pub fn callback_span(callback_type: &str) -> Span {
    tracing::debug_span!(
        "callback",
        callback.type = callback_type,
    )
}

/// Add common attributes to the current span
///
/// # Arguments
/// * `user_id` - User ID from context
/// * `session_id` - Session ID from context
pub fn add_context_attributes(user_id: &str, session_id: &str) {
    let span = Span::current();
    span.record("user.id", user_id);
    span.record("session.id", session_id);
}
