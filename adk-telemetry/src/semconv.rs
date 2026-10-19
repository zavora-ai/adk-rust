//! OpenTelemetry GenAI Semantic Convention attribute constants and helpers.
//!
//! Based on OTel Semantic Conventions v1.41.0:
//! <https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/>
//!
//! This module provides:
//! - All `gen_ai.*` attribute name constants
//! - [`GenAiProvider`] and [`GenAiOperation`] enums
//! - [`GenAiSpanBuilder`] fluent API for creating model call spans
//! - [`GenAiResponseRecorder`] for recording response attributes
//! - [`map_finish_reason`] for provider-specific finish reason mapping
//! - [`tool_call_semconv_span`] and [`agent_run_semconv_span`] helpers

use tracing::Span;

use crate::LlmUsage;

// --- Provider & Operation ---

/// The `gen_ai.system` attribute (legacy, kept for backward compatibility).
pub const GEN_AI_SYSTEM: &str = "gen_ai.system";

/// The `gen_ai.provider.name` attribute (v1.41.0+).
pub const GEN_AI_PROVIDER_NAME: &str = "gen_ai.provider.name";

/// The `gen_ai.operation.name` attribute.
pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";

// --- Request Attributes ---

/// The `gen_ai.request.model` attribute.
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";

/// The `gen_ai.request.max_tokens` attribute.
pub const GEN_AI_REQUEST_MAX_TOKENS: &str = "gen_ai.request.max_tokens";

/// The `gen_ai.request.temperature` attribute.
pub const GEN_AI_REQUEST_TEMPERATURE: &str = "gen_ai.request.temperature";

/// The `gen_ai.request.top_p` attribute.
pub const GEN_AI_REQUEST_TOP_P: &str = "gen_ai.request.top_p";

/// The `gen_ai.request.top_k` attribute.
pub const GEN_AI_REQUEST_TOP_K: &str = "gen_ai.request.top_k";

/// The `gen_ai.request.stream` attribute.
pub const GEN_AI_REQUEST_STREAM: &str = "gen_ai.request.stream";

/// The `gen_ai.request.frequency_penalty` attribute.
pub const GEN_AI_REQUEST_FREQUENCY_PENALTY: &str = "gen_ai.request.frequency_penalty";

/// The `gen_ai.request.presence_penalty` attribute.
pub const GEN_AI_REQUEST_PRESENCE_PENALTY: &str = "gen_ai.request.presence_penalty";

// --- Response Attributes ---

/// The `gen_ai.response.model` attribute.
pub const GEN_AI_RESPONSE_MODEL: &str = "gen_ai.response.model";

/// The `gen_ai.response.finish_reasons` attribute.
pub const GEN_AI_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";

/// The `gen_ai.response.id` attribute.
pub const GEN_AI_RESPONSE_ID: &str = "gen_ai.response.id";

// --- Token Usage ---

/// The `gen_ai.usage.input_tokens` attribute.
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";

/// The `gen_ai.usage.output_tokens` attribute.
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

/// The `gen_ai.usage.total_tokens` attribute.
pub const GEN_AI_USAGE_TOTAL_TOKENS: &str = "gen_ai.usage.total_tokens";

/// The `gen_ai.usage.cache_read_tokens` attribute.
pub const GEN_AI_USAGE_CACHE_READ_TOKENS: &str = "gen_ai.usage.cache_read_tokens";

/// The `gen_ai.usage.cache_creation_tokens` attribute.
pub const GEN_AI_USAGE_CACHE_CREATION_TOKENS: &str = "gen_ai.usage.cache_creation_tokens";

/// The `gen_ai.usage.thinking_tokens` attribute.
pub const GEN_AI_USAGE_THINKING_TOKENS: &str = "gen_ai.usage.thinking_tokens";

// --- Conversation ---

/// The `gen_ai.conversation.id` attribute.
pub const GEN_AI_CONVERSATION_ID: &str = "gen_ai.conversation.id";

// --- Tool Attributes ---

/// The `gen_ai.tool.name` attribute.
pub const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";

/// The `gen_ai.tool.call_id` attribute.
pub const GEN_AI_TOOL_CALL_ID: &str = "gen_ai.tool.call_id";

