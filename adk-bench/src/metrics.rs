//! Metric collection and statistical computation.
//!
//! Provides [`DurationStats`], [`BenchmarkResult`], and the [`MetricCollector`]
//! for accumulating timing samples during benchmark runs.
//!
//! # Statistical Computation
//!
//! The [`compute_stats`] function computes a full statistical summary from
//! a slice of [`Duration`] values, including percentiles using the nearest-rank
//! method.
//!
//! # Example
//!
//! ```rust
//! use std::time::Duration;
//! use adk_bench::metrics::compute_stats;
//!
//! let durations = vec![
//!     Duration::from_micros(100),
//!     Duration::from_micros(200),
//!     Duration::from_micros(300),
//! ];
//! let stats = compute_stats(&durations);
//! assert_eq!(stats.count, 3);
//! assert_eq!(stats.min_us, 100);
//! assert_eq!(stats.max_us, 300);
//! ```

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Statistical summary for a collection of duration measurements.
///
/// All timing values are reported in microseconds (μs).
/// Percentiles use the nearest-rank method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DurationStats {
    /// Minimum duration in microseconds.
    pub min_us: u64,
    /// Maximum duration in microseconds.
    pub max_us: u64,
    /// Arithmetic mean in microseconds.
    pub mean_us: u64,
    /// Median (50th percentile) in microseconds.
    pub median_us: u64,
    /// 95th percentile in microseconds (nearest-rank method).
    pub p95_us: u64,
    /// 99th percentile in microseconds (nearest-rank method).
    pub p99_us: u64,
    /// Population standard deviation in microseconds.
    pub std_dev_us: u64,
    /// Number of samples.
    pub count: usize,
    /// Coefficient of variation (std_dev / mean). 0.0 if mean is 0.
    pub coefficient_of_variation: f64,
}

/// Metrics for a single benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkResult {
    /// Schema version for forward compatibility.
    /// Defaults to 1 when deserializing older results that lack this field.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Workload that was executed.
    pub workload_name: String,
    /// Model used.
    pub model: String,
    /// Run metadata.
    pub metadata: RunMetadata,
    /// Cold start time (process start → first LLM call).
    pub cold_start: DurationStats,
    /// Per-turn agent loop overhead (total_turn - llm_round_trip).
    pub agent_loop_overhead: DurationStats,
    /// Tool invocation latency breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_invocation: Option<ToolInvocationMetrics>,
    /// Concurrent throughput (agents/sec at each concurrency level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput: Option<ThroughputMetrics>,
    /// Memory footprint measurements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryMetrics>,
    /// Token overhead analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_overhead: Option<TokenOverheadMetrics>,
    /// Reproducibility rate (percentage of semantically equivalent responses across runs).
    /// Semantic equivalence = same tool calls + same structured output field values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproducibility_rate: Option<f64>,
    /// Number of iterations performed.
    pub iterations: usize,
}

/// Returns the default schema version (1) for backward compatibility.
fn default_schema_version() -> u32 {
    1
}

/// Run metadata for result provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunMetadata {
    /// ISO 8601 timestamp of the run.
    pub timestamp: String,
    /// ADK-Rust crate version.
    pub adk_version: String,
    /// Rust compiler version.
    pub rust_version: String,
    /// Operating system.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
}

/// Tool invocation latency breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocationMetrics {
    /// Total tool invocation latency.
    pub total: DurationStats,
    /// Argument deserialization time.
    pub deserialization: DurationStats,
    /// Schema validation time.
    pub schema_validation: DurationStats,
    /// Execution dispatch time.
    pub execution_dispatch: DurationStats,
}

/// Throughput measurements at various concurrency levels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThroughputMetrics {
    /// Agents completed per second at each concurrency level.
    pub levels: Vec<ConcurrencyLevel>,
}

/// Throughput measurement at a specific concurrency level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConcurrencyLevel {
    /// Number of concurrent agents.
    pub concurrency: usize,
    /// Agents completed per second.
    pub agents_per_second: f64,
    /// Per-agent completion time statistics.
    pub completion_time: DurationStats,
}

/// Memory footprint measurements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryMetrics {
    /// Peak RSS in bytes during the run.
    pub peak_rss_bytes: u64,
    /// Estimated per-agent memory in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_agent_bytes: Option<u64>,
    /// Number of memory samples taken.
    pub sample_count: usize,
}

