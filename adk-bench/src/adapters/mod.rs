//! Task quality adapters for established benchmark suites.
//!
//! Provides thin adapter interfaces for routing established benchmark
//! suite requests through the ADK-Rust agent runtime.
//!
//! # Adapters
//!
//! - **τ²-bench** (`tau2` feature) — Implements the τ²-bench agent interface
//!   protocol, translating between its format and ADK-Rust's Event/Content model.
//! - **BFCL** (`bfcl` feature) — Loads Berkeley Function Calling Leaderboard
//!   dataset entries and scores tool call accuracy.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::adapters::{TaskQualityAdapter, TaskQualityResult};
//!
//! async fn run_adapter(adapter: &dyn TaskQualityAdapter, model: &str) {
//!     let result = adapter.run(model).await.unwrap();
//!     println!("Accuracy: {:.1}%", result.accuracy * 100.0);
//! }
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// τ²-bench adapter (feature-gated behind `tau2`).
#[cfg(feature = "tau2")]
pub mod tau2;

/// BFCL adapter (feature-gated behind `bfcl`).
#[cfg(feature = "bfcl")]
pub mod bfcl;

/// Trait for task quality benchmark adapters.
///
/// Implementations route requests from established benchmark suites
/// (τ²-bench, BFCL) through the ADK-Rust agent runtime and report
/// accuracy/quality scores.
#[async_trait]
pub trait TaskQualityAdapter: Send + Sync {
    /// Returns the adapter name (e.g., "tau2", "bfcl").
    fn name(&self) -> &str;

    /// Runs the task quality suite and returns results.
    async fn run(&self, model: &str) -> crate::Result<TaskQualityResult>;
}

/// Aggregated results from running a task quality benchmark suite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskQualityResult {
    /// Name of the adapter that produced these results.
    pub adapter_name: String,
    /// Model used for the benchmark run.
    pub model: String,
    /// Total number of test cases executed.
    pub total_cases: usize,
    /// Number of test cases that passed.
    pub passed_cases: usize,
    /// Accuracy score (passed_cases / total_cases).
    pub accuracy: f64,
    /// Per-case results.
    pub cases: Vec<CaseResult>,
}

/// Result from a single test case in a task quality benchmark.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CaseResult {
    /// Unique identifier for the test case.
    pub case_id: String,
    /// Whether the test case passed.
    pub passed: bool,
    /// Score for the test case (0.0 to 1.0).
    pub score: f64,
    /// Optional details about the result (e.g., failure reason).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}
