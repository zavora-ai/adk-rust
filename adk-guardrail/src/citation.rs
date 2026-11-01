//! A guardrail that requires an agent's final answer to cite what its run retrieved.

use std::collections::BTreeSet;

use adk_core::Content;
use async_trait::async_trait;

use crate::{Guardrail, GuardrailResult, OutputEvidence, Severity};

/// What [`CitationRequired`] does with an answer given while the run retrieved nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WhenNothingRetrieved {
    /// The answer fails: it cannot cite a source, so it is not grounded.
    Fail,
    /// The answer passes, for an agent that may answer without retrieval, such as a greeting.
    Pass,
}

/// Requires an agent's final answer to cite at least `min_citations` distinct sources its run
/// retrieved.
///
/// A citation counts when its URI is the URI of a
/// [`GroundingSource`](adk_core::GroundingSource) the run recorded: a markdown link the
/// agent bound from the answer, or a provider citation of a retrieved source. A provider
/// citation of any other URI counts only with
/// [`trust_provider_citations`](Self::trust_provider_citations). The guardrail checks only the
/// final answer: text written beside a tool call passes, and so does `validate` without the
/// evidence an agent supplies, which fails instead, because the citations cannot be checked.
///
/// | Setting | Default |
/// |---------|---------|
/// | [`min_citations`](Self::min_citations) | 1 |
/// | [`when_nothing_retrieved`](Self::when_nothing_retrieved) | [`WhenNothingRetrieved::Fail`] |
/// | [`trust_provider_citations`](Self::trust_provider_citations) | `false` |
/// | [`severity`](Self::severity) | [`Severity::High`], which blocks the answer |
///
/// # Example
///
/// ```rust
/// use adk_core::{CitationMetadata, CitationSource, Content, GroundingSource};
/// use adk_guardrail::{CitationRequired, GuardrailExecutor, GuardrailSet, OutputEvidence};
///
/// # futures::executor::block_on(async {
/// let set = GuardrailSet::new().with(CitationRequired::new());
/// let sources = [GroundingSource::new("rag_search", "https://example.com/refunds")];
/// let cited = CitationMetadata {
///     citation_sources: vec![CitationSource {
///         uri: Some("https://example.com/refunds".to_string()),
///         title: None,
///         start_index: Some(0),
///         end_index: Some(40),
///         license: None,
///         publication_date: None,
///     }],
/// };
/// let answer = Content::new("model")
///     .with_text("[Refunds take 30 days](https://example.com/refunds).");
/// let evidence = OutputEvidence::new(&sources, Some(&cited));
/// assert!(GuardrailExecutor::enforce_output(&set, &answer, &evidence).await.is_ok());
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct CitationRequired {
    min_citations: usize,
    when_nothing_retrieved: WhenNothingRetrieved,
    trust_provider_citations: bool,
    severity: Severity,
}

impl Default for CitationRequired {
    fn default() -> Self {
        Self::new()
    }
}

impl CitationRequired {
    /// Requires one citation of a retrieved source, failing an answer when nothing was
    /// retrieved.
    pub fn new() -> Self {
        Self {
            min_citations: 1,
            when_nothing_retrieved: WhenNothingRetrieved::Fail,
            trust_provider_citations: false,
            severity: Severity::High,
        }
    }

    /// Requires `count` distinct cited sources, at least one.
    #[must_use]
    pub fn min_citations(mut self, count: usize) -> Self {
        self.min_citations = count.max(1);
        self
    }

    /// Sets what an answer given while nothing was retrieved does.
    #[must_use]
    pub fn when_nothing_retrieved(mut self, outcome: WhenNothingRetrieved) -> Self {
        self.when_nothing_retrieved = outcome;
        self
    }

    /// Counts a provider citation whose URI the run did not retrieve, such as one from the
    /// provider's own web search.
    #[must_use]
    pub fn trust_provider_citations(mut self, trust: bool) -> Self {
        self.trust_provider_citations = trust;
        self
    }

    /// Sets the severity of a failure; [`Severity::Low`] records a warning and passes.
    #[must_use]
    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}

