//! Baseline storage for regression detection.
//!
//! Provides persistence of evaluation metric snapshots and comparison
//! against baselines to detect regressions.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::BaselineStore;
//! use std::collections::HashMap;
//!
//! let store = BaselineStore::new(".eval-baseline.json");
//!
//! // Save current metrics as baseline: metric name → case id → score
//! let mut metrics = HashMap::new();
//! let mut accuracy = HashMap::new();
//! accuracy.insert("case_1".to_string(), 0.95);
//! metrics.insert("accuracy".to_string(), accuracy);
//! store.save("my_eval_set", &metrics).unwrap();
//!
//! // Check for regressions on a later run; a missing baseline file is an error
//! let regressions = store.check_regressions(&metrics, 0.05).unwrap();
//! assert!(regressions.is_empty());
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{EvalError, Result};

/// Baseline file content containing metric snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    /// When the baseline was saved
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Identifier for the eval set
    pub eval_set_id: String,
    /// Per-case, per-metric scores: outer key is metric_name, inner key is case_id
    pub metrics: HashMap<String, HashMap<String, f64>>,
}

/// A regression detected between baseline and current run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regression {
    /// Name of the metric that regressed
    pub metric_name: String,
    /// Identifier of the case that regressed
    pub case_id: String,
    /// Score from the baseline
    pub baseline_value: f64,
    /// Score from the current run, or `None` when the current run has no score for
    /// this metric and case, for example because the case errored or was not run
    pub current_value: Option<f64>,
    /// Difference (baseline - current); the full `baseline_value` when `current_value`
    /// is `None`
    pub delta: f64,
}

/// Manages baseline persistence and regression detection.
pub struct BaselineStore {
    path: PathBuf,
}

impl BaselineStore {
    /// Create a new baseline store at the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Save metrics as a baseline.
    ///
    /// Writes the metrics map with a timestamp and eval set identifier
    /// to the configured path as pretty-printed JSON.
    pub fn save(
        &self,
        eval_set_id: &str,
        metrics: &HashMap<String, HashMap<String, f64>>,
    ) -> Result<()> {
        let baseline = Baseline {
            timestamp: chrono::Utc::now(),
            eval_set_id: eval_set_id.to_string(),
            metrics: metrics.clone(),
        };

        let json = serde_json::to_string_pretty(&baseline)
            .map_err(|e| EvalError::BaselineError(format!("failed to serialize baseline: {e}")))?;

        std::fs::write(&self.path, json)
            .map_err(|e| EvalError::BaselineError(format!("failed to write baseline file: {e}")))?;

        Ok(())
    }

    /// Load existing baseline.
    ///
    /// Returns `Ok(None)` if the baseline file does not exist.
    /// Returns an error only for actual I/O or parse failures.
    pub fn load(&self) -> Result<Option<Baseline>> {
        if !self.path.exists() {
            return Ok(None);
        }

        let contents = std::fs::read_to_string(&self.path)
            .map_err(|e| EvalError::BaselineError(format!("failed to read baseline file: {e}")))?;

        let baseline: Baseline = serde_json::from_str(&contents)
            .map_err(|e| EvalError::BaselineError(format!("failed to parse baseline file: {e}")))?;

        Ok(Some(baseline))
    }

    /// Compare current metrics against baseline and detect regressions.
    ///
    /// Every metric and case recorded in the baseline must be present in `current`.
    /// A regression is reported when `baseline_value - current_value > tolerance`, when
    /// the current score is NaN, or when the current run has no score for a baseline
    /// metric and case. Metrics and cases that appear only in `current` are ignored.
    /// Regressions are sorted by metric name, then case id.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::BaselineError`] when the baseline file does not exist, or
    /// cannot be read or parsed. A missing baseline is an error rather than an empty
    /// result, so a mistyped path or a baseline that was never committed cannot pass
    /// a regression gate.
    pub fn check_regressions(
        &self,
        current: &HashMap<String, HashMap<String, f64>>,
        tolerance: f64,
    ) -> Result<Vec<Regression>> {
        let Some(baseline) = self.load()? else {
            return Err(EvalError::BaselineError(format!(
                "no baseline file at {}, so there is nothing to check for regressions; \
                 save one first with BaselineStore::save (or `cargo adk eval --save-baseline`)",
                self.path.display()
            )));
        };

        let mut regressions = Vec::new();

        for (metric_name, baseline_cases) in &baseline.metrics {
            let current_cases = current.get(metric_name);
            for (case_id, &baseline_value) in baseline_cases {
                let current_value = current_cases.and_then(|cases| cases.get(case_id)).copied();
                let delta = baseline_value - current_value.unwrap_or(0.0);
                let regressed = match current_value {
                    Some(_) => delta > tolerance || delta.is_nan(),
                    None => true,
                };
                if regressed {
                    regressions.push(Regression {
                        metric_name: metric_name.clone(),
                        case_id: case_id.clone(),
                        baseline_value,
                        current_value,
                        delta,
                    });
                }
            }
        }

        regressions.sort_by(|a, b| {
            a.metric_name.cmp(&b.metric_name).then_with(|| a.case_id.cmp(&b.case_id))
        });
        Ok(regressions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> BaselineStore {
        let path = dir.path().join(".eval-baseline.json");
        BaselineStore::new(path)
    }

    fn sample_metrics() -> HashMap<String, HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        let mut accuracy = HashMap::new();
        accuracy.insert("case_1".to_string(), 0.95);
        accuracy.insert("case_2".to_string(), 0.88);
        metrics.insert("accuracy".to_string(), accuracy);

        let mut latency = HashMap::new();
        latency.insert("case_1".to_string(), 0.7);
        latency.insert("case_2".to_string(), 0.6);
        metrics.insert("latency".to_string(), latency);

        metrics
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        store.save("test_set", &metrics).unwrap();

        let loaded = store.load().unwrap().expect("baseline should exist");
        assert_eq!(loaded.eval_set_id, "test_set");
        assert_eq!(loaded.metrics, metrics);
    }

    #[test]
    fn test_load_returns_none_when_no_file() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);