// --- Content Events ---

/// The `gen_ai.content.prompt` event name.
pub const GEN_AI_CONTENT_PROMPT: &str = "gen_ai.content.prompt";

/// The `gen_ai.content.completion` event name.
pub const GEN_AI_CONTENT_COMPLETION: &str = "gen_ai.content.completion";

// =============================================================================
// Enums
// =============================================================================

/// Well-known GenAI provider identifiers per OTel semconv registry.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::GenAiProvider;
/// assert_eq!(GenAiProvider::Gemini.as_str(), "gcp.gemini");
/// assert_eq!(GenAiProvider::OpenAI.as_str(), "openai");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenAiProvider {
    /// Google Gemini (via AI Studio or Vertex AI).
    Gemini,
    /// OpenAI.
    OpenAI,
    /// Anthropic.
    Anthropic,
    /// DeepSeek.
    DeepSeek,
    /// Groq.
    Groq,
    /// Ollama (local).
    Ollama,
    /// Azure OpenAI Service.
    AzureOpenAI,
    /// Azure AI Inference.
    AzureAiInference,
    /// AWS Bedrock.
    AwsBedrock,
    /// Mistral AI.
    MistralAi,
    /// Perplexity.
    Perplexity,
    /// xAI (Grok).
    XAi,
}

impl GenAiProvider {
    /// Returns the OTel semconv `gen_ai.provider.name` string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gemini => "gcp.gemini",
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::DeepSeek => "deepseek",
            Self::Groq => "groq",
            Self::Ollama => "ollama",
            Self::AzureOpenAI => "azure.ai.openai",
            Self::AzureAiInference => "azure.ai.inference",
            Self::AwsBedrock => "aws.bedrock",
            Self::MistralAi => "mistral_ai",
            Self::Perplexity => "perplexity",
            Self::XAi => "x_ai",
        }
    }
}

/// Well-known GenAI operation names per OTel semconv.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::GenAiOperation;
/// assert_eq!(GenAiOperation::Chat.as_str(), "chat");
/// assert_eq!(GenAiOperation::Embeddings.as_str(), "embeddings");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenAiOperation {
    /// Chat completion.
    Chat,
    /// Generate content (Gemini-style).
    GenerateContent,
    /// Text completion (legacy).
    TextCompletion,
    /// Embedding generation.
    Embeddings,
    /// Tool execution.
    ExecuteTool,
    /// Agent invocation.
    InvokeAgent,
}

impl GenAiOperation {
    /// Returns the OTel semconv operation name string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::GenerateContent => "generate_content",
            Self::TextCompletion => "text_completion",
            Self::Embeddings => "embeddings",
            Self::ExecuteTool => "execute_tool",
            Self::InvokeAgent => "invoke_agent",
        }
    }
}

// =============================================================================
// GenAiSpanBuilder
// =============================================================================

/// Builder for creating model call spans with full OTel GenAI semconv attributes.
///
/// Providers use this to construct spans with all available metadata.
/// Fields not set are omitted from the span (recorded as `Empty`).
///
/// # Example
/// ```
/// use adk_telemetry::semconv::{GenAiSpanBuilder, GenAiProvider, GenAiOperation};
///
/// let span = GenAiSpanBuilder::new(GenAiProvider::Gemini, GenAiOperation::Chat, "gemini-3.7-flash")
///     .stream(true)
///     .temperature(0.7)
///     .max_tokens(4096)
///     .build();
/// ```
pub struct GenAiSpanBuilder {
    provider: GenAiProvider,
    operation: GenAiOperation,
    model: String,
    stream: bool,
    temperature: Option<f64>,
    max_tokens: Option<i64>,
    top_p: Option<f64>,
    top_k: Option<f64>,
    conversation_id: Option<String>,
}

