//! Evaluation result reporting
//!
//! Structures for representing and formatting evaluation results.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Complete evaluation report for a test file or eval set
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    /// Unique identifier for this evaluation run
    pub run_id: String,
    /// When the evaluation started
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the evaluation completed
    pub completed_at: chrono::DateTime<chrono::Utc>,
    /// Total duration
    pub duration: Duration,
    /// Results for each test case
    pub results: Vec<EvaluationResult>,
    /// Summary statistics
    pub summary: EvaluationSummary,
}

impl EvaluationReport {
    /// Create a new report
    pub fn new(
        run_id: &str,
        results: Vec<EvaluationResult>,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let completed_at = chrono::Utc::now();
        let duration = (completed_at - started_at).to_std().unwrap_or_default();
        let summary = EvaluationSummary::from_results(&results);

        Self { run_id: run_id.to_string(), started_at, completed_at, duration, results, summary }
    }

    /// Check if all tests passed
    pub fn all_passed(&self) -> bool {
        self.summary.failed == 0
    }

    /// Get failed results only
    pub fn failures(&self) -> Vec<&EvaluationResult> {
        self.results.iter().filter(|r| !r.passed).collect()
    }

    /// Format as a human-readable string
    pub fn format_summary(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!("Evaluation Report: {}\n", self.run_id));
        output.push_str(&format!("Duration: {:?}\n", self.duration));
        output.push_str("\nSummary:\n");
        output.push_str(&format!("  Total: {}\n", self.summary.total));
        output.push_str(&format!("  Passed: {}\n", self.summary.passed));
        output.push_str(&format!("  Failed: {}\n", self.summary.failed));
        output.push_str(&format!("  Pass Rate: {:.1}%\n", self.summary.pass_rate * 100.0));

        if !self.summary.avg_scores.is_empty() {
            output.push_str("\nAverage Scores:\n");
            for (criterion, score) in &self.summary.avg_scores {
                output.push_str(&format!("  {}: {:.3}\n", criterion, score));
            }
        }

