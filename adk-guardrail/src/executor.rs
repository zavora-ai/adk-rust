use crate::{Guardrail, GuardrailError, GuardrailResult, Result, Severity};
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

/// Executor for running guardrails in parallel
pub struct GuardrailExecutor;

impl GuardrailExecutor {
    /// Run all guardrails in parallel, with early exit on critical failures
    pub async fn run(guardrails: &GuardrailSet, content: &Content) -> Result<ExecutionResult> {
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
                .map(|g| Self::run_single(Arc::clone(g), &current_content))
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
                    GuardrailResult::Transform { new_content, reason } => {
                        tracing::debug!(
                            guardrail = guardrail.name(),
                            reason = %reason,
                            "Content transformed"
                        );
                        current_content = new_content;
                    }
                }
            }
        }

        // Run sequential guardrails
        for guardrail in sequential {
            let result = Self::run_single(Arc::clone(guardrail), &current_content).await;
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
                GuardrailResult::Transform { new_content, reason } => {
                    tracing::debug!(
                        guardrail = guardrail.name(),
                        reason = %reason,
                        "Content transformed"
                    );
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

    async fn run_single(guardrail: Arc<dyn Guardrail>, content: &Content) -> GuardrailResult {
        guardrail.validate(content).await
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
    async fn test_critical_early_exit() {
        let set = GuardrailSet::new().with(FailGuardrail { severity: Severity::Critical });
        let content = Content::new("user").with_text("hello");
        let result = GuardrailExecutor::run(&set, &content).await;
        assert!(result.is_err());
    }
}
