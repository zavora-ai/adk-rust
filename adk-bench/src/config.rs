//! Benchmark configuration types mapped from CLI flags.
//!
//! This module defines [`BenchConfig`], the top-level configuration struct
//! that maps CLI parameters to structured settings for the [`BenchRunner`].
//! It also defines supporting types like [`OutputFormat`], [`TaskSuite`],
//! and [`ExternalFrameworkConfig`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Top-level benchmark configuration mapped from CLI flags.
///
/// Controls all aspects of benchmark execution including iteration count,
/// concurrency, output format, regression detection, and cost guards.
///
/// # Example
///
/// ```rust
/// use adk_bench::BenchConfig;
///
/// let config = BenchConfig {
///     model: "gemini-2.5-flash".to_string(),
///     runs: 10,
///     concurrency: 4,
///     ..Default::default()
/// };
/// assert_eq!(config.runs, 10);
/// assert_eq!(config.warmup, 3);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchConfig {
    /// Model identifier (e.g., "gemini-2.5-flash").
    pub model: String,

    /// Number of measurement iterations per workload.
    pub runs: usize,

    /// Agent concurrency level (1 = sequential).
    pub concurrency: usize,

    /// Specific workload to run (None = all built-in).
    pub workload: Option<String>,

    /// Output format for results.
    pub output_format: OutputFormat,

    /// Output file path (None = stdout).
    pub output_path: Option<PathBuf>,

    /// Warm-up iterations before measurement begins (discarded).
    pub warmup: usize,

    /// Whether to save results as baseline after the run.
    pub save_baseline: bool,

    /// Whether to check regression against a saved baseline.
    pub check_regression: bool,

    /// Maximum allowed relative degradation (default 0.10 = 10%).
    pub tolerance: f64,

    /// External framework configurations for comparison.
    pub external_frameworks: Vec<ExternalFrameworkConfig>,

    /// Timeout for external framework runs in seconds.
    pub external_timeout_secs: u64,

    /// Concurrency sweep levels (if sweep mode enabled).
    /// When set, the runner tests each level sequentially: e.g., [1, 2, 4, 8, 16, 32, 64].
    pub concurrency_sweep: Option<Vec<usize>>,

    /// Memory sampling interval in milliseconds.
    pub memory_sample_interval_ms: u64,

    /// Task quality suite to run (tau2, bfcl).
    pub suite: Option<TaskSuite>,

    /// Baseline file path for regression detection.
    pub baseline_path: PathBuf,

    /// Dry-run mode: compute and display estimated cost without executing API calls.
    pub dry_run: bool,

    /// Maximum allowed API cost in USD; abort if estimated cost exceeds this.
    pub max_cost_usd: Option<f64>,

    /// Skip interactive cost confirmation (auto-confirm when estimated cost > $1.00).
    pub confirm_cost: bool,

    /// Enable experimental workloads (e.g., multi-agent delegation).
    pub experimental: bool,
}

impl Default for BenchConfig {
    /// Creates a `BenchConfig` with documented defaults:
    ///
    /// - `model`: `"gemini-2.5-flash"`
    /// - `runs`: 5
    /// - `concurrency`: 1 (sequential)
    /// - `warmup`: 3
    /// - `tolerance`: 0.10 (10%)
    /// - `external_timeout_secs`: 300
    /// - `memory_sample_interval_ms`: 100
    /// - `output_format`: Table
    /// - `baseline_path`: `.bench-baseline.json`
    /// - `dry_run`: false
    /// - `max_cost_usd`: None
    /// - `confirm_cost`: false
    /// - `experimental`: false
    fn default() -> Self {
        Self {
            model: "gemini-2.5-flash".to_string(),
            runs: 5,
            concurrency: 1,
            workload: None,
            output_format: OutputFormat::Table,
            output_path: None,
            warmup: 3,
            save_baseline: false,
            check_regression: false,
            tolerance: 0.10,
            external_frameworks: Vec::new(),
            external_timeout_secs: 300,
            concurrency_sweep: None,
            memory_sample_interval_ms: 100,
            suite: None,
            baseline_path: PathBuf::from(".bench-baseline.json"),
            dry_run: false,
            max_cost_usd: None,
            confirm_cost: false,
            experimental: false,
        }
    }
}

/// Output format for benchmark results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Machine-readable JSON with all raw metrics.
    Json,
    /// Human-readable aligned table for terminal display.
    Table,
    /// Markdown table suitable for README inclusion.
    Markdown,
}

/// Task quality benchmark suite selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskSuite {
    /// τ²-bench task quality scenarios.
    Tau2,
    /// Berkeley Function Calling Leaderboard dataset.
    Bfcl,
}

/// Configuration for an external framework comparison target.
///
/// Describes how to invoke a competitor framework benchmark subprocess
/// that emits metrics in the External Benchmark Protocol (EBP) JSON format.
///
/// # Example
///
/// ```rust
/// use adk_bench::ExternalFrameworkConfig;
///
/// let config = ExternalFrameworkConfig {
///     name: "langgraph".to_string(),
///     command: "python".to_string(),
///     args: vec!["-m".to_string(), "bench_langgraph".to_string()],
///     working_dir: None,
///     env: vec![("PYTHONPATH".to_string(), "./src".to_string())],
/// };
/// assert_eq!(config.name, "langgraph");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalFrameworkConfig {
    /// Framework name (e.g., "adk-python", "langgraph", "crewai").
    pub name: String,

    /// Command to execute the framework benchmark.
    pub command: String,

    /// Arguments passed to the command.
    pub args: Vec<String>,

    /// Working directory for execution.
    pub working_dir: Option<PathBuf>,

    /// Environment variables to set for the subprocess.
    pub env: Vec<(String, String)>,
}
