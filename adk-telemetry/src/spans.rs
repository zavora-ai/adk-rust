//! Span helpers for common ADK operations
//!
//! Provides pre-configured spans for instrumenting agent, model, and tool operations.

use tracing::Span;

/// The `gen_ai.provider.name` of an ADK-Rust provider id: the GenAI conventions' well-known
/// value where one exists (`gemini` is `gcp.gemini`, `bedrock` is `aws.bedrock`,
/// `azure-ai` is `azure.ai.inference`, `xai` is `x_ai`), and the id itself otherwise.
///
/// # Example
///
/// ```
/// use adk_telemetry::genai_provider_name;
///
/// assert_eq!(genai_provider_name("gemini"), "gcp.gemini");
/// assert_eq!(genai_provider_name("anthropic"), "anthropic");
/// assert_eq!(genai_provider_name("ollama"), "ollama");
/// ```
pub fn genai_provider_name(provider: &str) -> &str {
    match provider {
        "gemini" | "google" => "gcp.gemini",
        "gemini-vertex" | "vertex" | "vertex-ai" => "gcp.vertex_ai",
        "bedrock" | "aws-bedrock" => "aws.bedrock",
        "azure-ai" | "azure" => "azure.ai.inference",
        "azure-openai" => "azure.ai.openai",
        "mistral" | "mistral_ai" => "mistral",
        "moonshot" => "moonshot_ai",
        "xai" | "x-ai" => "x_ai",
        "watsonx" => "ibm.watsonx.ai",
        other => other,
    }
}

/// The `gen_ai.operation.name` of a model call to `provider`: `generate_content` for
/// Gemini, whose API is named so, and `chat` for every other provider.
pub fn genai_inference_operation(provider: &str) -> &'static str {
    match genai_provider_name(provider) {
        "gcp.gemini" | "gcp.vertex_ai" | "gcp.gen_ai" => "generate_content",
        _ => "chat",
    }
}

/// Creates the span of one in-process agent invocation, named `invoke_agent {agent_name}` in
/// OpenTelemetry export, kind `internal`, with `gen_ai.operation.name` `invoke_agent` and
/// `gen_ai.agent.name`, and declares `gen_ai.agent.description`, `gen_ai.conversation.id`
/// and `error.type`.
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
        otel.name = %format!("invoke_agent {agent_name}"),
        otel.kind = "internal",
        gen_ai.operation.name = "invoke_agent",
        gen_ai.agent.name = agent_name,
        gen_ai.agent.description = tracing::field::Empty,
        gen_ai.conversation.id = tracing::field::Empty,
        error.type = tracing::field::Empty,
        agent.name = agent_name,
        invocation.id = invocation_id,
    )
}

/// Create a span for model API calls
///
/// Prefer [`llm_generate_span`], which follows the GenAI conventions.
///
/// # Arguments
/// * `model_name` - Name of the LLM model being called
///
/// # Example
/// ```
/// use adk_telemetry::model_call_span;
/// let span = model_call_span("gemini-3.8-flash");
/// let _enter = span.enter();
/// // Model call code here
/// ```
pub fn model_call_span(model_name: &str) -> Span {
    tracing::info_span!("model.call", model.name = model_name, otel.kind = "client")
}

/// Creates the inference span of one model call under the
/// [OpenTelemetry GenAI semantic conventions](https://github.com/open-telemetry/semantic-conventions-genai):
/// named `{gen_ai.operation.name} {gen_ai.request.model}` in OpenTelemetry export (such as
/// `chat claude-sonnet-5-5`), kind `client`.
///
/// It sets `gen_ai.operation.name` ([`genai_inference_operation`]), `gen_ai.provider.name`
/// ([`genai_provider_name`]) and `gen_ai.request.model`, `gen_ai.request.stream` when the
/// request streams, and declares, for recording once known: the request parameters
/// (`gen_ai.request.temperature`, `top_p`, `top_k`, `max_tokens`, `frequency_penalty`,
/// `presence_penalty`, `seed`, `stop_sequences`), `gen_ai.output.type`, the response
/// (`gen_ai.response.model`, `gen_ai.response.id`, `gen_ai.response.finish_reasons`),
/// the usage ([`record_llm_usage`]), the opt-in
/// content (`gen_ai.system_instructions`, `gen_ai.input.messages`, `gen_ai.output.messages`,
/// `gen_ai.tool.definitions`) and `error.type`.
///
/// # Arguments
/// * `provider` - ADK-Rust provider id (e.g., "gemini", "openai", "anthropic")
/// * `model_name` - Model identifier (e.g., "gemini-3.8-flash", "gpt-6.1-sol")
/// * `stream` - Whether this is a streaming request
///
/// # Example
/// ```
/// use adk_telemetry::llm_generate_span;
/// let span = llm_generate_span("openai", "gpt-6.1-sol", true);
/// let _enter = span.enter();
/// // After response: adk_telemetry::record_llm_usage(&usage_metadata);
/// ```
pub fn llm_generate_span(provider: &str, model_name: &str, stream: bool) -> Span {
    let operation = genai_inference_operation(provider);
    let span = tracing::info_span!(
        "gen_ai.generate",
        otel.name = %format!("{operation} {model_name}"),
        otel.kind = "client",
        gen_ai.operation.name = operation,
        gen_ai.provider.name = genai_provider_name(provider),
        gen_ai.request.model = %model_name,
        gen_ai.request.stream = tracing::field::Empty,
        gen_ai.request.temperature = tracing::field::Empty,
        gen_ai.request.top_p = tracing::field::Empty,
        gen_ai.request.top_k = tracing::field::Empty,
        gen_ai.request.max_tokens = tracing::field::Empty,
        gen_ai.request.frequency_penalty = tracing::field::Empty,
        gen_ai.request.presence_penalty = tracing::field::Empty,
        gen_ai.request.seed = tracing::field::Empty,
        gen_ai.request.stop_sequences = tracing::field::Empty,
        gen_ai.output.type = tracing::field::Empty,
        gen_ai.response.model = tracing::field::Empty,
        gen_ai.response.id = tracing::field::Empty,
        gen_ai.response.finish_reasons = tracing::field::Empty,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
        gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
        gen_ai.usage.cache_write.input_tokens = tracing::field::Empty,
        gen_ai.usage.reasoning.output_tokens = tracing::field::Empty,
        gen_ai.usage.audio.input_tokens = tracing::field::Empty,
        gen_ai.usage.audio.output_tokens = tracing::field::Empty,
        gen_ai.system_instructions = tracing::field::Empty,
        gen_ai.input.messages = tracing::field::Empty,
        gen_ai.output.messages = tracing::field::Empty,
        gen_ai.tool.definitions = tracing::field::Empty,
        error.type = tracing::field::Empty,
    );
    // Conditionally required: present only when the request streams.
    if stream {
        span.record("gen_ai.request.stream", true);
    }
    span
}

