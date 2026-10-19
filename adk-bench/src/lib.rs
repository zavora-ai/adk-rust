//! # adk-bench
//!
//! Benchmarking framework for ADK-Rust that measures framework-level runtime overhead using
//! real LLM APIs.
//!
//! [`BenchRunner`] wraps the model in [`InstrumentedLlm`], which sends a deterministic
//! configuration (temperature 0, `top_p` 1.0, seed 42) and records each call's timing, so the
//! framework's per-turn overhead is the total turn time minus the LLM round trip.
//! [`ExternalRunner`] benchmarks other frameworks as subprocesses that speak the External
//! Benchmark Protocol, a single JSON object on stdout.
//!
//! ## Measurements
//!
//! - **Cold start**: run start to the first LLM request (process spawn for external harnesses)
//! - **Agent loop overhead**: per-turn framework time, excluding the LLM round trip
//! - **Concurrent throughput**: agents per second at each concurrency level
//! - **Memory**: peak RSS during the measurement phase and its growth per agent (Linux, macOS)
//! - **External comparison**: peak RSS and token overhead as reported by each harness
//! - **Regression detection**: baselines compared with a configurable tolerance
//!
//! `cargo adk bench` is the command-line front end.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use adk_bench::{BenchConfig, BenchRunner};
//!
//! # #[tokio::main]
//! # async fn main() -> adk_bench::Result<()> {
//! let config = BenchConfig { runs: 1, warmup: 0, ..BenchConfig::default() };
//! let results = BenchRunner::new(config).run().await?;
//! for result in &results {
//!     println!("{}: {} µs", result.workload_name, result.agent_loop_overhead.mean_us);
//! }
//! # Ok(())
//! # }
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
pub use config::{BenchConfig, ExternalFrameworkConfig, OutputFormat};
pub use error::{BenchError, Result};
pub use external::{
    ExternalConfigFile, ExternalDurationStats, ExternalMetricsOutput, ExternalRunner,
    ExternalTokenOverhead, load_external_configs,
};
pub use formatter::{ComparisonResult, format_comparison, format_result};
pub use instrumented_llm::{DeterministicConfig, InstrumentedLlm, LlmCallRecord};
pub use metrics::{
    BenchmarkResult, ConcurrencyLevel, DurationStats, MemoryMetrics, MetricCollector, RunMetadata,
    ThroughputMetrics, compute_stats,
};
pub use runner::{BenchRunner, RegressionReport};
pub use workload::{
    AgentConfig, ToolDefinition, Workload, builtin_workloads, load_workload,
    multi_agent_delegation_workload,
};