/// Token overhead analysis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenOverheadMetrics {
    /// Total tokens sent to LLM.
    pub total_tokens: u64,
    /// Tokens from user content only.
    pub user_content_tokens: u64,
    /// Framework overhead tokens.
    pub overhead_tokens: u64,
    /// Overhead as percentage of total.
    pub overhead_percentage: f64,
    /// Breakdown by category.
    pub breakdown: TokenBreakdown,
}

/// Token overhead breakdown by category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    /// Tokens from framework-injected system prompts.
    pub system_prompt_tokens: u64,
    /// Tokens consumed by serialized tool/function definitions.
    pub tool_schema_tokens: u64,
    /// Tokens added as framework wrappers around user messages.
    pub framework_wrapper_tokens: u64,
}

/// Computes a statistical summary from a slice of durations.
///
/// Returns a [`DurationStats`] with min, max, mean, median, P95, P99,
/// standard deviation, count, and coefficient of variation.
///
/// # Edge Cases
///
/// - **Empty slice**: Returns all zeros with `count = 0`.
/// - **Single element**: Min = max = mean = median = P95 = P99, std_dev = 0.
///
/// # Percentile Method
///
/// Uses the nearest-rank method: `rank = ceil(percentile / 100 * count)`,
/// then index into the sorted array at `rank - 1`.
pub fn compute_stats(durations: &[Duration]) -> DurationStats {
    if durations.is_empty() {
        return DurationStats {
            min_us: 0,
            max_us: 0,
            mean_us: 0,
            median_us: 0,
            p95_us: 0,
            p99_us: 0,
            std_dev_us: 0,
            count: 0,
            coefficient_of_variation: 0.0,
        };
    }

    let mut micros: Vec<u64> = durations.iter().map(|d| d.as_micros() as u64).collect();
    micros.sort_unstable();

    let count = micros.len();
    let min_us = micros[0];
    let max_us = micros[count - 1];

    // Mean
    let sum: u64 = micros.iter().sum();
    let mean_us = sum / count as u64;

    // Median using nearest-rank method (same as P50)
    let median_us = percentile_nearest_rank(&micros, 50.0);

    // P95 and P99 using nearest-rank method
    let p95_us = percentile_nearest_rank(&micros, 95.0);
    let p99_us = percentile_nearest_rank(&micros, 99.0);

    // Population standard deviation
    let mean_f64 = sum as f64 / count as f64;
    let variance: f64 = micros
        .iter()
        .map(|&v| {
            let diff = v as f64 - mean_f64;
            diff * diff
        })
        .sum::<f64>()
        / count as f64;
    let std_dev_f64 = variance.sqrt();
    let std_dev_us = std_dev_f64 as u64;

    // Coefficient of variation = std_dev / mean (0.0 if mean is 0)
    let coefficient_of_variation = if mean_f64 == 0.0 { 0.0 } else { std_dev_f64 / mean_f64 };

    DurationStats {
        min_us,
        max_us,
        mean_us,
        median_us,
        p95_us,
        p99_us,
        std_dev_us,
        count,
        coefficient_of_variation,
    }
}

/// Computes the percentile value using the nearest-rank method.
///
/// `sorted` must be a non-empty, sorted slice of values.
/// `percentile` is a value between 0.0 and 100.0.
fn percentile_nearest_rank(sorted: &[u64], percentile: f64) -> u64 {
    let count = sorted.len();
    if count == 1 {
        return sorted[0];
    }
    // Nearest-rank: rank = ceil(percentile / 100 * count)
    let rank = ((percentile / 100.0) * count as f64).ceil() as usize;
    // Clamp to valid index range [1, count]
    let rank = rank.clamp(1, count);
    sorted[rank - 1]
}

/// A record of tool invocation latency broken into phases.
#[derive(Debug, Clone)]
pub struct ToolLatencyRecord {
    /// Total tool invocation duration.
    pub total: Duration,
    /// Time spent deserializing tool arguments.
    pub deserialization: Duration,
    /// Time spent validating arguments against schema.
    pub schema_validation: Duration,
    /// Time spent dispatching the tool execution.
    pub execution_dispatch: Duration,
}

/// Accumulates timing samples during a benchmark run.
///
/// `MetricCollector` is a mutable accumulator that records various timing
/// and memory measurements as a benchmark progresses, then provides the
/// data needed to produce a [`BenchmarkResult`].
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_bench::metrics::MetricCollector;
///
/// let mut collector = MetricCollector::new();
/// collector.mark_run_start();
/// // ... perform work ...
/// collector.mark_first_llm_call();
/// collector.record_turn_overhead(Duration::from_micros(150));
/// collector.record_memory_sample(1024 * 1024);
///
/// if let Some(cold_start) = collector.cold_start_duration() {
///     println!("Cold start: {:?}", cold_start);
/// }
/// ```
pub struct MetricCollector {
    run_start: Option<Instant>,
    first_llm_call: Option<Instant>,
    turn_overheads: Vec<Duration>,
    tool_latencies: Vec<ToolLatencyRecord>,
    memory_samples: Vec<u64>,
}

