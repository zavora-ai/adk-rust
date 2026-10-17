use crate::{
    Guardrail, GuardrailError, GuardrailFailure, GuardrailPhase, GuardrailResult, Result, Severity,
};
use adk_core::Content;
use futures::future::join_all;
use std::sync::Arc;

/// A collection of guardrails to execute together.
///
/// Use the builder-style [`with`](Self::with) method to add guardrails.
pub struct GuardrailSet {
    guardrails: Vec<Arc<dyn Guardrail>>,
}

impl GuardrailSet {
    /// Create an empty guardrail set.
    pub fn new() -> Self {
        Self { guardrails: Vec::new() }
    }

    /// Add a guardrail (by value, automatically wrapped in `Arc`).
    pub fn with(mut self, guardrail: impl Guardrail + 'static) -> Self {
        self.guardrails.push(Arc::new(guardrail));
        self
    }

    /// Add a pre-wrapped guardrail.
    pub fn with_arc(mut self, guardrail: Arc<dyn Guardrail>) -> Self {
        self.guardrails.push(guardrail);
        self
    }

    /// Get a reference to the registered guardrails.
    pub fn guardrails(&self) -> &[Arc<dyn Guardrail>] {
        &self.guardrails
    }

    /// Returns `true` if no guardrails have been added.
    pub fn is_empty(&self) -> bool {
        self.guardrails.is_empty()
    }
}

impl Default for GuardrailSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of running a [`GuardrailSet`].
#[derive(Debug)]
pub struct ExecutionResult {
    /// `true` if all guardrails passed (no critical failures).
    pub passed: bool,
    /// Content after guardrail transformations, or `None` if unchanged.
    pub transformed_content: Option<Content>,
    /// List of failures as `(guardrail_name, reason, severity)`.
    pub failures: Vec<(String, String, Severity)>,
}

/// What [`GuardrailExecutor::enforce`] lets through.
#[derive(Debug, Clone)]
pub struct Screening {
    /// The content to continue with, after any transformation.
    pub content: Content,
    /// The [`Severity::Low`] failures, which pass the content.
    pub warnings: Vec<GuardrailFailure>,
}

/// Executor for running guardrails in parallel
pub struct GuardrailExecutor;

impl GuardrailExecutor {
    /// Run all guardrails in parallel, with early exit on critical failures
    pub async fn run(guardrails: &GuardrailSet, content: &Content) -> Result<ExecutionResult> {
        Self::run_with(guardrails, content, false).await
    }

    /// Runs every guardrail's [`validate_preamble`](Guardrail::validate_preamble) over the text
    /// of a model turn that also calls tools, as [`run`](Self::run) runs `validate`.
    pub async fn run_preamble(
        guardrails: &GuardrailSet,
        content: &Content,
    ) -> Result<ExecutionResult> {
        Self::run_with(guardrails, content, true).await
    }