        if self.summary.failed > 0 {
            output.push_str("\nFailed Tests:\n");
            for result in self.failures() {
                output.push_str(&format!(
                    "  - {} ({})\n",
                    result.eval_id,
                    result
                        .failures
                        .iter()
                        .map(|f| f.criterion.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }

        output
    }

    /// Export to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Summary statistics for an evaluation run
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluationSummary {
    /// Total number of test cases
    pub total: usize,
    /// Number of passed test cases
    pub passed: usize,
    /// Number of failed test cases
    pub failed: usize,
    /// Pass rate (0.0 - 1.0)
    pub pass_rate: f64,
    /// Average scores by criterion
    pub avg_scores: HashMap<String, f64>,
}

impl EvaluationSummary {
    /// Calculate summary from results
    pub fn from_results(results: &[EvaluationResult]) -> Self {
        let total = results.len();
        let passed = results.iter().filter(|r| r.passed).count();
        let failed = total - passed;
        let pass_rate = if total > 0 { passed as f64 / total as f64 } else { 0.0 };

        // Calculate average scores
        let mut score_sums: HashMap<String, (f64, usize)> = HashMap::new();
        for result in results {
            for (criterion, score) in &result.scores {
                let entry = score_sums.entry(criterion.clone()).or_insert((0.0, 0));
                entry.0 += score;
                entry.1 += 1;
            }
        }

        let avg_scores =
            score_sums.into_iter().map(|(k, (sum, count))| (k, sum / count as f64)).collect();

        Self { total, passed, failed, pass_rate, avg_scores }
    }
}

/// Result for a single test case
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    /// Test case identifier
    pub eval_id: String,
    /// Whether the test passed all criteria
    pub passed: bool,
    /// Scores for each criterion
    pub scores: HashMap<String, f64>,
    /// Failures (criteria that didn't meet threshold)
    pub failures: Vec<Failure>,
    /// Execution duration
    pub duration: Duration,
    /// Detailed turn results
    #[serde(default)]
    pub turn_results: Vec<TurnResult>,
}

impl EvaluationResult {
    /// Create a passed result
    pub fn passed(eval_id: &str, scores: HashMap<String, f64>, duration: Duration) -> Self {
        Self {
            eval_id: eval_id.to_string(),
            passed: true,
            scores,
            failures: vec![],
            duration,
            turn_results: vec![],
        }
    }

    /// Create a failed result
    pub fn failed(
        eval_id: &str,
        scores: HashMap<String, f64>,
        failures: Vec<Failure>,
        duration: Duration,
    ) -> Self {
        Self {
            eval_id: eval_id.to_string(),
            passed: false,
            scores,
            failures,
            duration,
            turn_results: vec![],
        }
    }

    /// Add turn results
    pub fn with_turn_results(mut self, turn_results: Vec<TurnResult>) -> Self {
        self.turn_results = turn_results;
        self
    }
}

/// A single failure in evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    /// Criterion that failed
    pub criterion: String,
    /// Expected value
    pub expected: Value,
    /// Actual value
    pub actual: Value,
    /// Score achieved
    pub score: f64,
    /// Threshold required
    pub threshold: f64,
    /// Additional details
    #[serde(default)]
    pub details: Option<String>,
}

impl Failure {
    /// Create a new failure
    pub fn new(
        criterion: &str,
        expected: Value,
        actual: Value,
        score: f64,
        threshold: f64,
    ) -> Self {
        Self { criterion: criterion.to_string(), expected, actual, score, threshold, details: None }
    }

    /// Add details
    pub fn with_details(mut self, details: &str) -> Self {
        self.details = Some(details.to_string());
        self
    }

    /// Format as human-readable string
    pub fn format(&self) -> String {
        let mut s = format!(
            "{}: score {:.3} < threshold {:.3}",
            self.criterion, self.score, self.threshold
        );
        if let Some(details) = &self.details {
            s.push_str(&format!("\n  Details: {}", details));
        }
        s
    }
}

/// Result for a single conversation turn
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    /// Turn/invocation identifier
    pub invocation_id: String,
    /// Actual response from the agent
    pub actual_response: Option<String>,
    /// Expected response
    pub expected_response: Option<String>,
    /// Actual tool calls made
    pub actual_tool_calls: Vec<crate::schema::ToolUse>,
    /// Expected tool calls
    pub expected_tool_calls: Vec<crate::schema::ToolUse>,
    /// Scores for this turn
    pub scores: HashMap<String, f64>,
}

/// Result for a single test case (alias for backward compatibility)
pub type TestCaseResult = EvaluationResult;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluation_summary() {
        let results = vec![
            EvaluationResult::passed(
                "test_1",
                HashMap::from([("tool_trajectory".to_string(), 1.0)]),
                Duration::from_millis(100),
            ),
            EvaluationResult::passed(
                "test_2",
                HashMap::from([("tool_trajectory".to_string(), 0.8)]),
                Duration::from_millis(150),
            ),
            EvaluationResult::failed(
                "test_3",
                HashMap::from([("tool_trajectory".to_string(), 0.5)]),
                vec![Failure::new("tool_trajectory", Value::Null, Value::Null, 0.5, 0.8)],
                Duration::from_millis(200),
            ),
        ];

        let summary = EvaluationSummary::from_results(&results);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.failed, 1);
        assert!((summary.pass_rate - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_failure_format() {
        let failure = Failure::new(
            "response_similarity",
            Value::String("expected".to_string()),
            Value::String("actual".to_string()),
            0.6,
            0.8,
        )
        .with_details("Responses differ significantly");

        let formatted = failure.format();
        assert!(formatted.contains("response_similarity"));
        assert!(formatted.contains("0.600"));
        assert!(formatted.contains("0.800"));
    }
}
