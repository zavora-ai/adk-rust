use adk_core::{CitationMetadata, Content, GroundingSource};
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

/// What an agent knows about its final answer's grounding, for
/// [`Guardrail::validate_output`].
///
/// # Example
///
/// ```rust
/// use adk_core::GroundingSource;
/// use adk_guardrail::OutputEvidence;
///
/// let sources = [GroundingSource::new("rag_search", "https://example.com/refunds")];
/// let evidence = OutputEvidence::new(&sources, None);
/// assert_eq!(evidence.sources.len(), 1);
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct OutputEvidence<'a> {
    /// The sources the agent's run recorded as retrieved, in the order recorded.
    pub sources: &'a [GroundingSource],
    /// The answer's citations: the provider's, and the answer's markdown links to a source in
    /// `sources`.
    pub citations: Option<&'a CitationMetadata>,
}

impl<'a> OutputEvidence<'a> {
    /// Evidence of `sources` retrieved and `citations` made.
    pub fn new(sources: &'a [GroundingSource], citations: Option<&'a CitationMetadata>) -> Self {
        Self { sources, citations }
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

    /// Validates an agent's final answer with what the agent knows about its grounding: the
    /// sources its run retrieved and the answer's citations.
    ///
    /// An agent's output guardrails screen its final answer through this method. Defaults to
    /// [`validate`](Self::validate); override it when a check needs the evidence, as
    /// [`CitationRequired`](crate::CitationRequired) does.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Content;
    /// use adk_guardrail::{Guardrail, GuardrailResult, OutputEvidence, Severity};
    /// use async_trait::async_trait;
    ///
    /// /// Fails an answer given while nothing was retrieved.
    /// struct Grounded;
    ///
    /// #[async_trait]
    /// impl Guardrail for Grounded {
    ///     fn name(&self) -> &str {
    ///         "grounded"
    ///     }
    ///
    ///     async fn validate(&self, _content: &Content) -> GuardrailResult {
    ///         GuardrailResult::Pass
    ///     }
    ///
    ///     async fn validate_output(
    ///         &self,
    ///         _content: &Content,
    ///         evidence: &OutputEvidence<'_>,
    ///     ) -> GuardrailResult {
    ///         if evidence.sources.is_empty() {
    ///             GuardrailResult::fail("nothing was retrieved", Severity::High)
    ///         } else {
    ///             GuardrailResult::Pass
    ///         }
    ///     }
    /// }
    /// ```
    async fn validate_output(
        &self,
        content: &Content,
        evidence: &OutputEvidence<'_>,
    ) -> GuardrailResult {
        let _ = evidence;
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
