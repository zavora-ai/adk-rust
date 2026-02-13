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