/// Records `values` as the string-array attribute `key` of `span`.
///
/// With feature `otlp`, the OpenTelemetry span receives a string array, as the GenAI
/// conventions type `gen_ai.response.finish_reasons` and `gen_ai.request.stop_sequences`,
/// and the tracing field `key` stays empty, since a tracing field cannot hold an array and
/// recording both would export the key twice. Without `otlp`, the tracing field `key`
/// holds the values as a JSON array, for the in-process and SQLite exporters.
///
/// # Example
/// ```
/// use adk_telemetry::{llm_generate_span, record_str_array};
///
/// let span = llm_generate_span("anthropic", "claude-sonnet-5-5", false);
/// record_str_array(&span, "gen_ai.response.finish_reasons", &["stop"]);
/// ```
pub fn record_str_array(span: &Span, key: &'static str, values: &[&str]) {
    #[cfg(not(feature = "otlp"))]
    span.record(key, serde_json_array(values).as_str());
    #[cfg(feature = "otlp")]
    {
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        let array: Vec<opentelemetry::StringValue> = values
            .iter()
            .map(|value| opentelemetry::StringValue::from(value.to_string()))
            .collect();
        span.set_attribute(key, opentelemetry::Value::Array(array.into()));
    }
}

/// `values` as a JSON array of strings.
#[cfg(not(feature = "otlp"))]
fn serde_json_array(values: &[&str]) -> String {
    let mut out = String::from("[");
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
                c => out.push(c),
            }
        }
        out.push('"');
    }
    out.push(']');
    out
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
    /// Prompt / input token count, cached tokens included.
    pub input_tokens: i32,
    /// Completion / output token count.
    pub output_tokens: i32,
    /// Total token count; the conventions define no total, so it is not recorded.
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

/// Records LLM token usage on the current span, under the GenAI conventions' names.
///
/// | `LlmUsage` | Attribute |
/// |---|---|
/// | `input_tokens` | `gen_ai.usage.input_tokens` |
/// | `output_tokens` | `gen_ai.usage.output_tokens` |
/// | `cache_read_tokens` | `gen_ai.usage.cache_read.input_tokens` |
/// | `cache_creation_tokens` | `gen_ai.usage.cache_write.input_tokens` |
/// | `thinking_tokens` | `gen_ai.usage.reasoning.output_tokens` |
/// | `audio_input_tokens` | `gen_ai.usage.audio.input_tokens` |
/// | `audio_output_tokens` | `gen_ai.usage.audio.output_tokens` |
///
/// `total_tokens` is not recorded: the conventions define no total.
///
/// Optional fields are recorded only when present. This is safe to call even if the current
/// span was not created by [`llm_generate_span`]: `tracing` ignores undeclared fields.
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
    if let Some(v) = usage.cache_read_tokens {
        span.record("gen_ai.usage.cache_read.input_tokens", i64::from(v));
    }
    if let Some(v) = usage.cache_creation_tokens {
        span.record("gen_ai.usage.cache_write.input_tokens", i64::from(v));
    }
    if let Some(v) = usage.thinking_tokens {
        span.record("gen_ai.usage.reasoning.output_tokens", i64::from(v));
    }
    if let Some(v) = usage.audio_input_tokens {
        span.record("gen_ai.usage.audio.input_tokens", i64::from(v));
    }
    if let Some(v) = usage.audio_output_tokens {
        span.record("gen_ai.usage.audio.output_tokens", i64::from(v));
    }
}

