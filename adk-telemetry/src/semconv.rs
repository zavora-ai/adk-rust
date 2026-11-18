//! OpenTelemetry GenAI semantic convention attribute constants and helpers.
//!
//! Follows the GenAI conventions of the
//! [`semantic-conventions-genai`](https://github.com/open-telemetry/semantic-conventions-genai)
//! repository, where the conventions moved in core semantic conventions v1.42.0. That
//! repository has no tagged release yet; these names follow its `main` branch as of
//! 2026-11-18 (schema `https://opentelemetry.io/schemas/gen-ai-dev/1.42.0-dev`), and
//! [`SCHEMA_URL`] names it. Core v1.41.0 was the last release that defined them itself.
//!
//! | Span | Name | Kind |
//! |---|---|---|
//! | Inference ([`crate::llm_generate_span`], [`GenAiSpanBuilder`]) | `{gen_ai.operation.name} {gen_ai.request.model}` | `client` |
//! | Agent invocation ([`agent_run_semconv_span`]) | `invoke_agent {gen_ai.agent.name}` | `internal` |
//! | Tool execution ([`tool_call_semconv_span`]) | `execute_tool {gen_ai.tool.name}` | `internal` |
//!
//! The conventions type the content attributes (`gen_ai.input.messages` and the rest) as
//! structured values; the OpenTelemetry Rust span API (0.32) takes no map-valued attribute,
//! so they are recorded as JSON strings in the conventions' message schema.

use tracing::Span;

use crate::LlmUsage;

/// The schema URL of the conventions this module follows.
pub const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/gen-ai-dev/1.42.0-dev";

// --- Provider & Operation ---

/// The `gen_ai.provider.name` attribute.
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

/// The `gen_ai.request.top_k` attribute, an integer.
pub const GEN_AI_REQUEST_TOP_K: &str = "gen_ai.request.top_k";

/// The `gen_ai.request.stream` attribute, set only when the request streams.
pub const GEN_AI_REQUEST_STREAM: &str = "gen_ai.request.stream";

/// The `gen_ai.request.frequency_penalty` attribute.
pub const GEN_AI_REQUEST_FREQUENCY_PENALTY: &str = "gen_ai.request.frequency_penalty";

/// The `gen_ai.request.presence_penalty` attribute.
pub const GEN_AI_REQUEST_PRESENCE_PENALTY: &str = "gen_ai.request.presence_penalty";

/// The `gen_ai.request.seed` attribute.
pub const GEN_AI_REQUEST_SEED: &str = "gen_ai.request.seed";

/// The `gen_ai.request.stop_sequences` attribute, a string array.
pub const GEN_AI_REQUEST_STOP_SEQUENCES: &str = "gen_ai.request.stop_sequences";

/// The `gen_ai.output.type` attribute: `text`, `json`, `image` or `speech`.
pub const GEN_AI_OUTPUT_TYPE: &str = "gen_ai.output.type";

// --- Response Attributes ---

/// The `gen_ai.response.model` attribute.
pub const GEN_AI_RESPONSE_MODEL: &str = "gen_ai.response.model";

/// The `gen_ai.response.finish_reasons` attribute, a string array.
pub const GEN_AI_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";

/// The `gen_ai.response.id` attribute.
pub const GEN_AI_RESPONSE_ID: &str = "gen_ai.response.id";

// --- Token Usage ---

/// The `gen_ai.usage.input_tokens` attribute: every input token, cached ones included.
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";

/// The `gen_ai.usage.output_tokens` attribute: every output token, reasoning included.
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

/// The `gen_ai.usage.cache_read.input_tokens` attribute.
pub const GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS: &str = "gen_ai.usage.cache_read.input_tokens";

/// The `gen_ai.usage.cache_write.input_tokens` attribute.
pub const GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS: &str = "gen_ai.usage.cache_write.input_tokens";

/// The `gen_ai.usage.reasoning.output_tokens` attribute.
pub const GEN_AI_USAGE_REASONING_OUTPUT_TOKENS: &str = "gen_ai.usage.reasoning.output_tokens";

// --- Conversation and content (opt-in) ---

/// The `gen_ai.conversation.id` attribute.
pub const GEN_AI_CONVERSATION_ID: &str = "gen_ai.conversation.id";

/// The opt-in `gen_ai.system_instructions` attribute.
pub const GEN_AI_SYSTEM_INSTRUCTIONS: &str = "gen_ai.system_instructions";

/// The opt-in `gen_ai.input.messages` attribute.
pub const GEN_AI_INPUT_MESSAGES: &str = "gen_ai.input.messages";

/// The opt-in `gen_ai.output.messages` attribute.
pub const GEN_AI_OUTPUT_MESSAGES: &str = "gen_ai.output.messages";

/// The opt-in `gen_ai.tool.definitions` attribute.
pub const GEN_AI_TOOL_DEFINITIONS: &str = "gen_ai.tool.definitions";

// --- Agent Attributes ---

/// The `gen_ai.agent.name` attribute.
pub const GEN_AI_AGENT_NAME: &str = "gen_ai.agent.name";