impl GenAiSpanBuilder {
    /// Create a new span builder with required fields.
    pub fn new(
        provider: GenAiProvider,
        operation: GenAiOperation,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            operation,
            model: model.into(),
            stream: false,
            temperature: None,
            max_tokens: None,
            top_p: None,
            top_k: None,
            conversation_id: None,
        }
    }

    /// Set whether this is a streaming request.
    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Set the temperature parameter.
    pub fn temperature(mut self, temp: f64) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// Set the max tokens parameter.
    pub fn max_tokens(mut self, max: i64) -> Self {
        self.max_tokens = Some(max);
        self
    }

    /// Set the top_p parameter.
    pub fn top_p(mut self, p: f64) -> Self {
        self.top_p = Some(p);
        self
    }

    /// Set the top_k parameter.
    pub fn top_k(mut self, k: f64) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Set the conversation/session ID for correlation.
    pub fn conversation_id(mut self, id: impl Into<String>) -> Self {
        self.conversation_id = Some(id.into());
        self
    }

    /// Build and return the tracing [`Span`].
    ///
    /// The span name follows the pattern: `gen_ai.{operation_name} {model}`.
    /// All `gen_ai.*` response and usage fields are pre-declared as `Empty`
    /// so they can be recorded later via [`GenAiResponseRecorder`].
    pub fn build(self) -> Span {
        let span_name = format!("gen_ai.{} {}", self.operation.as_str(), self.model);
        let provider_str = self.provider.as_str();
        let operation_str = self.operation.as_str();

        let span = tracing::info_span!(
            "gen_ai.call",
            "otel.name" = %span_name,
            "gen_ai.system" = %provider_str,
            "gen_ai.provider.name" = %provider_str,
            "gen_ai.operation.name" = %operation_str,
            "gen_ai.request.model" = %self.model,
            "gen_ai.request.stream" = self.stream,
            "gen_ai.request.temperature" = tracing::field::Empty,
            "gen_ai.request.max_tokens" = tracing::field::Empty,
            "gen_ai.request.top_p" = tracing::field::Empty,
            "gen_ai.request.top_k" = tracing::field::Empty,
            "gen_ai.conversation.id" = tracing::field::Empty,
            "gen_ai.response.model" = tracing::field::Empty,
            "gen_ai.response.finish_reasons" = tracing::field::Empty,
            "gen_ai.usage.input_tokens" = tracing::field::Empty,
            "gen_ai.usage.output_tokens" = tracing::field::Empty,
            "gen_ai.usage.total_tokens" = tracing::field::Empty,
            "gen_ai.usage.cache_read_tokens" = tracing::field::Empty,
            "gen_ai.usage.cache_creation_tokens" = tracing::field::Empty,
            "gen_ai.usage.thinking_tokens" = tracing::field::Empty,
            "otel.kind" = "client",
        );

        // Record optional fields that were set
        if let Some(temp) = self.temperature {
            span.record("gen_ai.request.temperature", temp);
        }
        if let Some(max) = self.max_tokens {
            span.record("gen_ai.request.max_tokens", max);
        }
        if let Some(p) = self.top_p {
            span.record("gen_ai.request.top_p", p);
        }
        if let Some(k) = self.top_k {
            span.record("gen_ai.request.top_k", k);
        }
        if let Some(ref conv_id) = self.conversation_id {
            span.record("gen_ai.conversation.id", conv_id.as_str());
        }

        span
    }
}

// =============================================================================
// GenAiResponseRecorder
// =============================================================================

/// Records response-time attributes on the current span.
///
/// Call after receiving the model response to populate response model,
/// finish reasons, and token usage.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::GenAiResponseRecorder;
/// use adk_telemetry::LlmUsage;
///
/// // After receiving model response:
/// GenAiResponseRecorder::record_response_model("gemini-3.7-flash");
/// GenAiResponseRecorder::record_finish_reasons(&["stop"]);
/// GenAiResponseRecorder::record_usage(&LlmUsage {
///     input_tokens: 100,
///     output_tokens: 50,
///     total_tokens: 150,
///     ..Default::default()
/// });
/// ```
pub struct GenAiResponseRecorder;

impl GenAiResponseRecorder {
    /// Record the response model (may differ from request model).
    pub fn record_response_model(model: &str) {
        Span::current().record("gen_ai.response.model", model);
    }

    /// Record finish reasons as a comma-separated string.
    ///
    /// OTel semconv specifies this as a string array; tracing encodes as CSV.
    pub fn record_finish_reasons(reasons: &[&str]) {
        let joined = reasons.join(",");
        Span::current().record("gen_ai.response.finish_reasons", joined.as_str());
    }

