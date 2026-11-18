//! Repeated runs of an eval set: per-case pass rate, score statistics and flakiness.
//!
//! An agent backed by a sampling model can pass a case once and fail it the next time. One
//! run cannot tell a reliable case from a lucky one, so
//! [`Evaluator::evaluate_test_file_runs`](crate::Evaluator::evaluate_test_file_runs) runs the
//! whole eval set `num_runs` times, each case in a fresh session every time, and
//! [`MultiRunReport`] summarises the runs per case:
//!
//! | Field | Meaning |
//! |-------|---------|
//! | [`pass_rate`](CaseRunSummary::pass_rate) | Runs in which the case passed, divided by the runs completed |
//! | [`scores`](CaseRunSummary::scores) | Mean, sample variance, standard deviation, minimum and maximum of each criterion's score |
//! | [`flaky`](CaseRunSummary::flaky) | The case passed in some runs and failed in others |
//! | [`passed`](CaseRunSummary::passed) | The pass rate reached [`MultiRunConfig::min_pass_rate`] |
//!
//! [`MultiRunReport::baseline_metrics`] gives the mean score of every criterion and the
//! [`PASS_RATE_METRIC`] per case, so a [`BaselineStore`](crate::BaselineStore) regression check
//! catches a case that became flaky as well as one whose scores dropped.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::Agent;
//! use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator, MultiRunConfig, TestFile};
//!
//! async fn check(agent: Arc<dyn Agent>) -> adk_eval::Result<()> {
//!     let evaluator = Evaluator::new(EvaluationConfig::with_criteria(
//!         EvaluationCriteria::exact_tools(),
//!     ));
//!     let test_file = TestFile::load("tests/weather.test.json")?;
//!     let config = MultiRunConfig::new(5)?.with_min_pass_rate(0.8)?;
//!     let report = evaluator.evaluate_test_file_runs(agent, &test_file, &config).await?;
//!     for case in report.flaky_cases() {
//!         eprintln!("{} passed {} of {} runs", case.eval_id, case.passed_runs, case.runs);
//!     }
//!     assert!(report.all_passed(), "{}", report.format_summary());
//!     Ok(())
//! }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{EvalError, Result};
use crate::report::{EvaluationReport, EvaluationResult, Failure};

/// The baseline metric that holds a case's pass rate across runs.
pub const PASS_RATE_METRIC: &str = "pass_rate";

/// How many times to run an eval set, and how often a case must pass.
///
/// # Example
///
/// ```
/// use adk_eval::MultiRunConfig;
///
/// let config = MultiRunConfig::new(3)?.with_min_pass_rate(2.0 / 3.0)?;
/// assert_eq!(config.num_runs, 3);
/// assert!(MultiRunConfig::new(0).is_err());
/// # Ok::<(), adk_eval::EvalError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MultiRunConfig {
    /// Runs of the whole eval set, at least 1.
    pub num_runs: usize,
    /// Fraction of runs, from 0.0 to 1.0, a case must pass to pass overall. The default,
    /// 1.0, requires every run to pass.
    pub min_pass_rate: f64,
}

impl MultiRunConfig {
    /// Runs the eval set `num_runs` times, requiring every run of a case to pass.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when `num_runs` is 0.
    pub fn new(num_runs: usize) -> Result<Self> {
        if num_runs == 0 {
            return Err(EvalError::ConfigError(
                "num_runs must be at least 1; one run is a plain evaluation".to_string(),
            ));
        }
        Ok(Self { num_runs, min_pass_rate: 1.0 })
    }

    /// Sets the fraction of runs a case must pass.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when `min_pass_rate` is not between 0.0 and 1.0.
    pub fn with_min_pass_rate(mut self, min_pass_rate: f64) -> Result<Self> {
        if !(0.0..=1.0).contains(&min_pass_rate) {
            return Err(EvalError::ConfigError(format!(
                "min_pass_rate must be between 0.0 and 1.0, got {min_pass_rate}"
            )));
        }
        self.min_pass_rate = min_pass_rate;
        Ok(self)
    }
}