/// The `gen_ai.agent.description` attribute.
pub const GEN_AI_AGENT_DESCRIPTION: &str = "gen_ai.agent.description";

// --- Tool Attributes ---

/// The `gen_ai.tool.name` attribute.
pub const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";

/// The `gen_ai.tool.call.id` attribute.
pub const GEN_AI_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";

/// The `gen_ai.tool.type` attribute: `function`, `extension` or `datastore`.
pub const GEN_AI_TOOL_TYPE: &str = "gen_ai.tool.type";

/// The `gen_ai.tool.description` attribute.
pub const GEN_AI_TOOL_DESCRIPTION: &str = "gen_ai.tool.description";

/// The opt-in `gen_ai.tool.call.arguments` attribute.
pub const GEN_AI_TOOL_CALL_ARGUMENTS: &str = "gen_ai.tool.call.arguments";

/// The opt-in `gen_ai.tool.call.result` attribute, recorded only for a call that succeeded.
pub const GEN_AI_TOOL_CALL_RESULT: &str = "gen_ai.tool.call.result";

// --- Events and errors ---

/// The event that carries an inference's content apart from the trace.
pub const GEN_AI_INFERENCE_DETAILS_EVENT: &str = "gen_ai.client.inference.operation.details";

/// The `error.type` attribute.
pub const ERROR_TYPE: &str = "error.type";

// =============================================================================
// Enums
// =============================================================================

/// Well-known `gen_ai.provider.name` values.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::GenAiProvider;
/// assert_eq!(GenAiProvider::Gemini.as_str(), "gcp.gemini");
/// assert_eq!(GenAiProvider::MistralAi.as_str(), "mistral");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GenAiProvider {
    /// Google Gemini API (AI Studio).
    Gemini,
    /// Gemini on Vertex AI.
    VertexAi,
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
    /// Cohere.
    Cohere,
    /// IBM watsonx.ai.
    IbmWatsonx,
    /// Moonshot AI.
    MoonshotAi,
}

impl GenAiProvider {
    /// Returns the `gen_ai.provider.name` string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gemini => "gcp.gemini",
            Self::VertexAi => "gcp.vertex_ai",
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::DeepSeek => "deepseek",
            Self::Groq => "groq",
            Self::Ollama => "ollama",
            Self::AzureOpenAI => "azure.ai.openai",
            Self::AzureAiInference => "azure.ai.inference",
            Self::AwsBedrock => "aws.bedrock",
            Self::MistralAi => "mistral",
            Self::Perplexity => "perplexity",
            Self::XAi => "x_ai",
            Self::Cohere => "cohere",
            Self::IbmWatsonx => "ibm.watsonx.ai",
            Self::MoonshotAi => "moonshot_ai",
        }
    }
}

/// Well-known `gen_ai.operation.name` values ADK-Rust emits.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::GenAiOperation;
/// assert_eq!(GenAiOperation::Chat.as_str(), "chat");
/// assert_eq!(GenAiOperation::InvokeAgent.as_str(), "invoke_agent");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
    /// Agent creation.
    CreateAgent,
    /// Retrieval.
    Retrieval,
    /// Realtime inference.
    RealtimeInference,
}

impl GenAiOperation {
    /// Returns the `gen_ai.operation.name` string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::GenerateContent => "generate_content",
            Self::TextCompletion => "text_completion",
            Self::Embeddings => "embeddings",
            Self::ExecuteTool => "execute_tool",
            Self::InvokeAgent => "invoke_agent",
            Self::CreateAgent => "create_agent",
            Self::Retrieval => "retrieval",
            Self::RealtimeInference => "realtime_inference",
        }
    }
}

// =============================================================================
// GenAiSpanBuilder
// =============================================================================

/// Builder for an inference span with request parameters set at creation.
///
/// The span is the one [`crate::llm_generate_span`] creates, named
/// `{gen_ai.operation.name} {gen_ai.request.model}` in OpenTelemetry export, kind `client`.
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
    top_k: Option<i64>,
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
    pub fn top_k(mut self, k: i64) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Set the conversation/session ID for correlation.
    pub fn conversation_id(mut self, id: impl Into<String>) -> Self {
        self.conversation_id = Some(id.into());
        self
    }

    /// Build and return the tracing [`Span`].
    pub fn build(self) -> Span {
        let span = tracing::info_span!(
            "gen_ai.call",
            otel.name = %format!("{} {}", self.operation.as_str(), self.model),
            otel.kind = "client",
            gen_ai.operation.name = self.operation.as_str(),
            gen_ai.provider.name = self.provider.as_str(),
            gen_ai.request.model = %self.model,
            gen_ai.request.stream = tracing::field::Empty,
            gen_ai.request.temperature = tracing::field::Empty,
            gen_ai.request.max_tokens = tracing::field::Empty,
            gen_ai.request.top_p = tracing::field::Empty,
            gen_ai.request.top_k = tracing::field::Empty,
            gen_ai.conversation.id = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            gen_ai.response.id = tracing::field::Empty,
            gen_ai.response.finish_reasons = tracing::field::Empty,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
            gen_ai.usage.cache_write.input_tokens = tracing::field::Empty,
            gen_ai.usage.reasoning.output_tokens = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        if self.stream {
            span.record(GEN_AI_REQUEST_STREAM, true);
        }
        if let Some(temp) = self.temperature {
            span.record(GEN_AI_REQUEST_TEMPERATURE, temp);
        }
        if let Some(max) = self.max_tokens {
            span.record(GEN_AI_REQUEST_MAX_TOKENS, max);
        }
        if let Some(p) = self.top_p {
            span.record(GEN_AI_REQUEST_TOP_P, p);
        }
        if let Some(k) = self.top_k {
            span.record(GEN_AI_REQUEST_TOP_K, k);
        }
        if let Some(ref conv_id) = self.conversation_id {
            span.record(GEN_AI_CONVERSATION_ID, conv_id.as_str());
        }
        span
    }
}

