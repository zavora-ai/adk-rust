//! Telemetry instrumentation for Ralph multi-agent system.
//!
//! This module provides:
//! - Span helpers for tracing agent and tool execution
//! - Metrics for tracking iterations, tasks, and performance
//!
//! ## Spans
//!
//! The following spans are created:
//! - `ralph.prd_generation` - PRD agent execution
//! - `ralph.architect_design` - Architect agent execution
//! - `ralph.loop_iteration` - Each Ralph loop iteration
//! - `ralph.task_execution` - Individual task execution
//! - `ralph.tool_call` - Each tool invocation
//!
//! ## Metrics
//!
//! The following metrics are collected:
//! - `ralph_iterations_total` - Total loop iterations (counter)
//! - `ralph_tasks_completed` - Tasks completed (counter)
//! - `ralph_tasks_failed` - Tasks failed (counter)
//! - `ralph_llm_latency_seconds` - LLM response latency (histogram)
//! - `ralph_tool_duration_seconds` - Tool execution duration (histogram)
//! - `ralph_tokens_used` - Tokens used per request (gauge)

use opentelemetry::metrics::{Counter, Histogram, Meter, UpDownCounter};
use opentelemetry::KeyValue;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;
use tracing::{info_span, Span};

// Re-export tracing macros for convenience
pub use tracing::{debug, error, info, instrument, trace, warn};

/// Global metrics instance
static METRICS: OnceLock<RalphMetrics> = OnceLock::new();

/// Global OpenTelemetry metrics instance
static OTEL_METRICS: OnceLock<RalphOtelMetrics> = OnceLock::new();

/// Get or initialize the global metrics instance.
pub fn metrics() -> &'static RalphMetrics {
    METRICS.get_or_init(RalphMetrics::new)
}

/// Get or initialize the global OpenTelemetry metrics instance.
pub fn otel_metrics() -> &'static RalphOtelMetrics {
    OTEL_METRICS.get_or_init(|| {
        let meter = opentelemetry::global::meter("ralph");
        RalphOtelMetrics::new(meter)
    })
}

/// OpenTelemetry metrics for Ralph execution.
pub struct RalphOtelMetrics {
    /// Counter for total iterations
    pub iterations_counter: Counter<u64>,
    /// Counter for tasks completed
    pub tasks_completed_counter: Counter<u64>,
    /// Counter for tasks failed
    pub tasks_failed_counter: Counter<u64>,
    /// Histogram for LLM latency in seconds
    pub llm_latency_histogram: Histogram<f64>,
    /// Histogram for tool duration in seconds
    pub tool_duration_histogram: Histogram<f64>,
    /// UpDownCounter for tokens used (can go up and down per request)
    pub tokens_gauge: UpDownCounter<i64>,
}

impl RalphOtelMetrics {
    /// Create a new OpenTelemetry metrics instance.
    pub fn new(meter: Meter) -> Self {
        let iterations_counter = meter
            .u64_counter("ralph_iterations_total")
            .with_description("Total number of Ralph loop iterations")
            .init();

        let tasks_completed_counter = meter
            .u64_counter("ralph_tasks_completed")
            .with_description("Total number of tasks completed successfully")
            .init();

        let tasks_failed_counter = meter
            .u64_counter("ralph_tasks_failed")
            .with_description("Total number of tasks that failed")
            .init();

        let llm_latency_histogram = meter
            .f64_histogram("ralph_llm_latency_seconds")
            .with_description("LLM response latency in seconds")
            .init();

        let tool_duration_histogram = meter
            .f64_histogram("ralph_tool_duration_seconds")
            .with_description("Tool execution duration in seconds")
            .init();

        let tokens_gauge = meter
            .i64_up_down_counter("ralph_tokens_used")
            .with_description("Tokens used in LLM requests")
            .init();

        Self {
            iterations_counter,
            tasks_completed_counter,
            tasks_failed_counter,
            llm_latency_histogram,
            tool_duration_histogram,
            tokens_gauge,
        }
    }