        let result = store.load().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn missing_baseline_is_an_error_not_a_pass() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let current = sample_metrics();

        let err = store.check_regressions(&current, 0.05).unwrap_err();
        assert!(
            matches!(&err, EvalError::BaselineError(message) if message.contains("no baseline file")),
            "{err}"
        );
    }

    #[test]
    fn test_check_regressions_no_regression() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        store.save("test_set", &metrics).unwrap();

        // Same metrics — no regression
        let regressions = store.check_regressions(&metrics, 0.05).unwrap();
        assert!(regressions.is_empty());
    }

    #[test]
    fn test_check_regressions_detects_regression() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        store.save("test_set", &metrics).unwrap();

        // Drop case_1 accuracy from 0.95 to 0.80 (delta = 0.15, exceeds 0.05 tolerance)
        let mut current = metrics.clone();
        current.get_mut("accuracy").unwrap().insert("case_1".to_string(), 0.80);

        let regressions = store.check_regressions(&current, 0.05).unwrap();
        assert_eq!(regressions.len(), 1);

        let reg = &regressions[0];
        assert_eq!(reg.metric_name, "accuracy");
        assert_eq!(reg.case_id, "case_1");
        assert!((reg.baseline_value - 0.95).abs() < f64::EPSILON);
        assert_eq!(reg.current_value, Some(0.80));
        assert!((reg.delta - 0.15).abs() < 1e-10);
    }

    #[test]
    fn missing_case_is_a_regression() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        store.save("test_set", &sample_metrics()).unwrap();

        // case_2 errored in the current run, so it has no scores at all.
        let mut current = sample_metrics();
        for cases in current.values_mut() {
            cases.remove("case_2");
        }

        let regressions = store.check_regressions(&current, 0.05).unwrap();
        assert_eq!(
            regressions,
            vec![
                Regression {
                    metric_name: "accuracy".to_string(),
                    case_id: "case_2".to_string(),
                    baseline_value: 0.88,
                    current_value: None,
                    delta: 0.88,
                },
                Regression {
                    metric_name: "latency".to_string(),
                    case_id: "case_2".to_string(),
                    baseline_value: 0.6,
                    current_value: None,
                    delta: 0.6,
                },
            ]
        );
    }

    #[test]
    fn missing_metric_is_a_regression_even_at_zero_baseline() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let mut baseline = HashMap::new();
        baseline.insert("safety".to_string(), HashMap::from([("case_1".to_string(), 0.0)]));
        store.save("test_set", &baseline).unwrap();

        let regressions = store.check_regressions(&HashMap::new(), 0.05).unwrap();
        assert_eq!(
            regressions,
            vec![Regression {
                metric_name: "safety".to_string(),
                case_id: "case_1".to_string(),
                baseline_value: 0.0,
                current_value: None,
                delta: 0.0,
            }]
        );
    }

    #[test]
    fn new_cases_in_current_run_are_not_regressions() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        store.save("test_set", &sample_metrics()).unwrap();

        let mut current = sample_metrics();
        current.get_mut("accuracy").unwrap().insert("case_3".to_string(), 0.1);
        current.insert("coverage".to_string(), HashMap::from([("case_1".to_string(), 0.0)]));

        assert!(store.check_regressions(&current, 0.05).unwrap().is_empty());
    }

    #[test]
    fn test_check_regressions_within_tolerance() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        store.save("test_set", &metrics).unwrap();

        // Drop case_1 accuracy from 0.95 to 0.91 (delta = 0.04, within 0.05 tolerance)
        let mut current = metrics.clone();
        current.get_mut("accuracy").unwrap().insert("case_1".to_string(), 0.91);

        let regressions = store.check_regressions(&current, 0.05).unwrap();
        assert!(regressions.is_empty());
    }

    #[test]
    fn test_check_regressions_improvement_not_flagged() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        store.save("test_set", &metrics).unwrap();

        // Improve case_1 accuracy from 0.95 to 0.99 (negative delta — improvement)
        let mut current = metrics.clone();
        current.get_mut("accuracy").unwrap().insert("case_1".to_string(), 0.99);

        let regressions = store.check_regressions(&current, 0.05).unwrap();
        assert!(regressions.is_empty());
    }

    #[test]
    fn test_save_writes_pretty_json() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        store.save("test_set", &metrics).unwrap();

        let contents = std::fs::read_to_string(dir.path().join(".eval-baseline.json")).unwrap();
        // Pretty-printed JSON has newlines and indentation
        assert!(contents.contains('\n'));
        assert!(contents.contains("  "));
        // Verify it's valid JSON
        let _: serde_json::Value = serde_json::from_str(&contents).unwrap();
    }

    #[test]
    fn test_baseline_contains_timestamp() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metrics = sample_metrics();

        let before = chrono::Utc::now();
        store.save("test_set", &metrics).unwrap();
        let after = chrono::Utc::now();

        let loaded = store.load().unwrap().unwrap();
        assert!(loaded.timestamp >= before);
        assert!(loaded.timestamp <= after);
    }
}