/// Creates the span of one tool execution, named `execute_tool {tool_name}` in OpenTelemetry
/// export, kind `internal`, with `gen_ai.operation.name` `execute_tool`, `gen_ai.tool.name`
/// and `gen_ai.tool.type` `function`, and declares `gen_ai.tool.call.id`,
/// `gen_ai.tool.description`, `gen_ai.agent.name`, `gen_ai.conversation.id`, the opt-in
/// `gen_ai.tool.call.arguments` and `gen_ai.tool.call.result`, and `error.type`.
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
    tracing::info_span!(
        "tool.execute",
        otel.name = %format!("execute_tool {tool_name}"),
        otel.kind = "internal",
        gen_ai.operation.name = "execute_tool",
        gen_ai.tool.name = tool_name,
        gen_ai.tool.type = "function",
        gen_ai.tool.call.id = tracing::field::Empty,
        gen_ai.tool.description = tracing::field::Empty,
        gen_ai.agent.name = tracing::field::Empty,
        gen_ai.conversation.id = tracing::field::Empty,
        gen_ai.tool.call.arguments = tracing::field::Empty,
        gen_ai.tool.call.result = tracing::field::Empty,
        error.type = tracing::field::Empty,
        tool.name = tool_name,
    )
}

/// Creates a span for one portable team invocation.
pub fn team_run_span(team_name: &str, invocation_id: &str, coordinator: &str) -> Span {
    tracing::info_span!(
        "team.run",
        team.name = team_name,
        team.coordinator = coordinator,
        invocation.id = invocation_id,
        team.status = tracing::field::Empty,
        otel.kind = "internal"
    )
}

/// Creates a portable team span with the runtime session identifiers required
/// by in-process session telemetry.
pub fn team_run_span_with_context(
    team_name: &str,
    invocation_id: &str,
    session_id: &str,
    coordinator: &str,
) -> Span {
    tracing::info_span!(
        "team.run",
        team.name = team_name,
        team.coordinator = coordinator,
        invocation.id = invocation_id,
        "gcp.vertex.agent.invocation_id" = invocation_id,
        "gcp.vertex.agent.session_id" = session_id,
        "gcp.vertex.agent.event_id" = %format!("{invocation_id}:team"),
        "gen_ai.conversation.id" = session_id,
        team.status = tracing::field::Empty,
        otel.kind = "internal"
    )
}

/// Creates a span for one member execution inside a portable team.
pub fn team_member_span(team_name: &str, member: &str, invocation_id: &str) -> Span {
    tracing::info_span!(
        "team.member.run",
        team.name = team_name,
        team.member = member,
        invocation.id = invocation_id,
        team.status = tracing::field::Empty,
        otel.kind = "internal"
    )
}

/// Creates a portable team member span with explicit invocation and session
/// identifiers for lossless in-process export.
pub fn team_member_span_with_context(
    team_name: &str,
    member: &str,
    invocation_id: &str,
    session_id: &str,
) -> Span {
    tracing::info_span!(
        "team.member.run",
        team.name = team_name,
        team.member = member,
        invocation.id = invocation_id,
        "gcp.vertex.agent.invocation_id" = invocation_id,
        "gcp.vertex.agent.session_id" = session_id,
        "gcp.vertex.agent.event_id" = %format!("{invocation_id}:team-member:{member}"),
        "gen_ai.conversation.id" = session_id,
        team.status = tracing::field::Empty,
        otel.kind = "internal"
    )
}

/// Creates a span for one exact team relationship execution.
pub fn team_relationship_span(
    team_name: &str,
    from: &str,
    to: &str,
    kind: &str,
    edge_id: &str,
) -> Span {
    tracing::info_span!(
        "team.relationship.execute",
        team.name = team_name,
        team.relationship.from = from,
        team.relationship.to = to,
        team.relationship.kind = kind,
        team.edge.id = edge_id,
        team.status = tracing::field::Empty,
        team.duration_ms = tracing::field::Empty,
        otel.kind = "internal"
    )
}

/// Creates an exact team relationship span with explicit runtime context.
///
/// `edge_id` is used as the event identifier because every relationship
/// execution already has a stable, unique causal edge ID.
pub fn team_relationship_span_with_context(
    team_name: &str,
    from: &str,
    to: &str,
    kind: &str,
    edge_id: &str,
    invocation_id: &str,
    session_id: &str,
) -> Span {
    tracing::info_span!(
        "team.relationship.execute",
        team.name = team_name,
        team.relationship.from = from,
        team.relationship.to = to,
        team.relationship.kind = kind,
        team.edge.id = edge_id,
        "gcp.vertex.agent.invocation_id" = invocation_id,
        "gcp.vertex.agent.session_id" = session_id,
        "gcp.vertex.agent.event_id" = edge_id,
        "gen_ai.conversation.id" = session_id,
        team.status = tracing::field::Empty,
        team.duration_ms = tracing::field::Empty,
        otel.kind = "internal"
    )
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