// =============================================================================
// GenAiResponseRecorder
// =============================================================================

/// Records response attributes on the current span.
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
        Span::current().record(GEN_AI_RESPONSE_MODEL, model);
    }

    /// Record the finish reasons, a string array ([`crate::record_str_array`]).
    pub fn record_finish_reasons(reasons: &[&str]) {
        crate::record_str_array(&Span::current(), GEN_AI_RESPONSE_FINISH_REASONS, reasons);
    }

    /// Record token usage (delegates to [`record_llm_usage`](crate::record_llm_usage)).
    pub fn record_usage(usage: &LlmUsage) {
        crate::record_llm_usage(usage);
    }
}

// =============================================================================
// Finish Reason Mapping
// =============================================================================

/// Maps provider-specific finish reason strings to the conventions' values: `stop`,
/// `length`, `content_filter`, `tool_call`, `compaction` and `error`.
///
/// Known mappings are converted; unknown values pass through unchanged.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::{map_finish_reason, GenAiProvider};
///
/// assert_eq!(map_finish_reason(GenAiProvider::Gemini, "STOP"), "stop");
/// assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "length"), "length");
/// assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "tool_use"), "tool_call");
/// assert_eq!(map_finish_reason(GenAiProvider::Gemini, "UNKNOWN"), "UNKNOWN");
/// ```
pub fn map_finish_reason(provider: GenAiProvider, raw: &str) -> &str {
    match provider {
        GenAiProvider::Gemini | GenAiProvider::VertexAi => match raw {
            "STOP" => "stop",
            "MAX_TOKENS" => "length",
            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
                "content_filter"
            }
            "MALFORMED_FUNCTION_CALL" => "error",
            _ => raw,
        },
        GenAiProvider::OpenAI | GenAiProvider::AzureOpenAI => match raw {
            "stop" => "stop",
            "length" => "length",
            "tool_calls" | "function_call" => "tool_call",
            "content_filter" => "content_filter",
            _ => raw,
        },
        GenAiProvider::Anthropic => match raw {
            "end_turn" | "stop_sequence" => "stop",
            "max_tokens" | "model_context_window_exceeded" => "length",
            "tool_use" => "tool_call",
            "refusal" => "content_filter",
            "compaction" => "compaction",
            _ => raw,
        },
        _ => raw,
    }
}

// =============================================================================
// Tool Call Span
// =============================================================================

/// Creates the span of one tool execution ([`crate::tool_execute_span`]), with
/// `gen_ai.tool.call.id` when the model supplied one.
///
/// # Example
/// ```
/// use adk_telemetry::semconv::tool_call_semconv_span;
///
/// let span = tool_call_semconv_span("weather_tool", Some("call_abc123"));
/// let _enter = span.enter();
/// ```
pub fn tool_call_semconv_span(tool_name: &str, call_id: Option<&str>) -> Span {
    let span = crate::tool_execute_span(tool_name);
    if let Some(id) = call_id {
        span.record(GEN_AI_TOOL_CALL_ID, id);
    }
    span
}

// =============================================================================
// Agent Run Span
// =============================================================================

/// Creates the span of one agent invocation ([`crate::agent_run_span`]), with
/// `gen_ai.conversation.id` set to `session_id` when provided.
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
    let span = crate::agent_run_span(agent_name, invocation_id);
    if let Some(sid) = session_id {
        span.record(GEN_AI_CONVERSATION_ID, sid);
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
        assert_eq!(GenAiProvider::MistralAi.as_str(), "mistral");
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
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "MAX_TOKENS"), "length");
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "SAFETY"), "content_filter");
        assert_eq!(map_finish_reason(GenAiProvider::Gemini, "UNKNOWN_REASON"), "UNKNOWN_REASON");
    }

    #[test]
    fn test_map_finish_reason_openai() {
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "stop"), "stop");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "length"), "length");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "tool_calls"), "tool_call");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "content_filter"), "content_filter");
        assert_eq!(map_finish_reason(GenAiProvider::OpenAI, "other"), "other");
    }

    #[test]
    fn test_map_finish_reason_anthropic() {
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "end_turn"), "stop");
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "max_tokens"), "length");
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "tool_use"), "tool_call");
        assert_eq!(map_finish_reason(GenAiProvider::Anthropic, "stop_sequence"), "stop");
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
                "gemini-3.7-flash",
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
