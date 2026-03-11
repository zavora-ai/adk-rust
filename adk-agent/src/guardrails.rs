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
    pub fn new() -> Self {
        Self
    }
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
        .map_err(|err| AdkError::Agent(format!("{phase} guardrail failed: {err}")))?;

    if !result.passed {
        let failures = result
            .failures
            .iter()
            .map(|(name, reason, severity)| format!("{name} ({severity:?}): {reason}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AdkError::Agent(format!("{phase} guardrails blocked content: {failures}")));
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
