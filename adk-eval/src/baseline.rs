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
//! // Save current metrics as baseline
//! let mut metrics = HashMap::new();
//! let mut case_metrics = HashMap::new();
//! case_metrics.insert("accuracy".to_string(), 0.95);
//! metrics.insert("case_1".to_string(), case_metrics);
//! store.save("my_eval_set", &metrics).unwrap();
//!
//! // Check for regressions on a later run
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regression {
    /// Name of the metric that regressed
    pub metric_name: String,
    /// Identifier of the case that regressed
    pub case_id: String,
    /// Score from the baseline
    pub baseline_value: f64,
    /// Score from the current run
    pub current_value: f64,
    /// Difference (baseline - current)
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
    /// A regression is detected when `baseline_value - current_value > tolerance`.
    /// If no baseline file exists, returns an empty vector (no regressions).
    pub fn check_regressions(
        &self,
        current: &HashMap<String, HashMap<String, f64>>,
        tolerance: f64,
    ) -> Result<Vec<Regression>> {
        let baseline = match self.load()? {
            Some(b) => b,
            None => {
                tracing::info!(
                    "no baseline file found at {:?}, skipping regression check",
                    self.path
                );
                return Ok(Vec::new());
            }
        };

        let mut regressions = Vec::new();

        for (metric_name, baseline_cases) in &baseline.metrics {
            if let Some(current_cases) = current.get(metric_name) {
                for (case_id, &baseline_value) in baseline_cases {
                    if let Some(&current_value) = current_cases.get(case_id) {
                        let delta = baseline_value - current_value;
                        if delta > tolerance {
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
            }
        }

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
    fn test_check_regressions_no_baseline() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let current = sample_metrics();

        let regressions = store.check_regressions(&current, 0.05).unwrap();
        assert!(regressions.is_empty());
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
        assert!((reg.current_value - 0.80).abs() < f64::EPSILON);
        assert!((reg.delta - 0.15).abs() < 1e-10);
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
