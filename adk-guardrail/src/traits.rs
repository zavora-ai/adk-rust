use adk_core::Content;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Severity level for guardrail failures
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Result of guardrail validation
#[derive(Debug, Clone)]
pub enum GuardrailResult {
    /// Content passed validation
    Pass,
    /// Content failed validation
    Fail { reason: String, severity: Severity },
    /// Content was transformed (e.g., PII redacted)
    Transform { new_content: Content, reason: String },
}

impl GuardrailResult {
    pub fn pass() -> Self {
        Self::Pass
    }

    pub fn fail(reason: impl Into<String>, severity: Severity) -> Self {
        Self::Fail { reason: reason.into(), severity }
    }

    pub fn transform(new_content: Content, reason: impl Into<String>) -> Self {
        Self::Transform { new_content, reason: reason.into() }
    }

    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }

    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail { .. })
    }
}

/// Core guardrail trait for input/output validation
#[async_trait]
pub trait Guardrail: Send + Sync {
    /// Unique name for this guardrail
    fn name(&self) -> &str;

    /// Validate content and return result
    async fn validate(&self, content: &Content) -> GuardrailResult;

    /// Validates the text a model writes in a turn that also calls tools, or in a turn the
    /// provider paused (a preamble).
    ///
    /// An agent's output guardrails screen this text before it reaches the client or the
    /// session, as they screen the final response: a failure blocks the turn and a transform
    /// rewrites the text. `content` holds only the turn's text parts. Defaults to
    /// [`validate`](Self::validate); override it when a check describes the complete answer
    /// rather than any text the user can see, as [`SchemaValidator`](crate::SchemaValidator)
    /// does for the answer's JSON shape.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Content, Part};
    /// use adk_guardrail::{Guardrail, GuardrailResult, Severity};
    /// use async_trait::async_trait;
    ///
    /// /// Requires the final answer to cite a source; a preamble needs none.
    /// struct CitesSource;
    ///
    /// #[async_trait]
    /// impl Guardrail for CitesSource {
    ///     fn name(&self) -> &str {
    ///         "cites_source"
    ///     }
    ///
    ///     async fn validate(&self, content: &Content) -> GuardrailResult {
    ///         let text: String = content.parts.iter().filter_map(Part::text).collect();
    ///         if text.contains("Source:") {
    ///             GuardrailResult::Pass
    ///         } else {
    ///             GuardrailResult::fail("the answer cites no source", Severity::Medium)
    ///         }
    ///     }
    ///
    ///     async fn validate_preamble(&self, _content: &Content) -> GuardrailResult {
    ///         GuardrailResult::Pass
    ///     }
    /// }
    /// ```
    async fn validate_preamble(&self, content: &Content) -> GuardrailResult {
        self.validate(content).await
    }

    /// Whether to run in parallel with other guardrails (default: true)
    fn run_parallel(&self) -> bool {
        true
    }

    /// Whether to fail fast on this guardrail's failure (default: true for High/Critical)
    fn fail_fast(&self) -> bool {
        true
    }
}