impl MetricCollector {
    /// Creates a new empty `MetricCollector`.
    pub fn new() -> Self {
        Self {
            run_start: None,
            first_llm_call: None,
            turn_overheads: Vec::new(),
            tool_latencies: Vec::new(),
            memory_samples: Vec::new(),
        }
    }

    /// Marks the start of the benchmark run.
    ///
    /// Records a monotonic timestamp for cold start calculation.
    pub fn mark_run_start(&mut self) {
        self.run_start = Some(Instant::now());
    }

    /// Marks the first LLM API call.
    ///
    /// Only records the timestamp on the first invocation; subsequent
    /// calls are no-ops.
    pub fn mark_first_llm_call(&mut self) {
        if self.first_llm_call.is_none() {
            self.first_llm_call = Some(Instant::now());
        }
    }

    /// Records a per-turn agent loop overhead duration.
    ///
    /// This is the framework processing time for a single turn,
    /// computed as `total_turn_time - llm_round_trip_time`.
    pub fn record_turn_overhead(&mut self, overhead: Duration) {
        self.turn_overheads.push(overhead);
    }

    /// Records a tool invocation latency breakdown.
    pub fn record_tool_latency(&mut self, record: ToolLatencyRecord) {
        self.tool_latencies.push(record);
    }

    /// Records a memory RSS sample in bytes.
    pub fn record_memory_sample(&mut self, rss_bytes: u64) {
        self.memory_samples.push(rss_bytes);
    }

    /// Returns the cold start duration (run start → first LLM call).
    ///
    /// Returns `None` if either `mark_run_start` or `mark_first_llm_call`
    /// has not been called.
    pub fn cold_start_duration(&self) -> Option<Duration> {
        match (self.run_start, self.first_llm_call) {
            (Some(start), Some(first)) => Some(first.duration_since(start)),
            _ => None,
        }
    }

    /// Returns the recorded turn overhead durations.
    pub fn turn_overheads(&self) -> &[Duration] {
        &self.turn_overheads
    }

    /// Returns the recorded tool latency records.
    pub fn tool_latencies(&self) -> &[ToolLatencyRecord] {
        &self.tool_latencies
    }

    /// Returns the recorded memory samples.
    pub fn memory_samples(&self) -> &[u64] {
        &self.memory_samples
    }
}