    /// Record token usage (delegates to existing [`record_llm_usage`](crate::record_llm_usage)).
    pub fn record_usage(usage: &LlmUsage) {
        crate::record_llm_usage(usage);
    }
}

// =============================================================================
// Finish Reason Mapping
// =============================================================================

/// Maps provider-specific finish reason strings to OTel semconv values.
///
/// Known mappings are converted; unknown values pass through unchanged.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::{map_finish_reason, GenAiProvider};
///
/// assert_eq!(map_finish_reason(GenAiProvider::Gemini, "STOP"), "stop");
/// assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "length"), "max_tokens");
/// assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "end_turn"), "stop");
/// assert_eq!(map_finish_reason(GenAiProvider::Gemini, "UNKNOWN"), "UNKNOWN");
/// ```
pub fn map_finish_reason(provider: GenAiProvider, raw: &str) -> &str {
    match provider {
        GenAiProvider::Gemini => match raw {
            "STOP" => "stop",
            "MAX_TOKENS" => "max_tokens",
            "SAFETY" => "content_filter",
            _ => raw,
        },
        GenAiProvider::OpenAI | GenAiProvider::AzureOpenAI => match raw {
            "stop" => "stop",
            "length" => "max_tokens",
            "tool_calls" => "tool_calls",
            "content_filter" => "content_filter",
            _ => raw,
        },
        GenAiProvider::Anthropic => match raw {
            "end_turn" => "stop",
            "max_tokens" => "max_tokens",
            "tool_use" => "tool_calls",
            _ => raw,
        },
        _ => raw,
    }
}

// =============================================================================
// Tool Call Span
// =============================================================================

/// Create a span for tool execution with GenAI semconv attributes.
///
/// Emits `gen_ai.tool.name` always and `gen_ai.tool.call_id` when provided.
/// Pre-declares `gen_ai.conversation.id` and `gen_ai.system` as `Empty` for
/// propagation from parent spans via [`AdkSpanLayer`](crate::AdkSpanLayer).
///
/// # Example
/// ```
/// use adk_telemetry::semconv::tool_call_semconv_span;
///
/// let span = tool_call_semconv_span("weather_tool", Some("call_abc123"));
/// let _enter = span.enter();
/// ```
pub fn tool_call_semconv_span(tool_name: &str, call_id: Option<&str>) -> Span {
    let span = tracing::info_span!(
        "execute_tool",
        "gen_ai.tool.name" = %tool_name,
        "gen_ai.tool.call_id" = tracing::field::Empty,
        "gen_ai.conversation.id" = tracing::field::Empty,
        "gen_ai.system" = tracing::field::Empty,
        "gen_ai.provider.name" = tracing::field::Empty,
        "otel.kind" = "internal",
    );

    if let Some(id) = call_id {
        span.record("gen_ai.tool.call_id", id);
    }

    span
}

// =============================================================================
// Agent Run Span
// =============================================================================

