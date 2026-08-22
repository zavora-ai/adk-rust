//! Guardrail integration for LlmAgent
//!
//! This module provides guardrail support when the `guardrails` feature is enabled.

use adk_core::{Content, Result};

#[cfg(feature = "guardrails")]
use adk_core::AdkError;

#[cfg(feature = "guardrails")]
pub use adk_guardrail::{
    ContentFilter, ContentFilterConfig, Guardrail, GuardrailExecutor, GuardrailResult,
    GuardrailSet, PiiRedactor, PiiType, Severity,
};

#[cfg(feature = "guardrails")]
pub use adk_guardrail::SchemaValidator;

/// Placeholder type when guardrails feature is disabled
#[cfg(not(feature = "guardrails"))]
pub struct GuardrailSet;

#[cfg(not(feature = "guardrails"))]
impl GuardrailSet {
    /// Create an empty guardrail set (no-op when feature is disabled).
    pub fn new() -> Self {
        Self
    }
    /// Returns `true` (always empty when feature is disabled).
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[cfg(not(feature = "guardrails"))]
impl Default for GuardrailSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "guardrails")]
pub(crate) async fn enforce_guardrails(
    guardrails: &GuardrailSet,
    content: &Content,
    phase: &str,
) -> Result<Content> {
    let result = GuardrailExecutor::run(guardrails, content)
        .await
        .map_err(|err| AdkError::agent(format!("{phase} guardrail failed: {err}")))?;

    if !result.passed {
        let failures = result
            .failures
            .iter()
            .map(|(name, reason, severity)| format!("{name} ({severity:?}): {reason}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AdkError::agent(format!("{phase} guardrails blocked content: {failures}")));
    }

    Ok(result.transformed_content.unwrap_or_else(|| content.clone()))
}

#[cfg(not(feature = "guardrails"))]
pub(crate) async fn enforce_guardrails(
    _guardrails: &GuardrailSet,
    content: &Content,
    _phase: &str,
) -> Result<Content> {
    Ok(content.clone())
}

#[cfg(feature = "guardrails")]
pub use adk_guardrail::{
    DeniedArgumentPattern, PathAllowList, ToolCallDecision, ToolGuardrail, ToolGuardrailResult,
    ToolGuardrailSet,
};

/// Placeholder type when the guardrails feature is disabled.
#[cfg(not(feature = "guardrails"))]
pub struct ToolGuardrailSet;

#[cfg(not(feature = "guardrails"))]
impl ToolGuardrailSet {
    /// Create an empty tool guardrail set (no-op when the feature is disabled).
    pub fn new() -> Self {
        Self
    }
    /// Returns `true` (always empty when the feature is disabled).
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[cfg(not(feature = "guardrails"))]
impl Default for ToolGuardrailSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of screening a tool call, independent of whether guardrails are compiled in.
pub(crate) enum ToolScreening {
    /// The call may proceed with these arguments.
    Allow(serde_json::Value),
    /// The call is refused, with a reason to report back to the model.
    ///
    /// Never constructed without the `guardrails` feature, where screening is a no-op that always
    /// allows. The variant still exists so the call site is identical in both builds.
    #[cfg_attr(not(feature = "guardrails"), allow(dead_code))]
    Deny(String),
}

/// Screens a tool call against `guardrails` before it executes.
#[cfg(feature = "guardrails")]
pub(crate) async fn screen_tool_call(
    guardrails: &ToolGuardrailSet,
    tool_name: &str,
    args: &serde_json::Value,
) -> ToolScreening {
    if guardrails.is_empty() {
        return ToolScreening::Allow(args.clone());
    }

    match guardrails.evaluate(tool_name, args).await {
        ToolCallDecision::Allow { args } => ToolScreening::Allow(args),
        ToolCallDecision::Deny { guardrail, reason, severity } => ToolScreening::Deny(format!(
            "Tool '{tool_name}' blocked by guardrail '{guardrail}' ({severity:?}): {reason}"
        )),
    }
}

#[cfg(not(feature = "guardrails"))]
pub(crate) async fn screen_tool_call(
    _guardrails: &ToolGuardrailSet,
    _tool_name: &str,
    args: &serde_json::Value,
) -> ToolScreening {
    ToolScreening::Allow(args.clone())
}