    /// Record an iteration.
    pub fn record_iteration(&self) {
        self.iterations_counter.add(1, &[]);
    }

    /// Record a completed task.
    pub fn record_task_completed(&self, task_id: &str) {
        self.tasks_completed_counter
            .add(1, &[KeyValue::new("task_id", task_id.to_string())]);
    }

    /// Record a failed task.
    pub fn record_task_failed(&self, task_id: &str) {
        self.tasks_failed_counter
            .add(1, &[KeyValue::new("task_id", task_id.to_string())]);
    }

    /// Record LLM latency.
    pub fn record_llm_latency(&self, duration_secs: f64, model: &str, provider: &str) {
        self.llm_latency_histogram.record(
            duration_secs,
            &[
                KeyValue::new("model", model.to_string()),
                KeyValue::new("provider", provider.to_string()),
            ],
        );
    }

    /// Record tool duration.
    pub fn record_tool_duration(&self, duration_secs: f64, tool_name: &str, operation: &str) {
        self.tool_duration_histogram.record(
            duration_secs,
            &[
                KeyValue::new("tool", tool_name.to_string()),
                KeyValue::new("operation", operation.to_string()),
            ],
        );
    }

    /// Record tokens used.
    pub fn record_tokens(&self, tokens: i64, model: &str) {
        self.tokens_gauge
            .add(tokens, &[KeyValue::new("model", model.to_string())]);
    }
}

/// Metrics for Ralph execution (simple atomic counters for local tracking).
#[derive(Debug)]
pub struct RalphMetrics {
    /// Total iterations counter
    pub iterations_total: AtomicU64,
    /// Tasks completed counter
    pub tasks_completed: AtomicU64,
    /// Tasks failed counter
    pub tasks_failed: AtomicU64,
    /// Total tokens used
    pub tokens_used: AtomicU64,
}

impl RalphMetrics {
    /// Create a new metrics instance.
    pub fn new() -> Self {
        Self {
            iterations_total: AtomicU64::new(0),
            tasks_completed: AtomicU64::new(0),
            tasks_failed: AtomicU64::new(0),
            tokens_used: AtomicU64::new(0),
        }
    }