/// Create a span for agent execution with conversation ID.
///
/// Sets `gen_ai.conversation.id` to `session_id` when provided, which
/// propagates to all child spans via [`AdkSpanLayer`](crate::AdkSpanLayer).
///
/// # Example
/// ```
/// use adk_telemetry::semconv::agent_run_semconv_span;
///
/// let span = agent_run_semconv_span("my-agent", "inv-123", Some("session-456"));
/// let _enter = span.enter();
/// ```
pub fn agent_run_semconv_span(
    agent_name: &str,
    invocation_id: &str,
    session_id: Option<&str>,
) -> Span {
    let span = tracing::info_span!(
        "agent.execute",
        "agent.name" = %agent_name,
        "invocation.id" = %invocation_id,
        "gen_ai.conversation.id" = tracing::field::Empty,
        "otel.kind" = "internal",
    );

    if let Some(sid) = session_id {
        span.record("gen_ai.conversation.id", sid);
    }

    span
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// Helper to run test code with a subscriber so spans are not disabled.
    fn with_subscriber(f: impl FnOnce()) {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_test_writer());
        tracing::subscriber::with_default(subscriber, f);
    }

    #[test]
    fn test_provider_as_str() {
        assert_eq!(GenAiProvider::Gemini.as_str(), "gcp.gemini");
        assert_eq!(GenAiProvider::OpenAI.as_str(), "openai");
        assert_eq!(GenAiProvider::Anthropic.as_str(), "anthropic");
        assert_eq!(GenAiProvider::DeepSeek.as_str(), "deepseek");
        assert_eq!(GenAiProvider::Groq.as_str(), "groq");
        assert_eq!(GenAiProvider::Ollama.as_str(), "ollama");
        assert_eq!(GenAiProvider::AzureOpenAI.as_str(), "azure.ai.openai");
        assert_eq!(GenAiProvider::AzureAiInference.as_str(), "azure.ai.inference");
        assert_eq!(GenAiProvider::AwsBedrock.as_str(), "aws.bedrock");
        assert_eq!(GenAiProvider::MistralAi.as_str(), "mistral_ai");
        assert_eq!(GenAiProvider::Perplexity.as_str(), "perplexity");
        assert_eq!(GenAiProvider::XAi.as_str(), "x_ai");
    }

    #[test]
    fn test_operation_as_str() {
        assert_eq!(GenAiOperation::Chat.as_str(), "chat");
        assert_eq!(GenAiOperation::GenerateContent.as_str(), "generate_content");
        assert_eq!(GenAiOperation::TextCompletion.as_str(), "text_completion");
        assert_eq!(GenAiOperation::Embeddings.as_str(), "embeddings");
        assert_eq!(GenAiOperation::ExecuteTool.as_str(), "execute_tool");
        assert_eq!(GenAiOperation::InvokeAgent.as_str(), "invoke_agent");
    }

    #[test]
    fn test_map_finish_reason_gemini() {
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "STOP"), "stop");
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "MAX_TOKENS"), "max_tokens");
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "SAFETY"), "content_filter");
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "UNKNOWN_REASON"), "UNKNOWN_REASON");
    }

    #[test]
    fn test_map_finish_reason_openai() {
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "stop"), "stop");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "length"), "max_tokens");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "tool_calls"), "tool_calls");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "content_filter"), "content_filter");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "other"), "other");
    }

    #[test]
    fn test_map_finish_reason_anthropic() {
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "end_turn"), "stop");
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "max_tokens"), "max_tokens");
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "tool_use"), "tool_calls");
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "stop_sequence"), "stop_sequence");
    }

    #[test]
    fn test_map_finish_reason_unknown_provider_passthrough() {
        assert_eq!(map_finish_reason(GenAiProvider::Ollama, "done"), "done");
        assert_eq!(map_finish_reason(GenAiProvider::DeepSeek, "stop"), "stop");
    }

    #[test]
    fn test_span_builder_creates_span() {
        with_subscriber(|| {
            let span = GenAiSpanBuilder::new(
                GenAiProvider::Gemini,
                GenAiOperation::Chat,
                "gemini-2.5-flash",
            )
            .stream(true)
            .temperature(0.7)
            .max_tokens(4096)
            .conversation_id("session-123")
            .build();

            assert!(!span.is_disabled());
        });
    }

    #[test]
    fn test_tool_call_semconv_span_with_call_id() {
        with_subscriber(|| {
            let span = tool_call_semconv_span("weather_tool", Some("call_abc"));
            assert!(!span.is_disabled());
        });
    }

    #[test]
    fn test_tool_call_semconv_span_without_call_id() {
        with_subscriber(|| {
            let span = tool_call_semconv_span("weather_tool", None);
            assert!(!span.is_disabled());
        });
    }

    #[test]
    fn test_agent_run_semconv_span_with_session() {
        with_subscriber(|| {
            let span = agent_run_semconv_span("my-agent", "inv-1", Some("session-1"));
            assert!(!span.is_disabled());
        });
    }

    #[test]
    fn test_agent_run_semconv_span_without_session() {
        with_subscriber(|| {
            let span = agent_run_semconv_span("my-agent", "inv-1", None);
            assert!(!span.is_disabled());
        });
    }
}