/// Statistics of one criterion's score across runs.
///
/// # Example
///
/// ```
/// use adk_eval::ScoreStats;
///
/// let stats = ScoreStats::from_scores(&[1.0, 0.5, 0.0]).unwrap();
/// assert_eq!(stats.mean, 0.5);
/// assert_eq!(stats.variance, 0.25); // sample variance, n - 1 in the denominator
/// assert_eq!(stats.std_dev, 0.5);
/// assert!(ScoreStats::from_scores(&[]).is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScoreStats {
    /// Runs in which the criterion was scored.
    pub count: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Sample variance, with `count - 1` in the denominator; 0.0 for a single score.
    pub variance: f64,
    /// Square root of the variance.
    pub std_dev: f64,
    /// Lowest score.
    pub min: f64,
    /// Highest score.
    pub max: f64,
}

impl ScoreStats {
    /// Computes the statistics of `scores`, or `None` when there are none.
    pub fn from_scores(scores: &[f64]) -> Option<Self> {
        if scores.is_empty() {
            return None;
        }
        let count = scores.len();
        let mean = scores.iter().sum::<f64>() / count as f64;
        let variance = if count > 1 {
            scores.iter().map(|score| (score - mean).powi(2)).sum::<f64>() / (count - 1) as f64
        } else {
            0.0
        };
        Some(Self {
            count,
            mean,
            variance,
            std_dev: variance.sqrt(),
            min: scores.iter().copied().fold(f64::INFINITY, f64::min),
            max: scores.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        })
    }
}

/// One case across every completed run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CaseRunSummary {
    /// The case's `eval_id`.
    pub eval_id: String,
    /// Runs in which the case ran.
    pub runs: usize,
    /// Runs in which the case passed.
    pub passed_runs: usize,
    /// `passed_runs / runs`.
    pub pass_rate: f64,
    /// `true` when the case passed in some runs and failed in others.
    pub flaky: bool,
    /// `true` when the pass rate reached the report's minimum.
    pub passed: bool,
    /// Statistics of each criterion's score, by criterion name.
    pub scores: BTreeMap<String, ScoreStats>,
    /// Runs in which each criterion failed, by criterion name.
    pub failed_criteria: BTreeMap<String, usize>,
}

/// The result of running an eval set several times.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MultiRunReport {
    /// The eval set's id.
    pub eval_set_id: String,
    /// Runs requested.
    pub requested_runs: usize,
    /// Fraction of runs a case must pass.
    pub min_pass_rate: f64,
    /// One summary per case, in the order the cases first appeared.
    pub cases: Vec<CaseRunSummary>,
    /// The report of every completed run, in order.
    pub runs: Vec<EvaluationReport>,
    /// Why the runs stopped before `requested_runs` completed: a budget, spend cap or kill
    /// switch refusal. The run it interrupted is not counted.
    pub stopped: Option<String>,
    /// When the first run started.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Wall time of every completed run.
    pub duration: Duration,
}

impl MultiRunReport {
    /// Summarises the reports of completed runs of one eval set.
    ///
    /// A case passes when it passed in at least `min_pass_rate` of the runs it ran in. A
    /// case that errored in a run counts as failing that run.
    ///
    /// # Example
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use std::time::Duration;
    ///
    /// use adk_eval::{EvaluationReport, EvaluationResult, MultiRunReport};
    ///
    /// let run = |passed: bool| {
    ///     let scores = HashMap::from([("response_similarity".to_string(), if passed { 1.0 } else { 0.4 })]);
    ///     let result = if passed {
    ///         EvaluationResult::passed("weather", scores, Duration::ZERO)
    ///     } else {
    ///         EvaluationResult::failed("weather", scores, vec![], Duration::ZERO)
    ///     };
    ///     EvaluationReport::new("run", vec![result], chrono::Utc::now())
    /// };
    /// let report = MultiRunReport::from_runs("weather", 3, 1.0, vec![run(true), run(false), run(true)]);
    /// let case = &report.cases[0];
    /// assert_eq!((case.passed_runs, case.runs, case.flaky, case.passed), (2, 3, true, false));
    /// assert!(!report.all_passed());
    /// ```
    pub fn from_runs(
        eval_set_id: &str,
        requested_runs: usize,
        min_pass_rate: f64,
        runs: Vec<EvaluationReport>,
    ) -> Self {
        let mut order: Vec<String> = Vec::new();
        let mut by_case: HashMap<String, Vec<&EvaluationResult>> = HashMap::new();
        for result in runs.iter().flat_map(|run| &run.results) {
            by_case
                .entry(result.eval_id.clone())
                .or_insert_with(|| {
                    order.push(result.eval_id.clone());
                    Vec::new()
                })
                .push(result);
        }

        let cases = order
            .into_iter()
            .map(|eval_id| {
                let results = &by_case[&eval_id];
                summarise_case(eval_id, results, min_pass_rate)
            })
            .collect();

        Self {
            eval_set_id: eval_set_id.to_string(),
            requested_runs,
            min_pass_rate,
            cases,
            started_at: runs.first().map_or_else(chrono::Utc::now, |run| run.started_at),
            duration: runs.iter().map(|run| run.duration).sum(),
            runs,
            stopped: None,
        }
    }