    /// Runs `guardrails` over `content` and turns a failing verdict into a block.
    ///
    /// Content passes when no failure is above [`Severity::Low`]; it continues transformed if a
    /// guardrail transformed it, and the low-severity failures are returned as
    /// [`Screening::warnings`] and logged at `warn` with the guardrail's name and severity (not
    /// its reason, which can quote the content). `preamble` selects
    /// [`validate_preamble`](Guardrail::validate_preamble) over
    /// [`validate`](Guardrail::validate).
    ///
    /// # Errors
    ///
    /// Returns [`GuardrailError::Blocked`] with every failure when a failure is above `Low`,
    /// including a critical failure that stopped the run early.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Content;
    /// use adk_guardrail::{GuardrailExecutor, GuardrailPhase, GuardrailSet, PiiRedactor};
    ///
    /// # futures::executor::block_on(async {
    /// let set = GuardrailSet::new().with(PiiRedactor::new());
    /// let content = Content::new("model").with_text("Mail ana@example.com");
    /// let screened = GuardrailExecutor::enforce(&set, &content, GuardrailPhase::Output, false)
    ///     .await
    ///     .unwrap();
    /// assert_eq!(screened.content.parts[0].text(), Some("Mail [EMAIL REDACTED]"));
    /// # });
    /// ```
    pub async fn enforce(
        guardrails: &GuardrailSet,
        content: &Content,
        phase: GuardrailPhase,
        preamble: bool,
    ) -> Result<Screening> {
        let result = match Self::run_with(guardrails, content, preamble).await {
            Ok(result) => result,
            Err(GuardrailError::ValidationFailed { name, reason, severity }) => {
                return Err(GuardrailError::Blocked {
                    phase,
                    failures: vec![GuardrailFailure { guardrail: name, reason, severity }],
                });
            }
            Err(error) => return Err(error),
        };
        let failures: Vec<GuardrailFailure> = result
            .failures
            .into_iter()
            .map(|(guardrail, reason, severity)| GuardrailFailure { guardrail, reason, severity })
            .collect();
        if !result.passed {
            return Err(GuardrailError::Blocked { phase, failures });
        }
        for warning in &failures {
            tracing::warn!(
                guardrail.name = %warning.guardrail,
                guardrail.severity = ?warning.severity,
                guardrail.phase = %phase,
                "guardrail flagged content at low severity; the content passes"
            );
        }
        Ok(Screening {
            content: result.transformed_content.unwrap_or_else(|| content.clone()),
            warnings: failures,
        })
    }

    async fn run_with(
        guardrails: &GuardrailSet,
        content: &Content,
        preamble: bool,
    ) -> Result<ExecutionResult> {
        if guardrails.is_empty() {
            return Ok(ExecutionResult {
                passed: true,
                transformed_content: None,
                failures: vec![],
            });
        }

        // Separate parallel and sequential guardrails
        let (parallel, sequential): (Vec<_>, Vec<_>) =
            guardrails.guardrails().iter().partition(|g| g.run_parallel());

        let mut current_content = content.clone();
        let mut all_failures = Vec::new();

        // Run parallel guardrails
        if !parallel.is_empty() {
            let futures: Vec<_> = parallel
                .iter()
                .map(|g| Self::run_single(Arc::clone(g), &current_content, preamble))
                .collect();

            let results = join_all(futures).await;

            for (guardrail, result) in parallel.iter().zip(results) {
                match result {
                    GuardrailResult::Pass => {}
                    GuardrailResult::Fail { reason, severity } => {
                        all_failures.push((guardrail.name().to_string(), reason.clone(), severity));
                        // Early exit on critical
                        if severity == Severity::Critical && guardrail.fail_fast() {
                            return Err(GuardrailError::ValidationFailed {
                                name: guardrail.name().to_string(),
                                reason,
                                severity,
                            });
                        }
                    }
                    // A transform reason can quote the content it replaced, so it is not logged.
                    GuardrailResult::Transform { new_content, reason: _ } => {
                        tracing::debug!(guardrail = guardrail.name(), "content transformed");
                        current_content = new_content;
                    }
                }
            }
        }

        // Run sequential guardrails
        for guardrail in sequential {
            let result = Self::run_single(Arc::clone(guardrail), &current_content, preamble).await;
            match result {
                GuardrailResult::Pass => {}
                GuardrailResult::Fail { reason, severity } => {
                    all_failures.push((guardrail.name().to_string(), reason.clone(), severity));
                    if severity == Severity::Critical && guardrail.fail_fast() {
                        return Err(GuardrailError::ValidationFailed {
                            name: guardrail.name().to_string(),
                            reason,
                            severity,
                        });
                    }
                }
                GuardrailResult::Transform { new_content, reason: _ } => {
                    tracing::debug!(guardrail = guardrail.name(), "content transformed");
                    current_content = new_content;
                }
            }
        }

        let passed =
            all_failures.is_empty() || all_failures.iter().all(|(_, _, s)| *s == Severity::Low);

        // Check if content was transformed by comparing serialized forms
        let was_transformed =
            serde_json::to_string(&current_content).ok() != serde_json::to_string(content).ok();
        let transformed = if was_transformed { Some(current_content) } else { None };

        Ok(ExecutionResult { passed, transformed_content: transformed, failures: all_failures })
    }

