//! # adk-bench
//!
//! A comprehensive benchmarking framework for ADK-Rust that measures
//! framework-level runtime performance using real LLM APIs.
//!
//! `adk-bench` isolates framework overhead from LLM latency through precise
//! per-call instrumentation, supports concurrent agent throughput testing,
//! memory profiling, and external framework comparison via subprocess
//! execution with a standardized JSON protocol (External Benchmark Protocol).
//!
//! ## Features
//!
//! - **Cold start measurement**: Binary launch to first LLM call timing
//! - **Agent loop overhead**: Per-turn framework processing latency (excluding LLM time)
//! - **Concurrent throughput**: Agents/second under Tokio async load
//! - **Memory footprint**: Platform-specific RSS sampling (Linux/macOS)
//! - **Tool invocation latency**: Deserialization, validation, and dispatch timing
//! - **Token overhead**: Framework-injected token cost analysis
//! - **External comparison**: Subprocess-based competitor framework benchmarking
//! - **Regression detection**: Baseline save/compare with configurable tolerance
//!
//! ## Feature Flags
//!
//! - `tau2` — Enables the τ²-bench task quality adapter
//! - `bfcl` — Enables the BFCL (Berkeley Function Calling Leaderboard) adapter
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_bench::{BenchConfig, BenchRunner};
//!
//! let config = BenchConfig::default();
//! let runner = BenchRunner::new(config);
//! let results = runner.run().await?;
//! ```

pub mod config;
pub mod error;
pub mod external;
pub mod formatter;
pub mod instrumented_llm;
pub mod memory;
pub mod metrics;
pub mod runner;
pub mod workload;

/// Task quality adapters for established benchmark suites.
pub mod adapters;
pub use adapters::{CaseResult, TaskQualityAdapter, TaskQualityResult};

// Public re-exports
pub use config::{BenchConfig, ExternalFrameworkConfig, OutputFormat, TaskSuite};
pub use error::{BenchError, Result};
pub use external::{
    ExternalConfigFile, ExternalDurationStats, ExternalMetricsOutput, ExternalRunner,
    ExternalTokenOverhead, load_external_configs,
};
pub use formatter::{ComparisonResult, format_comparison, format_result};
pub use instrumented_llm::{DeterministicConfig, InstrumentedLlm, LlmCallRecord};
pub use metrics::{
    BenchmarkResult, ConcurrencyLevel, DurationStats, MemoryMetrics, MetricCollector, RunMetadata,
    ThroughputMetrics, TokenBreakdown, TokenOverheadMetrics, ToolInvocationMetrics, compute_stats,
};
pub use runner::{BenchRunner, RegressionReport};
pub use workload::{
    AgentConfig, ToolDefinition, Workload, builtin_workloads, load_workload,
    multi_agent_delegation_workload,
};