    /// Records why the runs stopped early.
    pub fn with_stopped(mut self, reason: impl Into<String>) -> Self {
        self.stopped = Some(reason.into());
        self
    }

    /// Runs that completed.
    pub fn completed_runs(&self) -> usize {
        self.runs.len()
    }

    /// `true` when every requested run completed and every case reached the minimum pass
    /// rate.
    pub fn all_passed(&self) -> bool {
        self.stopped.is_none()
            && self.completed_runs() == self.requested_runs
            && !self.cases.is_empty()
            && self.cases.iter().all(|case| case.passed)
    }

    /// The cases that passed in some runs and failed in others.
    pub fn flaky_cases(&self) -> impl Iterator<Item = &CaseRunSummary> {
        self.cases.iter().filter(|case| case.flaky)
    }

    /// Metric name → case id → value, the shape [`BaselineStore`](crate::BaselineStore)
    /// stores: the mean score of every criterion and the case's [`PASS_RATE_METRIC`].
    pub fn baseline_metrics(&self) -> HashMap<String, HashMap<String, f64>> {
        let mut metrics: HashMap<String, HashMap<String, f64>> = HashMap::new();
        for case in &self.cases {
            for (criterion, stats) in &case.scores {
                metrics
                    .entry(criterion.clone())
                    .or_default()
                    .insert(case.eval_id.clone(), stats.mean);
            }
            metrics
                .entry(PASS_RATE_METRIC.to_string())
                .or_default()
                .insert(case.eval_id.clone(), case.pass_rate);
        }
        metrics
    }