    /// Increment the iterations counter.
    pub fn inc_iterations(&self) {
        self.iterations_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the tasks completed counter.
    pub fn inc_tasks_completed(&self) {
        self.tasks_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the tasks failed counter.
    pub fn inc_tasks_failed(&self) {
        self.tasks_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Add tokens used.
    pub fn add_tokens(&self, tokens: u64) {
        self.tokens_used.fetch_add(tokens, Ordering::Relaxed);
    }

    /// Get current iteration count.
    pub fn get_iterations(&self) -> u64 {
        self.iterations_total.load(Ordering::Relaxed)
    }

    /// Get current tasks completed count.
    pub fn get_tasks_completed(&self) -> u64 {
        self.tasks_completed.load(Ordering::Relaxed)
    }

    /// Get current tasks failed count.
    pub fn get_tasks_failed(&self) -> u64 {
        self.tasks_failed.load(Ordering::Relaxed)
    }

    /// Get total tokens used.
    pub fn get_tokens(&self) -> u64 {
        self.tokens_used.load(Ordering::Relaxed)
    }

    /// Reset all metrics (useful for testing).
    pub fn reset(&self) {
        self.iterations_total.store(0, Ordering::Relaxed);
        self.tasks_completed.store(0, Ordering::Relaxed);
        self.tasks_failed.store(0, Ordering::Relaxed);
        self.tokens_used.store(0, Ordering::Relaxed);
    }
}

impl Default for RalphMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Span Helpers
// ============================================================================

/// Create a span for PRD generation.
pub fn prd_generation_span(model: &str) -> Span {
    info_span!(
        "ralph.prd_generation",
        otel.name = "PRD Generation",
        model = %model,
        phase = "requirements"
    )
}

/// Create a span for architect design generation.
pub fn architect_design_span(model: &str, language: &str) -> Span {
    info_span!(
        "ralph.architect_design",
        otel.name = "Architect Design",
        model = %model,
        language = %language,
        phase = "design"
    )
}

/// Create a span for a Ralph loop iteration.
pub fn loop_iteration_span(iteration: u32, max_iterations: usize) -> Span {
    info_span!(
        "ralph.loop_iteration",
        otel.name = "Loop Iteration",
        iteration = %iteration,
        max_iterations = %max_iterations,
        phase = "implementation"
    )
}

/// Create a span for task execution.
pub fn task_execution_span(task_id: &str, task_title: &str, priority: u8) -> Span {
    info_span!(
        "ralph.task_execution",
        otel.name = "Task Execution",
        task_id = %task_id,
        task_title = %task_title,
        priority = %priority
    )
}

/// Create a span for tool invocation.
pub fn tool_call_span(tool_name: &str, operation: &str) -> Span {
    info_span!(
        "ralph.tool_call",
        otel.name = "Tool Call",
        tool = %tool_name,
        operation = %operation
    )
}

/// Create a span for test execution.
pub fn test_execution_span(language: &str) -> Span {
    info_span!(
        "ralph.test_execution",
        otel.name = "Test Execution",
        language = %language
    )
}

/// Create a span for git operations.
pub fn git_operation_span(operation: &str) -> Span {
    info_span!(
        "ralph.git_operation",
        otel.name = "Git Operation",
        operation = %operation
    )
}

/// Create a span for LLM requests.
pub fn llm_request_span(model: &str, provider: &str) -> Span {
    info_span!(
        "ralph.llm_request",
        otel.name = "LLM Request",
        model = %model,
        provider = %provider
    )
}

// ============================================================================
// Timing Helpers
// ============================================================================

/// A guard that records duration when dropped.
pub struct TimingGuard {
    start: Instant,
    name: String,
}

impl TimingGuard {
    /// Create a new timing guard.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            start: Instant::now(),
            name: name.into(),
        }
    }

    /// Get elapsed time in milliseconds.
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        tracing::debug!(
            target: "ralph.timing",
            name = %self.name,
            duration_ms = %elapsed.as_millis(),
            "Operation completed"
        );
    }
}

/// Start timing an operation.
pub fn start_timing(name: impl Into<String>) -> TimingGuard {
    TimingGuard::new(name)
}

// ============================================================================
// Event Logging Helpers
// ============================================================================

/// Log task start event.
pub fn log_task_start(task_id: &str, task_title: &str) {
    tracing::info!(
        target: "ralph.events",
        event = "task_start",
        task_id = %task_id,
        task_title = %task_title,
        "Starting task"
    );
}

/// Log task completion event.
pub fn log_task_complete(task_id: &str, success: bool, duration_ms: u64) {
    if success {
        metrics().inc_tasks_completed();
        otel_metrics().record_task_completed(task_id);
        tracing::info!(
            target: "ralph.events",
            event = "task_complete",
            task_id = %task_id,
            success = %success,
            duration_ms = %duration_ms,
            "Task completed successfully"
        );
    } else {
        metrics().inc_tasks_failed();
        otel_metrics().record_task_failed(task_id);
        tracing::warn!(
            target: "ralph.events",
            event = "task_failed",
            task_id = %task_id,
            success = %success,
            duration_ms = %duration_ms,
            "Task failed"
        );
    }
}

/// Log test results event.
pub fn log_test_results(passed: usize, failed: usize, skipped: usize) {
    tracing::info!(
        target: "ralph.events",
        event = "test_results",
        passed = %passed,
        failed = %failed,
        skipped = %skipped,
        success = %(failed == 0),
        "Test execution completed"
    );
}

