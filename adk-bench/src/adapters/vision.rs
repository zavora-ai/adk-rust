//! The vision adapter (spec 3.7 §7.17): a backend's pass rate on a vision eval set under
//! `adk-eval`'s vision thresholds, beside the τ²-bench and BFCL adapters.

use std::sync::Arc;

use adk_eval::vision::VisionCriteria;
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator};
use async_trait::async_trait;

use super::{CaseResult, TaskQualityAdapter, TaskQualityResult};
use crate::error::{BenchError, Result};
use crate::vision::{VisionBackend, VisionWorkload};

/// One backend on one vision eval set: a case passes when every threshold holds.
///
/// # Example
///
/// ```no_run
/// use adk_bench::adapters::TaskQualityAdapter;
/// use adk_bench::adapters::vision::VisionAdapter;
/// use adk_bench::vision::{FixtureSet, backends, eval_dir};
/// use adk_eval::vision::VisionCriteria;
///
/// # async fn demo() -> adk_bench::Result<()> {
/// let backends::Loaded::Agent(backend) = backends::load_local("pp-ocrv6-tiny").await? else {
///     unreachable!("a reader answers eval cases");
/// };
/// let workload = FixtureSet::new(eval_dir()).workload("documents-read", backends::READ_SCORES)?;
/// let criteria = VisionCriteria { ocr_char_accuracy: Some(0.95), ..VisionCriteria::default() };
/// let result = VisionAdapter::new(backend, workload, criteria).run("pp-ocrv6-tiny").await?;
/// println!("{:.0}% of cases passed", result.accuracy * 100.0);
/// # Ok(())
/// # }
/// ```
pub struct VisionAdapter {
    backend: VisionBackend,
    workload: VisionWorkload,
    criteria: VisionCriteria,
}

impl VisionAdapter {
    /// The backend, the eval set and the thresholds a case must meet.
    pub fn new(backend: VisionBackend, workload: VisionWorkload, criteria: VisionCriteria) -> Self {
        Self { backend, workload, criteria }
    }
}

#[async_trait]
impl TaskQualityAdapter for VisionAdapter {
    fn name(&self) -> &str {
        "vision"
    }

    /// Runs the eval set; `model` names the result (the backend's own models are fixed).
    async fn run(&self, model: &str) -> Result<TaskQualityResult> {
        let criteria = EvaluationCriteria::default().with_vision(self.criteria.clone());
        let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));
        let agent: Arc<dyn adk_core::Agent> = self.backend.agent.clone();
        let report = evaluator
            .evaluate_test_file(agent, &self.workload.eval_set)
            .await
            .map_err(|e| BenchError::Vision(format!("{}: {e}", self.backend.id)))?;
        let cases: Vec<CaseResult> = report
            .results
            .iter()
            .map(|result| {
                let score = if result.scores.is_empty() {
                    0.0
                } else {
                    result.scores.values().sum::<f64>() / result.scores.len() as f64
                };
                let details = (!result.passed).then(|| {
                    result
                        .failures
                        .iter()
                        .map(|f| f.details.clone().unwrap_or_else(|| f.criterion.clone()))
                        .collect::<Vec<_>>()
                        .join("; ")
                });
                CaseResult {
                    case_id: result.eval_id.clone(),
                    passed: result.passed,
                    score,
                    details,
                }
            })
            .collect();
        let passed_cases = cases.iter().filter(|c| c.passed).count();
        Ok(TaskQualityResult {
            adapter_name: self.name().to_string(),
            model: model.to_string(),
            total_cases: cases.len(),
            passed_cases,
            accuracy: passed_cases as f64 / cases.len().max(1) as f64,
            cases,
        })
    }
}