    /// One result per case for reporters that take an [`EvaluationReport`], such as the
    /// JUnit reporter: it passes when the case reached the minimum pass rate, carries the
    /// mean scores and the pass rate, and fails with a `pass_rate` failure otherwise.
    pub fn case_results(&self) -> Vec<EvaluationResult> {
        self.cases
            .iter()
            .map(|case| {
                let mut scores: HashMap<String, f64> =
                    case.scores.iter().map(|(name, stats)| (name.clone(), stats.mean)).collect();
                scores.insert(PASS_RATE_METRIC.to_string(), case.pass_rate);
                let duration = self
                    .runs
                    .iter()
                    .flat_map(|run| &run.results)
                    .filter(|result| result.eval_id == case.eval_id)
                    .map(|result| result.duration)
                    .sum();
                if case.passed {
                    return EvaluationResult::passed(&case.eval_id, scores, duration);
                }
                let failing = case
                    .failed_criteria
                    .iter()
                    .map(|(criterion, runs)| format!("{criterion} in {runs}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let failure = Failure::new(
                    PASS_RATE_METRIC,
                    number(self.min_pass_rate),
                    number(case.pass_rate),
                    case.pass_rate,
                    self.min_pass_rate,
                )
                .with_details(&format!(
                    "passed {} of {} runs{}; failed criteria: {failing}",
                    case.passed_runs,
                    case.runs,
                    if case.flaky { " (flaky)" } else { "" }
                ));
                EvaluationResult::failed(&case.eval_id, scores, vec![failure], duration)
            })
            .collect()
    }

    /// A human-readable summary: one line per case with its pass rate, then the score
    /// statistics of each criterion.
    pub fn format_summary(&self) -> String {
        use std::fmt::Write;

        let passed = self.cases.iter().filter(|case| case.passed).count();
        let flaky = self.flaky_cases().count();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Runs: {}/{}  Cases: {}  Passed: {passed}  Failed: {}  Flaky: {flaky}  Min pass rate: {:.2}",
            self.completed_runs(),
            self.requested_runs,
            self.cases.len(),
            self.cases.len() - passed,
            self.min_pass_rate
        );
        if let Some(reason) = &self.stopped {
            let _ = writeln!(out, "Stopped early: {reason}");
        }
        let _ = writeln!(out, "\n{:<28} {:>10} {:>9}  Verdict", "Case", "Passed", "Rate");
        for case in &self.cases {
            let verdict = match (case.passed, case.flaky) {
                (true, false) => "PASS",
                (true, true) => "PASS (flaky)",
                (false, true) => "FAIL (flaky)",
                (false, false) => "FAIL",
            };
            let _ = writeln!(
                out,
                "{:<28} {:>10} {:>9.2}  {verdict}",
                case.eval_id,
                format!("{}/{}", case.passed_runs, case.runs),
                case.pass_rate
            );
        }

        let mut by_criterion: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        for result in self.runs.iter().flat_map(|run| &run.results) {
            for (criterion, &score) in &result.scores {
                by_criterion.entry(criterion.as_str()).or_default().push(score);
            }
        }
        if !by_criterion.is_empty() {
            let _ = writeln!(
                out,
                "\n{:<28} {:>8} {:>8} {:>8} {:>8}",
                "Criterion", "Mean", "Std", "Min", "Max"
            );
            for (criterion, scores) in by_criterion {
                if let Some(stats) = ScoreStats::from_scores(&scores) {
                    let _ = writeln!(
                        out,
                        "{criterion:<28} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
                        stats.mean, stats.std_dev, stats.min, stats.max
                    );
                }
            }
        }
        let _ = writeln!(out, "\nDuration: {:.2}s", self.duration.as_secs_f64());
        out
    }

