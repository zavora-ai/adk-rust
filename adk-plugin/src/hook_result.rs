//! Hook result types for the enhanced plugin system.
//!
//! These enums define the return types for plugin hooks, enabling
//! continue/short-circuit semantics in the plugin pipeline.
//!
//! # Overview
//!
//! Each hook type has a corresponding result enum:
//!
//! - [`BeforeToolCallResult`] — returned by `before_tool_call` hooks
//! - [`AfterToolCallResult`] — returned by `after_tool_call` hooks
//! - [`BeforeModelCallResult`] — returned by `before_model_call` hooks
//! - [`AfterModelCallResult`] — returned by `after_model_call` hooks
//!
//! "Before" hooks support short-circuiting (skipping the underlying operation),
//! while "after" hooks only support continuing with a (possibly modified) result.

use adk_core::{LlmRequest, LlmResponse};

/// Result from a `before_tool_call` hook invocation.
///
/// Determines whether tool execution continues with (possibly modified) arguments,
/// or is short-circuited with a synthetic result.
///
/// # Examples
///
/// ## Continuing with modified arguments
///
/// ```rust
/// use adk_plugin::BeforeToolCallResult;
/// use serde_json::json;
///
/// // Pass through arguments unchanged
/// let args = json!({"query": "hello"});
/// let result = BeforeToolCallResult::Continue(args);
///
/// // Or modify arguments before passing to the tool
/// let sanitized = json!({"query": "hello", "safe_mode": true});
/// let result = BeforeToolCallResult::Continue(sanitized);
/// ```
///
/// ## Short-circuiting tool execution
///
/// ```rust
/// use adk_plugin::BeforeToolCallResult;
/// use serde_json::json;
///
/// // Skip tool execution and return a cached result
/// let cached = json!({"data": "cached response"});
/// let result = BeforeToolCallResult::ShortCircuit(cached);
/// ```
#[derive(Debug)]
pub enum BeforeToolCallResult {
    /// Continue execution with (possibly modified) arguments.
    ///
    /// The contained value will be passed to the next plugin in the chain,
    /// and ultimately to the tool execution if this is the last plugin.
    Continue(serde_json::Value),

    /// Short-circuit: skip tool execution and use this synthetic result.
    ///
    /// When returned, no further plugins in the chain are invoked,
    /// the tool is not executed, and this value is used as the tool output.
    ShortCircuit(serde_json::Value),
}

/// Result from an `after_tool_call` hook invocation.
///
/// After-tool hooks can only continue with a (possibly modified) result.
/// Short-circuiting is not supported for after-hooks since the operation
/// has already completed.
///
/// # Examples
///
/// ```rust
/// use adk_plugin::AfterToolCallResult;
/// use serde_json::json;
///
/// // Pass through the tool result unchanged
/// let tool_output = json!({"status": "success", "data": [1, 2, 3]});
/// let result = AfterToolCallResult::Continue(tool_output);
///
/// // Or modify the result (e.g., add metadata)
/// let enriched = json!({"status": "success", "data": [1, 2, 3], "cached": false});
/// let result = AfterToolCallResult::Continue(enriched);
/// ```
#[derive(Debug)]
pub enum AfterToolCallResult {
    /// Continue with (possibly modified) result.
    ///
    /// The contained value will be passed to the next plugin in the chain,
    /// and ultimately returned to the agent as the tool output.
    Continue(serde_json::Value),
}

/// Result from a `before_model_call` hook invocation.
///
/// Determines whether the model call continues with a (possibly modified) request,
/// or is short-circuited with a synthetic response.
///
/// # Examples
///
/// ## Continuing with a modified request
///
/// ```rust,ignore
/// use adk_plugin::BeforeModelCallResult;
/// use adk_core::LlmRequest;
///
/// // Modify the request (e.g., inject a system instruction)
/// let mut request = LlmRequest::default();
/// request.model = "gemini-2.5-flash".to_string();
/// let result = BeforeModelCallResult::Continue(request);
/// ```
///
/// ## Short-circuiting with a cached response
///
/// ```rust,ignore
/// use adk_plugin::BeforeModelCallResult;
/// use adk_core::LlmResponse;
///
/// // Return a cached response without calling the model
/// let cached_response = LlmResponse::default();
/// let result = BeforeModelCallResult::ShortCircuit(cached_response);
/// ```
#[derive(Debug)]
pub enum BeforeModelCallResult {
    /// Continue execution with (possibly modified) LLM request.
    ///
    /// The contained request will be passed to the next plugin in the chain,
    /// and ultimately to the LLM provider if this is the last plugin.
    Continue(LlmRequest),

    /// Short-circuit: skip the model call and use this synthetic response.
    ///
    /// When returned, no further plugins in the chain are invoked,
    /// the LLM is not called, and this response is used as the model output.
    ShortCircuit(LlmResponse),
}

/// Result from an `after_model_call` hook invocation.
///
/// After-model hooks can only continue with a (possibly modified) response.
/// Short-circuiting is not supported for after-hooks since the model call
/// has already completed.
///
/// # Examples
///
/// ```rust,ignore
/// use adk_plugin::AfterModelCallResult;
/// use adk_core::LlmResponse;
///
/// // Pass through the model response unchanged
/// let response = LlmResponse::default();
/// let result = AfterModelCallResult::Continue(response);
/// ```
#[derive(Debug)]
pub enum AfterModelCallResult {
    /// Continue with (possibly modified) LLM response.
    ///
    /// The contained response will be passed to the next plugin in the chain,
    /// and ultimately returned to the agent as the model output.
    Continue(LlmResponse),
}