#[async_trait]
impl Guardrail for CitationRequired {
    fn name(&self) -> &str {
        "citation_required"
    }

    async fn validate(&self, _content: &Content) -> GuardrailResult {
        GuardrailResult::fail(
            "citations cannot be checked without the agent's output evidence; run \
             CitationRequired as an agent's output guardrail",
            self.severity,
        )
    }

    async fn validate_preamble(&self, _content: &Content) -> GuardrailResult {
        GuardrailResult::Pass
    }

    async fn validate_output(
        &self,
        _content: &Content,
        evidence: &OutputEvidence<'_>,
    ) -> GuardrailResult {
        let retrieved: BTreeSet<&str> =
            evidence.sources.iter().map(|source| source.uri.as_str()).collect();
        let cited: BTreeSet<&str> = evidence
            .citations
            .into_iter()
            .flat_map(|metadata| &metadata.citation_sources)
            .filter_map(|citation| citation.uri.as_deref())
            .filter(|uri| self.trust_provider_citations || retrieved.contains(uri))
            .collect();
        if cited.len() >= self.min_citations {
            return GuardrailResult::Pass;
        }
        if retrieved.is_empty() {
            return match self.when_nothing_retrieved {
                WhenNothingRetrieved::Pass => GuardrailResult::Pass,
                WhenNothingRetrieved::Fail => GuardrailResult::fail(
                    "the answer is not grounded: nothing was retrieved in this run",
                    self.severity,
                ),
            };
        }
        GuardrailResult::fail(
            format!(
                "the answer cites {} of the {} retrieved sources it must cite; link a \
                 retrieved source as [title](uri)",
                cited.len(),
                self.min_citations
            ),
            self.severity,
        )
    }
}

#[cfg(test)]
mod tests {
    use adk_core::{CitationMetadata, CitationSource, GroundingSource};

    use super::*;

    fn citation(uri: &str) -> CitationSource {
        CitationSource {
            uri: Some(uri.to_string()),
            title: None,
            start_index: None,
            end_index: None,
            license: None,
            publication_date: None,
        }
    }

    fn answer() -> Content {
        Content::new("model").with_text("an answer")
    }

    async fn check(guard: &CitationRequired, sources: &[&str], cited: &[&str]) -> bool {
        let sources: Vec<GroundingSource> =
            sources.iter().map(|uri| GroundingSource::new("rag_search", *uri)).collect();
        let metadata =
            CitationMetadata { citation_sources: cited.iter().map(|uri| citation(uri)).collect() };
        let evidence = OutputEvidence::new(&sources, Some(&metadata));
        guard.validate_output(&answer(), &evidence).await.is_pass()
    }

    #[tokio::test]
    async fn counts_only_citations_of_retrieved_sources_by_default() {
        let guard = CitationRequired::new();
        assert!(check(&guard, &["u:a"], &["u:a"]).await);
        assert!(!check(&guard, &["u:a"], &["u:elsewhere"]).await);
        assert!(!check(&guard, &["u:a"], &[]).await);
        assert!(check(&guard.clone().trust_provider_citations(true), &["u:a"], &["u:x"]).await);
    }

    #[tokio::test]
    async fn min_citations_counts_distinct_sources() {
        let guard = CitationRequired::new().min_citations(2);
        assert!(!check(&guard, &["u:a", "u:b"], &["u:a", "u:a"]).await);
        assert!(check(&guard, &["u:a", "u:b"], &["u:a", "u:b"]).await);
    }

    #[tokio::test]
    async fn nothing_retrieved_fails_unless_configured_to_pass() {
        assert!(!check(&CitationRequired::new(), &[], &[]).await);
        let lenient = CitationRequired::new().when_nothing_retrieved(WhenNothingRetrieved::Pass);
        assert!(check(&lenient, &[], &[]).await);
    }

    #[tokio::test]
    async fn only_the_final_answer_is_checked() {
        let guard = CitationRequired::new();
        assert!(guard.validate_preamble(&answer()).await.is_pass());
        assert!(guard.validate(&answer()).await.is_fail(), "no evidence, no pass");
    }
}