    /// Serializes the report as pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when a score cannot be serialized.
    pub fn to_json(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

fn summarise_case(
    eval_id: String,
    results: &[&EvaluationResult],
    min_pass_rate: f64,
) -> CaseRunSummary {
    let runs = results.len();
    let passed_runs = results.iter().filter(|result| result.passed).count();
    let pass_rate = if runs == 0 { 0.0 } else { passed_runs as f64 / runs as f64 };

    let mut per_criterion: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut failed_criteria: BTreeMap<String, usize> = BTreeMap::new();
    for result in results {
        for (criterion, &score) in &result.scores {
            per_criterion.entry(criterion.clone()).or_default().push(score);
        }
        let mut failed: Vec<&str> =
            result.failures.iter().map(|failure| failure.criterion.as_str()).collect();
        failed.sort_unstable();
        failed.dedup();
        for criterion in failed {
            *failed_criteria.entry(criterion.to_string()).or_default() += 1;
        }
    }

    CaseRunSummary {
        eval_id,
        runs,
        passed_runs,
        pass_rate,
        flaky: passed_runs > 0 && passed_runs < runs,
        // Compared in whole runs, so 2 of 3 meets a minimum of 2/3 despite rounding.
        passed: runs > 0 && passed_runs as f64 + 1e-9 >= min_pass_rate * runs as f64,
        scores: per_criterion
            .into_iter()
            .filter_map(|(criterion, scores)| {
                ScoreStats::from_scores(&scores).map(|stats| (criterion, stats))
            })
            .collect(),
        failed_criteria,
    }
}

fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(eval_id: &str, passed: bool, score: f64) -> EvaluationResult {
        let scores = HashMap::from([("tool_trajectory".to_string(), score)]);
        if passed {
            EvaluationResult::passed(eval_id, scores, Duration::from_millis(10))
        } else {
            let failure = Failure::new("tool_trajectory", Value::Null, Value::Null, score, 1.0);
            EvaluationResult::failed(eval_id, scores, vec![failure], Duration::from_millis(10))
        }
    }

    fn run(results: Vec<EvaluationResult>) -> EvaluationReport {
        EvaluationReport::new("run", results, chrono::Utc::now())
    }

    #[test]
    fn pass_rates_count_runs_per_case() {
        let report = MultiRunReport::from_runs(
            "set",
            3,
            1.0,
            vec![
                run(vec![result("steady", true, 1.0), result("flaky", true, 1.0)]),
                run(vec![result("steady", true, 1.0), result("flaky", false, 0.0)]),
                run(vec![result("steady", true, 1.0), result("flaky", true, 1.0)]),
            ],
        );

        let steady = &report.cases[0];
        assert_eq!(
            (steady.eval_id.as_str(), steady.passed_runs, steady.runs, steady.flaky, steady.passed),
            ("steady", 3, 3, false, true)
        );
        let flaky = &report.cases[1];
        assert_eq!((flaky.passed_runs, flaky.runs, flaky.flaky, flaky.passed), (2, 3, true, false));
        assert!((flaky.pass_rate - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(flaky.failed_criteria, BTreeMap::from([("tool_trajectory".to_string(), 1)]));
        let stats = &flaky.scores["tool_trajectory"];
        assert!((stats.mean - 2.0 / 3.0).abs() < 1e-12);
        assert!((stats.variance - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!((stats.min, stats.max, stats.count), (0.0, 1.0, 3));
        assert_eq!(
            report.flaky_cases().map(|case| case.eval_id.as_str()).collect::<Vec<_>>(),
            ["flaky"]
        );
        assert!(!report.all_passed());
    }

    #[test]
    fn a_minimum_pass_rate_admits_flaky_cases_in_whole_runs() {
        let runs = || {
            vec![
                run(vec![result("flaky", true, 1.0)]),
                run(vec![result("flaky", false, 0.0)]),
                run(vec![result("flaky", true, 1.0)]),
            ]
        };
        assert!(MultiRunReport::from_runs("set", 3, 2.0 / 3.0, runs()).all_passed());
        assert!(!MultiRunReport::from_runs("set", 3, 0.7, runs()).all_passed());
    }

    #[test]
    fn baselines_hold_mean_scores_and_pass_rates() {
        let report = MultiRunReport::from_runs(
            "set",
            2,
            1.0,
            vec![run(vec![result("case", true, 1.0)]), run(vec![result("case", false, 0.5)])],
        );
        let metrics = report.baseline_metrics();
        assert_eq!(
            metrics,
            HashMap::from([
                ("tool_trajectory".to_string(), HashMap::from([("case".to_string(), 0.75)])),
                (PASS_RATE_METRIC.to_string(), HashMap::from([("case".to_string(), 0.5)])),
            ])
        );
    }

    #[test]
    fn a_report_that_stopped_early_or_ran_nothing_does_not_pass() {
        let complete = || vec![run(vec![result("case", true, 1.0)])];
        assert!(MultiRunReport::from_runs("set", 1, 1.0, complete()).all_passed());
        assert!(!MultiRunReport::from_runs("set", 2, 1.0, complete()).all_passed());
        assert!(
            !MultiRunReport::from_runs("set", 1, 1.0, complete())
                .with_stopped("budget spent")
                .all_passed()
        );
        assert!(!MultiRunReport::from_runs("set", 1, 1.0, Vec::new()).all_passed());
    }

    #[test]
    fn case_results_fail_below_the_minimum_with_the_pass_rate() {
        let report = MultiRunReport::from_runs(
            "set",
            2,
            1.0,
            vec![run(vec![result("case", true, 1.0)]), run(vec![result("case", false, 0.0)])],
        );
        let results = report.case_results();
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
        assert_eq!(results[0].scores[PASS_RATE_METRIC], 0.5);
        assert_eq!(results[0].failures[0].criterion, PASS_RATE_METRIC);
        assert!(
            results[0].failures[0]
                .details
                .as_deref()
                .is_some_and(|details| details.contains("passed 1 of 2 runs (flaky)"))
        );
    }

    #[test]
    fn configs_reject_impossible_values() {
        assert!(MultiRunConfig::new(0).is_err());
        let config = MultiRunConfig::new(2).unwrap();
        assert!(config.clone().with_min_pass_rate(1.5).is_err());
        assert!(config.clone().with_min_pass_rate(f64::NAN).is_err());
        assert_eq!(config.with_min_pass_rate(0.5).unwrap().min_pass_rate, 0.5);
    }
}