    async fn run_single(
        guardrail: Arc<dyn Guardrail>,
        content: &Content,
        preamble: bool,
    ) -> GuardrailResult {
        if preamble {
            guardrail.validate_preamble(content).await
        } else {
            guardrail.validate(content).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PassGuardrail;

    #[async_trait::async_trait]
    impl Guardrail for PassGuardrail {
        fn name(&self) -> &str {
            "pass"
        }
        async fn validate(&self, _: &Content) -> GuardrailResult {
            GuardrailResult::Pass
        }
    }

    struct FailGuardrail {
        severity: Severity,
    }

    #[async_trait::async_trait]
    impl Guardrail for FailGuardrail {
        fn name(&self) -> &str {
            "fail"
        }
        async fn validate(&self, _: &Content) -> GuardrailResult {
            GuardrailResult::Fail { reason: "test failure".into(), severity: self.severity }
        }
    }

    #[tokio::test]
    async fn test_empty_guardrails_pass() {
        let set = GuardrailSet::new();
        let content = Content::new("user").with_text("hello");
        let result = GuardrailExecutor::run(&set, &content).await.unwrap();
        assert!(result.passed);
    }

    #[tokio::test]
    async fn test_pass_guardrail() {
        let set = GuardrailSet::new().with(PassGuardrail);
        let content = Content::new("user").with_text("hello");
        let result = GuardrailExecutor::run(&set, &content).await.unwrap();
        assert!(result.passed);
    }

    #[tokio::test]
    async fn test_fail_guardrail_low_severity() {
        let set = GuardrailSet::new().with(FailGuardrail { severity: Severity::Low });
        let content = Content::new("user").with_text("hello");
        let result = GuardrailExecutor::run(&set, &content).await.unwrap();
        assert!(result.passed); // Low severity doesn't fail
        assert_eq!(result.failures.len(), 1);
    }

    #[tokio::test]
    async fn test_fail_guardrail_high_severity() {
        let set = GuardrailSet::new().with(FailGuardrail { severity: Severity::High });
        let content = Content::new("user").with_text("hello");
        let result = GuardrailExecutor::run(&set, &content).await.unwrap();
        assert!(!result.passed);
    }

    #[tokio::test]
    async fn enforce_passes_low_severity_failures_as_warnings() {
        let set = GuardrailSet::new().with(FailGuardrail { severity: Severity::Low });
        let content = Content::new("model").with_text("hello");
        let screened = GuardrailExecutor::enforce(&set, &content, GuardrailPhase::Output, false)
            .await
            .unwrap();
        assert_eq!(screened.content.parts, content.parts);
        assert_eq!(
            screened.warnings,
            vec![GuardrailFailure {
                guardrail: "fail".into(),
                reason: "test failure".into(),
                severity: Severity::Low,
            }]
        );
    }

    #[tokio::test]
    async fn enforce_blocks_a_critical_failure_with_its_phase() {
        let set = GuardrailSet::new().with(FailGuardrail { severity: Severity::Critical });
        let content = Content::new("user").with_text("hello");
        let error = GuardrailExecutor::enforce(&set, &content, GuardrailPhase::Input, false)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            GuardrailError::Blocked { phase: GuardrailPhase::Input, ref failures }
                if failures.len() == 1 && failures[0].severity == Severity::Critical
        ));
    }

    #[tokio::test]
    async fn test_critical_early_exit() {
        let set = GuardrailSet::new().with(FailGuardrail { severity: Severity::Critical });
        let content = Content::new("user").with_text("hello");
        let result = GuardrailExecutor::run(&set, &content).await;
        assert!(result.is_err());
    }
}