/// Log git commit event.
pub fn log_git_commit(task_id: &str, commit_hash: &str) {
    tracing::info!(
        target: "ralph.events",
        event = "git_commit",
        task_id = %task_id,
        commit_hash = %commit_hash,
        "Code committed"
    );
}

/// Log iteration start event.
pub fn log_iteration_start(iteration: u32) {
    metrics().inc_iterations();
    otel_metrics().record_iteration();
    tracing::info!(
        target: "ralph.events",
        event = "iteration_start",
        iteration = %iteration,
        "Starting iteration"
    );
}

/// Log completion event.
pub fn log_completion(tasks_completed: usize, iterations: u32, message: &str) {
    tracing::info!(
        target: "ralph.events",
        event = "completion",
        tasks_completed = %tasks_completed,
        iterations = %iterations,
        message = %message,
        "Ralph execution completed"
    );
}

/// Log error event.
pub fn log_error(context: &str, error: &str) {
    tracing::error!(
        target: "ralph.events",
        event = "error",
        context = %context,
        error = %error,
        "Error occurred"
    );
}

/// Record LLM latency metric.
pub fn record_llm_latency(duration_secs: f64, model: &str, provider: &str) {
    otel_metrics().record_llm_latency(duration_secs, model, provider);
    tracing::debug!(
        target: "ralph.metrics",
        metric = "llm_latency",
        duration_secs = %duration_secs,
        model = %model,
        provider = %provider,
        "LLM latency recorded"
    );
}

/// Record tool duration metric.
pub fn record_tool_duration(duration_secs: f64, tool_name: &str, operation: &str) {
    otel_metrics().record_tool_duration(duration_secs, tool_name, operation);
    tracing::debug!(
        target: "ralph.metrics",
        metric = "tool_duration",
        duration_secs = %duration_secs,
        tool = %tool_name,
        operation = %operation,
        "Tool duration recorded"
    );
}

/// Record tokens used metric.
pub fn record_tokens_used(tokens: i64, model: &str) {
    if tokens > 0 {
        metrics().add_tokens(tokens as u64);
    }
    otel_metrics().record_tokens(tokens, model);
    tracing::debug!(
        target: "ralph.metrics",
        metric = "tokens_used",
        tokens = %tokens,
        model = %model,
        "Tokens recorded"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_increment() {
        let metrics = RalphMetrics::new();
        
        assert_eq!(metrics.get_iterations(), 0);
        metrics.inc_iterations();
        assert_eq!(metrics.get_iterations(), 1);
        
        assert_eq!(metrics.get_tasks_completed(), 0);
        metrics.inc_tasks_completed();
        assert_eq!(metrics.get_tasks_completed(), 1);
        
        assert_eq!(metrics.get_tasks_failed(), 0);
        metrics.inc_tasks_failed();
        assert_eq!(metrics.get_tasks_failed(), 1);
    }

    #[test]
    fn test_metrics_reset() {
        let metrics = RalphMetrics::new();
        
        metrics.inc_iterations();
        metrics.inc_tasks_completed();
        metrics.inc_tasks_failed();
        metrics.add_tokens(100);
        
        metrics.reset();
        
        assert_eq!(metrics.get_iterations(), 0);
        assert_eq!(metrics.get_tasks_completed(), 0);
        assert_eq!(metrics.get_tasks_failed(), 0);
        assert_eq!(metrics.get_tokens(), 0);
    }

    #[test]
    fn test_timing_guard() {
        let guard = start_timing("test_operation");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(guard.elapsed_ms() >= 10);
    }

    #[test]
    fn test_span_creation() {
        // Just verify spans can be created without panicking
        let _span1 = prd_generation_span("test-model");
        let _span2 = architect_design_span("test-model", "rust");
        let _span3 = loop_iteration_span(1, 50);
        let _span4 = task_execution_span("TASK-001", "Test Task", 1);
        let _span5 = tool_call_span("progress", "read");
    }
}