impl Default for MetricCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_stats_empty() {
        let stats = compute_stats(&[]);
        assert_eq!(stats.count, 0);
        assert_eq!(stats.min_us, 0);
        assert_eq!(stats.max_us, 0);
        assert_eq!(stats.mean_us, 0);
        assert_eq!(stats.median_us, 0);
        assert_eq!(stats.p95_us, 0);
        assert_eq!(stats.p99_us, 0);
        assert_eq!(stats.std_dev_us, 0);
        assert_eq!(stats.coefficient_of_variation, 0.0);
    }

    #[test]
    fn test_compute_stats_single_element() {
        let durations = vec![Duration::from_micros(500)];
        let stats = compute_stats(&durations);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.min_us, 500);
        assert_eq!(stats.max_us, 500);
        assert_eq!(stats.mean_us, 500);
        assert_eq!(stats.median_us, 500);
        assert_eq!(stats.p95_us, 500);
        assert_eq!(stats.p99_us, 500);
        assert_eq!(stats.std_dev_us, 0);
        assert_eq!(stats.coefficient_of_variation, 0.0);
    }

    #[test]
    fn test_compute_stats_multiple_elements() {
        let durations = vec![
            Duration::from_micros(100),
            Duration::from_micros(200),
            Duration::from_micros(300),
            Duration::from_micros(400),
            Duration::from_micros(500),
        ];
        let stats = compute_stats(&durations);
        assert_eq!(stats.count, 5);
        assert_eq!(stats.min_us, 100);
        assert_eq!(stats.max_us, 500);
        assert_eq!(stats.mean_us, 300);
        assert_eq!(stats.median_us, 300);
        // P95 nearest rank: ceil(0.95 * 5) = 5, so index 4 → 500
        assert_eq!(stats.p95_us, 500);
        // P99 nearest rank: ceil(0.99 * 5) = 5, so index 4 → 500
        assert_eq!(stats.p99_us, 500);
    }

    #[test]
    fn test_compute_stats_ordering_invariant() {
        let durations = vec![
            Duration::from_micros(50),
            Duration::from_micros(100),
            Duration::from_micros(150),
            Duration::from_micros(200),
            Duration::from_micros(250),
            Duration::from_micros(300),
            Duration::from_micros(350),
            Duration::from_micros(400),
            Duration::from_micros(450),
            Duration::from_micros(500),
        ];
        let stats = compute_stats(&durations);
        assert!(stats.min_us <= stats.median_us);
        assert!(stats.median_us <= stats.p95_us);
        assert!(stats.p95_us <= stats.p99_us);
        assert!(stats.p99_us <= stats.max_us);
    }

    #[test]
    fn test_compute_stats_unsorted_input() {
        let durations = vec![
            Duration::from_micros(500),
            Duration::from_micros(100),
            Duration::from_micros(300),
            Duration::from_micros(200),
            Duration::from_micros(400),
        ];
        let stats = compute_stats(&durations);
        assert_eq!(stats.min_us, 100);
        assert_eq!(stats.max_us, 500);
        assert_eq!(stats.mean_us, 300);
    }

    #[test]
    fn test_metric_collector_cold_start() {
        let mut collector = MetricCollector::new();
        assert!(collector.cold_start_duration().is_none());

        collector.mark_run_start();
        assert!(collector.cold_start_duration().is_none());

        // Small sleep to ensure non-zero duration
        std::thread::sleep(Duration::from_millis(1));
        collector.mark_first_llm_call();

        let cold_start = collector.cold_start_duration().unwrap();
        assert!(cold_start >= Duration::from_millis(1));
    }

    #[test]
    fn test_metric_collector_first_llm_call_only_once() {
        let mut collector = MetricCollector::new();
        collector.mark_run_start();
        std::thread::sleep(Duration::from_millis(1));
        collector.mark_first_llm_call();

        let first_duration = collector.cold_start_duration().unwrap();

        // Calling again should not update the timestamp
        std::thread::sleep(Duration::from_millis(10));
        collector.mark_first_llm_call();

        let second_duration = collector.cold_start_duration().unwrap();
        assert_eq!(first_duration, second_duration);
    }

    #[test]
    fn test_metric_collector_turn_overheads() {
        let mut collector = MetricCollector::new();
        collector.record_turn_overhead(Duration::from_micros(100));
        collector.record_turn_overhead(Duration::from_micros(200));
        assert_eq!(collector.turn_overheads().len(), 2);
    }

    #[test]
    fn test_metric_collector_memory_samples() {
        let mut collector = MetricCollector::new();
        collector.record_memory_sample(1024);
        collector.record_memory_sample(2048);
        collector.record_memory_sample(4096);
        assert_eq!(collector.memory_samples(), &[1024, 2048, 4096]);
    }

    #[test]
    fn test_metric_collector_tool_latencies() {
        let mut collector = MetricCollector::new();
        collector.record_tool_latency(ToolLatencyRecord {
            total: Duration::from_micros(500),
            deserialization: Duration::from_micros(100),
            schema_validation: Duration::from_micros(150),
            execution_dispatch: Duration::from_micros(250),
        });
        assert_eq!(collector.tool_latencies().len(), 1);
    }

    #[test]
    fn test_duration_stats_serialization_round_trip() {
        let stats = DurationStats {
            min_us: 100,
            max_us: 500,
            mean_us: 300,
            median_us: 300,
            p95_us: 480,
            p99_us: 499,
            std_dev_us: 141,
            count: 5,
            coefficient_of_variation: 0.47,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: DurationStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, deserialized);
    }

    #[test]
    fn test_coefficient_of_variation_zero_mean() {
        let durations = vec![Duration::from_micros(0), Duration::from_micros(0)];
        let stats = compute_stats(&durations);
        assert_eq!(stats.coefficient_of_variation, 0.0);
    }

    /// Helper to create a sample BenchmarkResult for testing.
    fn sample_benchmark_result() -> BenchmarkResult {
        BenchmarkResult {
            schema_version: 1,
            workload_name: "simple_tool_call".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-15T10:30:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: DurationStats {
                min_us: 1000,
                max_us: 5000,
                mean_us: 2500,
                median_us: 2400,
                p95_us: 4800,
                p99_us: 4950,
                std_dev_us: 800,
                count: 5,
                coefficient_of_variation: 0.32,
            },
            agent_loop_overhead: DurationStats {
                min_us: 100,
                max_us: 500,
                mean_us: 250,
                median_us: 240,
                p95_us: 480,
                p99_us: 495,
                std_dev_us: 80,
                count: 10,
                coefficient_of_variation: 0.32,
            },
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: Some(TokenOverheadMetrics {
                total_tokens: 1200,
                user_content_tokens: 950,
                overhead_tokens: 250,
                overhead_percentage: 20.83,
                breakdown: TokenBreakdown {
                    system_prompt_tokens: 100,
                    tool_schema_tokens: 100,
                    framework_wrapper_tokens: 50,
                },
            }),
            reproducibility_rate: Some(0.95),
            iterations: 5,
        }
    }

    #[test]
    fn test_benchmark_result_serialization_round_trip() {
        let result = sample_benchmark_result();
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, deserialized);
    }

    #[test]
    fn test_benchmark_result_schema_version_always_present() {
        let result = sample_benchmark_result();
        let json = serde_json::to_string(&result).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schemaVersion"], serde_json::json!(1));
    }

    #[test]
    fn test_benchmark_result_deserialize_missing_schema_version() {
        // Simulate an older schema where schema_version is missing
        let json = r#"{
            "workloadName": "simple_tool_call",
            "model": "gemini-2.5-flash",
            "metadata": {
                "timestamp": "2025-01-15T10:30:00Z",
                "adkVersion": "0.4.0",
                "rustVersion": "1.85.0",
                "os": "linux",
                "arch": "x86_64"
            },
            "coldStart": {
                "minUs": 1000, "maxUs": 5000, "meanUs": 2500,
                "medianUs": 2400, "p95Us": 4800, "p99Us": 4950,
                "stdDevUs": 800, "count": 5, "coefficientOfVariation": 0.32
            },
            "agentLoopOverhead": {
                "minUs": 100, "maxUs": 500, "meanUs": 250,
                "medianUs": 240, "p95Us": 480, "p99Us": 495,
                "stdDevUs": 80, "count": 10, "coefficientOfVariation": 0.32
            },
            "iterations": 5
        }"#;

        let result: BenchmarkResult = serde_json::from_str(json).unwrap();
        // schema_version defaults to 1 when missing
        assert_eq!(result.schema_version, 1);
    }

    #[test]
    fn test_benchmark_result_deserialize_missing_optional_fields() {
        // Simulate older schema without token_overhead, reproducibility_rate, etc.
        let json = r#"{
            "schemaVersion": 1,
            "workloadName": "simple_tool_call",
            "model": "gemini-2.5-flash",
            "metadata": {
                "timestamp": "2025-01-15T10:30:00Z",
                "adkVersion": "0.4.0",
                "rustVersion": "1.85.0",
                "os": "linux",
                "arch": "x86_64"
            },
            "coldStart": {
                "minUs": 1000, "maxUs": 5000, "meanUs": 2500,
                "medianUs": 2400, "p95Us": 4800, "p99Us": 4950,
                "stdDevUs": 800, "count": 5, "coefficientOfVariation": 0.32
            },
            "agentLoopOverhead": {
                "minUs": 100, "maxUs": 500, "meanUs": 250,
                "medianUs": 240, "p95Us": 480, "p99Us": 495,
                "stdDevUs": 80, "count": 10, "coefficientOfVariation": 0.32
            },
            "iterations": 5
        }"#;

        let result: BenchmarkResult = serde_json::from_str(json).unwrap();
        // All optional fields default to None
        assert_eq!(result.token_overhead, None);
        assert_eq!(result.reproducibility_rate, None);
        assert_eq!(result.memory, None);
        assert_eq!(result.throughput, None);
        assert_eq!(result.tool_invocation, None);
    }

    #[test]
    fn test_benchmark_result_with_all_optional_fields() {
        let mut result = sample_benchmark_result();
        result.memory = Some(MemoryMetrics {
            peak_rss_bytes: 52_428_800,
            per_agent_bytes: Some(2_097_152),
            sample_count: 50,
        });
        result.throughput = Some(ThroughputMetrics {
            levels: vec![ConcurrencyLevel {
                concurrency: 4,
                agents_per_second: 12.5,
                completion_time: DurationStats {
                    min_us: 800_000,
                    max_us: 1_200_000,
                    mean_us: 1_000_000,
                    median_us: 980_000,
                    p95_us: 1_150_000,
                    p99_us: 1_190_000,
                    std_dev_us: 100_000,
                    count: 4,
                    coefficient_of_variation: 0.1,
                },
            }],
        });

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, deserialized);
    }
}
