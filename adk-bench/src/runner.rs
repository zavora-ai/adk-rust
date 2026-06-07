//! Benchmark runner orchestrator.
//!
//! Coordinates workload execution, warm-up, iteration, concurrency,
//! metric aggregation, and regression detection.
//!
//! The [`BenchRunner`] is the top-level orchestrator for benchmark execution.
//! It loads workloads, performs warm-up iterations, runs measurement iterations,
//! handles concurrent agent execution, supports concurrency sweep mode, and
//! integrates with [`BaselineStore`] for regression detection and [`CostTracker`]
//! for cost estimation.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::{BenchConfig, BenchRunner};
//!
//! let config = BenchConfig::default();
//! let runner = BenchRunner::new(config);
//! let results = runner.run().await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, Llm,
    identity::{SessionId, UserId},
};
use adk_eval::{BaselineStore, CostTracker};
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use adk_session::SessionService;
use adk_tool::FunctionTool;
use futures::StreamExt;
use tokio::task::JoinSet;

use crate::config::BenchConfig;
use crate::error::{BenchError, Result};
use crate::instrumented_llm::InstrumentedLlm;
use crate::metrics::{
    BenchmarkResult, ConcurrencyLevel, DurationStats, RunMetadata, ThroughputMetrics, compute_stats,
};
use crate::workload::{
    Workload, builtin_workloads, load_workload, multi_agent_delegation_workload,
};

/// Default concurrency sweep levels when `--sweep` is active.
const SWEEP_LEVELS: &[usize] = &[1, 2, 4, 8, 16, 32, 64];

/// CV threshold (20%) above which a warning is emitted for Agent_Loop_Overhead.
const CV_WARNING_THRESHOLD: f64 = 0.20;

/// A regression detected during baseline comparison.
///
/// Reports which metric and workload regressed, the baseline and current values,
/// and the degradation percentage.
#[derive(Debug, Clone)]
pub struct RegressionReport {
    /// The metric that regressed (e.g., "agent_loop_overhead_mean_us").
    pub metric_name: String,
    /// The workload where the regression was detected.
    pub workload_name: String,
    /// The baseline value for the metric.
    pub baseline_value: f64,
    /// The current measured value for the metric.
    pub current_value: f64,
    /// Degradation as a fraction (e.g., 0.15 means 15% worse).
    pub degradation: f64,
}

/// Top-level orchestrator for benchmark execution.
///
/// Manages the full lifecycle of a benchmark run:
/// 1. Loading workloads (built-in or from file)
/// 2. Cost estimation and budget enforcement
/// 3. Warm-up phase (iterations discarded)
/// 4. Measurement phase (iterations recorded)
/// 5. Concurrent execution and sweep modes
/// 6. Metric aggregation and CV warnings
/// 7. Baseline save and regression detection
pub struct BenchRunner {
    config: BenchConfig,
    baseline_store: BaselineStore,
    cost_tracker: CostTracker,
}

impl BenchRunner {
    /// Creates a new `BenchRunner` with the given configuration.
    ///
    /// Initializes [`BaselineStore`] at the configured baseline path
    /// and a default [`CostTracker`] with standard model pricing.
    pub fn new(config: BenchConfig) -> Self {
        let baseline_store = BaselineStore::new(&config.baseline_path);
        let cost_tracker = CostTracker::new();
        Self { config, baseline_store, cost_tracker }
    }

    /// Runs the full benchmark suite and returns results.
    ///
    /// # Execution Flow
    ///
    /// 1. Resolves workloads (specific workload via `--workload` or all built-in)
    /// 2. Estimates cost and enforces budget (dry-run, max-cost-usd, confirm-cost)
    /// 3. For each workload:
    ///    a. Warm-up phase: run `config.warmup` iterations, discard results
    ///    b. Measurement phase: run `config.runs` iterations, collect metrics
    ///    c. If sweep mode: iterate through concurrency levels
    ///    d. If concurrency > 1: spawn concurrent tasks
    /// 4. Aggregates metrics and emits CV warnings
    /// 5. Returns collected [`BenchmarkResult`] values
    ///
    /// # Errors
    ///
    /// - [`BenchError::WorkloadNotFound`] if a specified workload file doesn't exist
    /// - [`BenchError::Baseline`] if cost exceeds `--max-cost-usd`
    pub async fn run(&self) -> Result<Vec<BenchmarkResult>> {
        let workloads = self.resolve_workloads()?;

        // Cost estimation phase
        let estimated_cost = self.estimate_cost(&workloads);
        if self.config.dry_run {
            tracing::info!(
                estimated_cost_usd = estimated_cost,
                total_workloads = workloads.len(),
                runs = self.config.runs,
                concurrency = self.config.concurrency,
                "dry-run: displaying estimated cost without executing"
            );
            return Ok(Vec::new());
        }

        // Max cost guard
        if let Some(max_cost) = self.config.max_cost_usd
            && estimated_cost > max_cost
        {
            return Err(BenchError::Baseline(format!(
                "estimated cost ${estimated_cost:.4} exceeds --max-cost-usd limit ${max_cost:.4}. \
                 Reduce runs, concurrency, or workloads to stay within budget."
            )));
        }

        // Cost confirmation gate (when cost > $1.00 and --confirm-cost not set)
        if estimated_cost > 1.0 && !self.config.confirm_cost {
            tracing::warn!(
                estimated_cost_usd = estimated_cost,
                "estimated cost exceeds $1.00; pass --confirm-cost to proceed"
            );
            return Err(BenchError::Baseline(format!(
                "estimated cost ${estimated_cost:.4} exceeds $1.00. \
                 Pass --confirm-cost to acknowledge, or use --max-cost-usd to set a limit."
            )));
        }

        let mut results = Vec::new();

        for workload in &workloads {
            if let Some(ref sweep_levels) = self.config.concurrency_sweep {
                // Concurrency sweep mode
                let result = self.run_workload_with_sweep(workload, sweep_levels).await?;
                results.push(result);
            } else if self.config.concurrency > 1 {
                // Fixed concurrency mode
                let result =
                    self.run_workload_concurrent(workload, self.config.concurrency).await?;
                results.push(result);
            } else {
                // Sequential mode
                let result = self.run_workload_sequential(workload).await?;
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Saves current results as the regression baseline.
    ///
    /// Persists metrics via [`BaselineStore`] for later regression detection.
    pub fn save_baseline(&self, results: &[BenchmarkResult]) -> Result<()> {
        let metrics = self.results_to_baseline_metrics(results);
        self.baseline_store
            .save("adk-bench", &metrics)
            .map_err(|e| BenchError::Baseline(format!("failed to save baseline: {e}")))?;
        Ok(())
    }

    /// Checks results against saved baseline using configured tolerance.
    ///
    /// For benchmark timing metrics, a regression means the current value is
    /// *higher* than the baseline (worse performance). The formula is:
    ///
    /// ```text
    /// regression detected when (current - baseline) / baseline > tolerance
    /// ```
    ///
    /// Returns a list of [`RegressionReport`] entries for any metrics that
    /// exceed the tolerance threshold. An empty list means no regressions.
    ///
    /// # Exit Code Contract
    ///
    /// The CLI layer (Task 8.1) should exit with code 2 when this method
    /// returns a non-empty list, and exit with code 0 otherwise.
    pub fn check_regression(&self, results: &[BenchmarkResult]) -> Result<Vec<RegressionReport>> {
        let current_metrics = self.results_to_baseline_metrics(results);

        // Load baseline directly for timing-aware comparison.
        // BaselineStore::check_regressions() uses `baseline - current > tolerance`
        // which is designed for "higher is better" metrics (like accuracy).
        // For benchmarks, higher timing values are *worse*, so we need the inverse:
        // detect when `(current - baseline) / baseline > tolerance`.
        let baseline = self
            .baseline_store
            .load()
            .map_err(|e| BenchError::Baseline(format!("regression check failed: {e}")))?;

        let baseline = match baseline {
            Some(b) => b,
            None => {
                tracing::info!("no baseline file found, skipping regression check");
                return Ok(Vec::new());
            }
        };

        let mut reports = Vec::new();

        for (metric_name, baseline_cases) in &baseline.metrics {
            if let Some(current_cases) = current_metrics.get(metric_name) {
                for (case_id, &baseline_value) in baseline_cases {
                    if let Some(&current_value) = current_cases.get(case_id) {
                        // For timing metrics: regression = current is worse (higher) than baseline
                        let degradation = if baseline_value > 0.0 {
                            (current_value - baseline_value) / baseline_value
                        } else {
                            0.0
                        };

                        if degradation > self.config.tolerance {
                            // Parse workload and metric name from the case_id
                            // Format is "workload_name::metric_suffix"
                            let (workload_name, parsed_metric_name) = case_id
                                .split_once("::")
                                .map(|(w, m)| (w.to_string(), m.to_string()))
                                .unwrap_or((metric_name.clone(), case_id.clone()));

                            reports.push(RegressionReport {
                                metric_name: parsed_metric_name,
                                workload_name,
                                baseline_value,
                                current_value,
                                degradation,
                            });
                        }
                    }
                }
            }
        }

        Ok(reports)
    }

    /// Resolves workloads based on configuration.
    fn resolve_workloads(&self) -> Result<Vec<Workload>> {
        if let Some(ref workload_path) = self.config.workload {
            // Check if it's a file path
            let path = std::path::Path::new(workload_path);
            if path.exists() {
                let workload = load_workload(path)?;
                return Ok(vec![workload]);
            }

            // Otherwise, look for it in built-in workloads
            let mut all = builtin_workloads();
            if self.config.experimental {
                all.push(multi_agent_delegation_workload());
            }

            let found = all.into_iter().find(|w| w.name == *workload_path);
            match found {
                Some(w) => Ok(vec![w]),
                None => Err(BenchError::WorkloadNotFound { path: workload_path.clone() }),
            }
        } else {
            let mut workloads = builtin_workloads();
            if self.config.experimental {
                workloads.push(multi_agent_delegation_workload());
            }
            Ok(workloads)
        }
    }

    /// Estimates the total API cost for the benchmark run.
    ///
    /// Uses the CostTracker to compute cost from estimated token counts.
    /// Estimation is based on workload expected_turns × average tokens per turn.
    fn estimate_cost(&self, workloads: &[Workload]) -> f64 {
        let mut total_cost = 0.0;

        // Rough token estimate: ~500 input + ~200 output per turn
        const ESTIMATED_INPUT_TOKENS_PER_TURN: u64 = 500;
        const ESTIMATED_OUTPUT_TOKENS_PER_TURN: u64 = 200;

        let concurrency_multiplier = if let Some(ref levels) = self.config.concurrency_sweep {
            // Sum of all sweep levels
            levels.iter().sum::<usize>()
        } else {
            self.config.concurrency
        };

        for workload in workloads {
            let turns = workload.expected_turns as u64;
            let total_iterations =
                (self.config.runs + self.config.warmup) as u64 * concurrency_multiplier as u64;

            let prompt_tokens = turns * ESTIMATED_INPUT_TOKENS_PER_TURN * total_iterations;
            let completion_tokens = turns * ESTIMATED_OUTPUT_TOKENS_PER_TURN * total_iterations;

            if let Some(cost) =
                self.cost_tracker.compute_cost(&workload.model, prompt_tokens, completion_tokens)
            {
                total_cost += cost;
            }
        }

        total_cost
    }

    /// Runs a single workload sequentially (concurrency=1).
    async fn run_workload_sequential(&self, workload: &Workload) -> Result<BenchmarkResult> {
        // Warm-up phase: run iterations but discard results
        tracing::info!(
            workload = workload.name,
            warmup = self.config.warmup,
            "starting warm-up phase"
        );
        for i in 0..self.config.warmup {
            tracing::debug!(workload = workload.name, iteration = i, "warm-up iteration");
            self.execute_single_workload(workload).await?;
        }

        // Measurement phase
        tracing::info!(
            workload = workload.name,
            runs = self.config.runs,
            "starting measurement phase"
        );
        let mut cold_start_durations = Vec::new();
        let mut overhead_durations = Vec::new();

        for i in 0..self.config.runs {
            tracing::debug!(workload = workload.name, iteration = i, "measurement iteration");
            let (cold_start, overheads) = self.execute_single_workload(workload).await?;
            cold_start_durations.push(cold_start);
            overhead_durations.extend(overheads);
        }

        let cold_start_stats = compute_stats(&cold_start_durations);
        let overhead_stats = compute_stats(&overhead_durations);

        // CV warning for Agent_Loop_Overhead
        self.emit_cv_warning(&overhead_stats, &workload.name);

        Ok(BenchmarkResult {
            schema_version: 1,
            workload_name: workload.name.clone(),
            model: workload.model.clone(),
            metadata: self.build_run_metadata(),
            cold_start: cold_start_stats,
            agent_loop_overhead: overhead_stats,
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: self.config.runs,
        })
    }

    /// Runs a single workload at a fixed concurrency level.
    async fn run_workload_concurrent(
        &self,
        workload: &Workload,
        concurrency: usize,
    ) -> Result<BenchmarkResult> {
        // Warm-up phase
        tracing::info!(
            workload = workload.name,
            warmup = self.config.warmup,
            concurrency,
            "starting concurrent warm-up phase"
        );
        for _ in 0..self.config.warmup {
            self.execute_concurrent_batch(workload, concurrency).await?;
        }

        // Measurement phase
        tracing::info!(
            workload = workload.name,
            runs = self.config.runs,
            concurrency,
            "starting concurrent measurement phase"
        );
        let mut cold_start_durations = Vec::new();
        let mut overhead_durations = Vec::new();
        let mut completion_times = Vec::new();

        for _ in 0..self.config.runs {
            let batch_start = Instant::now();
            let batch_results = self.execute_concurrent_batch(workload, concurrency).await?;
            let batch_elapsed = batch_start.elapsed();

            for (cold_start, overheads) in &batch_results {
                cold_start_durations.push(*cold_start);
                overhead_durations.extend(overheads.iter().copied());
            }
            // Per-agent completion time is the full batch divided by concurrency
            completion_times.push(batch_elapsed);
        }

        let cold_start_stats = compute_stats(&cold_start_durations);
        let overhead_stats = compute_stats(&overhead_durations);
        let completion_stats = compute_stats(&completion_times);

        // CV warning for Agent_Loop_Overhead
        self.emit_cv_warning(&overhead_stats, &workload.name);

        // Compute throughput: agents_per_second = concurrency / mean_completion_time_secs
        let mean_completion_secs = if !completion_times.is_empty() {
            completion_times.iter().map(|d| d.as_secs_f64()).sum::<f64>()
                / completion_times.len() as f64
        } else {
            1.0
        };
        let agents_per_second = concurrency as f64 / mean_completion_secs;

        let throughput = Some(ThroughputMetrics {
            levels: vec![ConcurrencyLevel {
                concurrency,
                agents_per_second,
                completion_time: completion_stats,
            }],
        });

        Ok(BenchmarkResult {
            schema_version: 1,
            workload_name: workload.name.clone(),
            model: workload.model.clone(),
            metadata: self.build_run_metadata(),
            cold_start: cold_start_stats,
            agent_loop_overhead: overhead_stats,
            tool_invocation: None,
            throughput,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: self.config.runs,
        })
    }

    /// Runs a workload in concurrency sweep mode.
    ///
    /// Tests multiple concurrency levels (e.g., 1, 2, 4, 8, 16, 32, 64) and
    /// records throughput at each level.
    async fn run_workload_with_sweep(
        &self,
        workload: &Workload,
        sweep_levels: &[usize],
    ) -> Result<BenchmarkResult> {
        let levels_to_test =
            if sweep_levels.is_empty() { SWEEP_LEVELS.to_vec() } else { sweep_levels.to_vec() };

        tracing::info!(
            workload = workload.name,
            levels = ?levels_to_test,
            "starting concurrency sweep"
        );

        // Warm-up at lowest concurrency level
        let min_level = *levels_to_test.first().unwrap_or(&1);
        for _ in 0..self.config.warmup {
            self.execute_concurrent_batch(workload, min_level).await?;
        }

        let mut all_cold_starts = Vec::new();
        let mut all_overheads = Vec::new();
        let mut throughput_levels = Vec::new();

        for &level in &levels_to_test {
            tracing::info!(
                workload = workload.name,
                concurrency = level,
                "sweeping concurrency level"
            );

            let mut level_completion_times = Vec::new();

            for _ in 0..self.config.runs {
                let batch_start = Instant::now();
                let batch_results = self.execute_concurrent_batch(workload, level).await?;
                let batch_elapsed = batch_start.elapsed();

                for (cold_start, overheads) in &batch_results {
                    all_cold_starts.push(*cold_start);
                    all_overheads.extend(overheads.iter().copied());
                }
                level_completion_times.push(batch_elapsed);
            }

            let completion_stats = compute_stats(&level_completion_times);
            let mean_secs = if !level_completion_times.is_empty() {
                level_completion_times.iter().map(|d| d.as_secs_f64()).sum::<f64>()
                    / level_completion_times.len() as f64
            } else {
                1.0
            };
            let agents_per_second = level as f64 / mean_secs;

            throughput_levels.push(ConcurrencyLevel {
                concurrency: level,
                agents_per_second,
                completion_time: completion_stats,
            });
        }

        let cold_start_stats = compute_stats(&all_cold_starts);
        let overhead_stats = compute_stats(&all_overheads);

        // CV warning for Agent_Loop_Overhead
        self.emit_cv_warning(&overhead_stats, &workload.name);

        Ok(BenchmarkResult {
            schema_version: 1,
            workload_name: workload.name.clone(),
            model: workload.model.clone(),
            metadata: self.build_run_metadata(),
            cold_start: cold_start_stats,
            agent_loop_overhead: overhead_stats,
            tool_invocation: None,
            throughput: Some(ThroughputMetrics { levels: throughput_levels }),
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: self.config.runs,
        })
    }

    /// Executes a batch of concurrent workload instances.
    ///
    /// Spawns `concurrency` tasks, each executing the workload independently.
    /// Returns timing results for each task.
    async fn execute_concurrent_batch(
        &self,
        workload: &Workload,
        concurrency: usize,
    ) -> Result<Vec<(Duration, Vec<Duration>)>> {
        let mut join_set = JoinSet::new();

        for _ in 0..concurrency {
            let workload = workload.clone();
            let model_name = self.config.model.clone();
            join_set.spawn(async move { execute_workload_real(&workload, &model_name).await });
        }

        let mut results = Vec::with_capacity(concurrency);
        while let Some(join_result) = join_set.join_next().await {
            let task_result =
                join_result.map_err(|e| BenchError::Llm(format!("task join failed: {e}")))?;
            results.push(task_result?);
        }

        Ok(results)
    }

    /// Executes a single workload iteration using a real LLM.
    ///
    /// Returns (cold_start_duration, vec_of_per_turn_overheads).
    async fn execute_single_workload(
        &self,
        workload: &Workload,
    ) -> Result<(Duration, Vec<Duration>)> {
        execute_workload_real(workload, &self.config.model).await
    }

    /// Emits a warning if the coefficient of variation exceeds the threshold.
    fn emit_cv_warning(&self, stats: &DurationStats, workload_name: &str) {
        if stats.count > 1 && stats.coefficient_of_variation > CV_WARNING_THRESHOLD {
            tracing::warn!(
                workload = workload_name,
                cv = format!("{:.1}%", stats.coefficient_of_variation * 100.0),
                threshold = "20%",
                mean_us = stats.mean_us,
                std_dev_us = stats.std_dev_us,
                "Agent_Loop_Overhead CV exceeds 20%, measurements may be unstable. \
                 Consider increasing iteration count or reducing system load."
            );
        }
    }

    /// Builds run metadata for result provenance.
    fn build_run_metadata(&self) -> RunMetadata {
        RunMetadata {
            timestamp: chrono::Utc::now().to_rfc3339(),
            adk_version: env!("CARGO_PKG_VERSION").to_string(),
            rust_version: rustc_version(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    /// Converts benchmark results to the format expected by BaselineStore.
    fn results_to_baseline_metrics(
        &self,
        results: &[BenchmarkResult],
    ) -> HashMap<String, HashMap<String, f64>> {
        let mut metrics: HashMap<String, HashMap<String, f64>> = HashMap::new();

        for result in results {
            let prefix = &result.workload_name;

            let mut case_metrics = HashMap::new();
            case_metrics
                .insert(format!("{prefix}::cold_start_mean_us"), result.cold_start.mean_us as f64);
            case_metrics
                .insert(format!("{prefix}::cold_start_p95_us"), result.cold_start.p95_us as f64);
            case_metrics.insert(
                format!("{prefix}::overhead_mean_us"),
                result.agent_loop_overhead.mean_us as f64,
            );
            case_metrics.insert(
                format!("{prefix}::overhead_p95_us"),
                result.agent_loop_overhead.p95_us as f64,
            );

            // For BaselineStore, we use metric_name → { case_id → value }
            // We invert: store workload metrics under a "timing" key
            metrics.entry("timing".to_string()).or_default().extend(case_metrics);
        }

        metrics
    }
}

/// Creates an LLM model instance from a model name string.
///
/// Currently supports Gemini models (default). The model is selected based on
/// the GOOGLE_API_KEY environment variable.
fn create_llm(model_name: &str) -> Result<Arc<dyn Llm>> {
    let api_key = std::env::var("GOOGLE_API_KEY").map_err(|_| {
        BenchError::Llm(
            "GOOGLE_API_KEY environment variable not set. \
             Set it to your Gemini API key to run benchmarks."
                .to_string(),
        )
    })?;

    let model = GeminiModel::new(api_key, model_name).map_err(|e| {
        BenchError::Llm(format!("failed to create Gemini model '{model_name}': {e}"))
    })?;

    Ok(Arc::new(model))
}

/// Creates simulated tools from workload tool definitions.
///
/// Each tool returns its `fixed_response` after sleeping for its
/// `simulated_latency_ms`. If no fixed response is defined, returns
/// a generic success object.
fn create_tools_from_workload(workload: &Workload) -> Vec<Arc<dyn adk_core::Tool>> {
    workload
        .agent
        .tools
        .iter()
        .map(|(name, def)| {
            let tool_name = name.clone();
            let description = def.description.clone();
            let fixed_response = def.fixed_response.clone();
            let latency_ms = def.simulated_latency_ms;

            let tool = FunctionTool::new(tool_name, description, move |_ctx, _args| {
                let response = fixed_response.clone();
                let latency = latency_ms;
                async move {
                    if latency > 0 {
                        tokio::time::sleep(Duration::from_millis(latency)).await;
                    }
                    Ok(response.unwrap_or(serde_json::json!({"status": "success"})))
                }
            })
            .with_read_only(true)
            .with_concurrency_safe(true);

            Arc::new(tool) as Arc<dyn adk_core::Tool>
        })
        .collect()
}

/// Executes a single workload against a real LLM using the full agent pipeline.
///
/// 1. Creates the model and wraps it in InstrumentedLlm
/// 2. Builds an LlmAgent with the workload's tools and instructions
/// 3. Runs through the Runner, collecting events
/// 4. Computes cold_start from InstrumentedLlm records
/// 5. Computes per-turn overhead = total_turn_time - llm_round_trip
async fn execute_workload_real(
    workload: &Workload,
    model_name: &str,
) -> Result<(Duration, Vec<Duration>)> {
    let run_start = Instant::now();

    // 1. Create model and wrap in InstrumentedLlm
    let inner_llm = create_llm(model_name)?;
    let instrumented = Arc::new(InstrumentedLlm::new(inner_llm));

    // 2. Build LlmAgent with workload tools and instructions
    let tools = create_tools_from_workload(workload);
    let mut agent_builder = LlmAgentBuilder::new(&workload.name)
        .model(instrumented.clone() as Arc<dyn Llm>)
        .instruction(&workload.agent.instructions);

    for tool in tools {
        agent_builder = agent_builder.tool(tool);
    }

    let agent = agent_builder
        .build()
        .map_err(|e| BenchError::Llm(format!("failed to build agent: {e}")))?;

    // 3. Create Runner with in-memory session
    let session_service = Arc::new(InMemorySessionService::new());

    // Pre-create the session so the Runner can find it
    let app_name = format!("bench-{}", workload.name);
    let session_id_str = format!("bench-{}", uuid_v4());
    session_service
        .create(adk_session::CreateRequest {
            app_name: app_name.clone(),
            user_id: "bench-user".to_string(),
            session_id: Some(session_id_str.clone()),
            state: HashMap::new(),
        })
        .await
        .map_err(|e| BenchError::Llm(format!("failed to create session: {e}")))?;

    let runner = Runner::builder()
        .app_name(app_name)
        .agent(Arc::new(agent))
        .session_service(session_service)
        .build()
        .map_err(|e| BenchError::Llm(format!("failed to create runner: {e}")))?;

    // 4. Run the agent with the workload's user message
    let user_content = Content::new("user").with_text(&workload.agent.user_message);

    let user_id = UserId::try_from("bench-user")
        .map_err(|e| BenchError::Llm(format!("invalid user id: {e}")))?;
    let session_id = SessionId::try_from(session_id_str.as_str())
        .map_err(|e| BenchError::Llm(format!("invalid session id: {e}")))?;

    let turn_start = Instant::now();
    let mut event_stream = runner
        .run(user_id, session_id, user_content)
        .await
        .map_err(|e| BenchError::Llm(format!("agent run failed: {e}")))?;

    // Consume all events
    while let Some(event_result) = event_stream.next().await {
        match event_result {
            Ok(_event) => {
                // Events consumed — timing captured by InstrumentedLlm
            }
            Err(e) => {
                tracing::warn!(error = %e, "event stream error during benchmark");
            }
        }
    }
    let total_turn_time = turn_start.elapsed();

    // 5. Compute metrics from InstrumentedLlm records
    let records = instrumented.records().await;

    // Cold start = time from run_start to first LLM call
    let cold_start = if let Some(first_record) = records.first() {
        first_record.request_sent.duration_since(run_start)
    } else {
        run_start.elapsed()
    };

    // Per-turn overhead = total_turn_time - sum(llm_round_trips)
    // If multiple LLM calls, compute overhead per call
    let total_llm_time: Duration = records.iter().map(|r| r.round_trip).sum();
    let overhead = total_turn_time.saturating_sub(total_llm_time);

    // Distribute overhead evenly across turns for per-turn reporting
    let num_turns = records.len().max(1);
    let per_turn_overhead = overhead / num_turns as u32;
    let overheads: Vec<Duration> = (0..num_turns).map(|_| per_turn_overhead).collect();

    tracing::debug!(
        workload = workload.name,
        cold_start_us = cold_start.as_micros(),
        total_turn_ms = total_turn_time.as_millis(),
        llm_calls = records.len(),
        total_llm_ms = total_llm_time.as_millis(),
        overhead_us = overhead.as_micros(),
        "workload execution complete"
    );

    Ok((cold_start, overheads))
}

/// Generates a simple UUID v4 string for session IDs.
fn uuid_v4() -> String {
    use std::time::SystemTime;
    let nanos =
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{:032x}", nanos)
}

/// Returns the Rust compiler version string.
fn rustc_version() -> String {
    // Use a compile-time constant for the Rust version
    option_env!("RUSTC_VERSION").unwrap_or(env!("CARGO_PKG_RUST_VERSION")).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BenchConfig {
        BenchConfig { runs: 3, warmup: 1, concurrency: 1, ..Default::default() }
    }

    #[tokio::test]
    async fn test_bench_runner_new() {
        let config = test_config();
        let runner = BenchRunner::new(config.clone());
        assert_eq!(runner.config.runs, 3);
        assert_eq!(runner.config.warmup, 1);
    }

    #[tokio::test]
    async fn test_resolve_workloads_all_builtin() {
        let config = test_config();
        let runner = BenchRunner::new(config);
        let workloads = runner.resolve_workloads().unwrap();
        assert_eq!(workloads.len(), 3);
    }

    #[tokio::test]
    async fn test_resolve_workloads_with_experimental() {
        let config = BenchConfig { experimental: true, ..test_config() };
        let runner = BenchRunner::new(config);
        let workloads = runner.resolve_workloads().unwrap();
        assert_eq!(workloads.len(), 4);
    }

    #[tokio::test]
    async fn test_resolve_workloads_specific_builtin() {
        let config =
            BenchConfig { workload: Some("simple_tool_call".to_string()), ..test_config() };
        let runner = BenchRunner::new(config);
        let workloads = runner.resolve_workloads().unwrap();
        assert_eq!(workloads.len(), 1);
        assert_eq!(workloads[0].name, "simple_tool_call");
    }

    #[tokio::test]
    async fn test_resolve_workloads_not_found() {
        let config =
            BenchConfig { workload: Some("nonexistent_workload".to_string()), ..test_config() };
        let runner = BenchRunner::new(config);
        let result = runner.resolve_workloads();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dry_run_returns_empty() {
        let config = BenchConfig { dry_run: true, ..test_config() };
        let runner = BenchRunner::new(config);
        let results = runner.run().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_max_cost_usd_abort() {
        let config = BenchConfig {
            max_cost_usd: Some(0.0001), // Extremely low limit
            runs: 100,
            ..test_config()
        };
        let runner = BenchRunner::new(config);
        let result = runner.run().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // Requires GOOGLE_API_KEY
    async fn test_sequential_run() {
        let config = BenchConfig {
            workload: Some("simple_tool_call".to_string()),
            runs: 2,
            warmup: 1,
            confirm_cost: true,
            ..test_config()
        };
        let runner = BenchRunner::new(config);
        let results = runner.run().await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].workload_name, "simple_tool_call");
        assert_eq!(results[0].iterations, 2);
        assert!(results[0].throughput.is_none());
    }

    #[tokio::test]
    #[ignore] // Requires GOOGLE_API_KEY
    async fn test_concurrent_run() {
        let config = BenchConfig {
            workload: Some("simple_tool_call".to_string()),
            runs: 2,
            warmup: 1,
            concurrency: 4,
            confirm_cost: true,
            ..test_config()
        };
        let runner = BenchRunner::new(config);
        let results = runner.run().await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].throughput.is_some());
        let throughput = results[0].throughput.as_ref().unwrap();
        assert_eq!(throughput.levels.len(), 1);
        assert_eq!(throughput.levels[0].concurrency, 4);
    }

    #[tokio::test]
    #[ignore] // Requires GOOGLE_API_KEY
    async fn test_sweep_mode() {
        let config = BenchConfig {
            workload: Some("simple_tool_call".to_string()),
            runs: 1,
            warmup: 1,
            concurrency_sweep: Some(vec![1, 2, 4]),
            confirm_cost: true,
            ..test_config()
        };
        let runner = BenchRunner::new(config);
        let results = runner.run().await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].throughput.is_some());
        let throughput = results[0].throughput.as_ref().unwrap();
        assert_eq!(throughput.levels.len(), 3);
        assert_eq!(throughput.levels[0].concurrency, 1);
        assert_eq!(throughput.levels[1].concurrency, 2);
        assert_eq!(throughput.levels[2].concurrency, 4);
    }

    #[tokio::test]
    async fn test_cv_warning_not_emitted_for_low_cv() {
        let stats = DurationStats {
            min_us: 100,
            max_us: 120,
            mean_us: 110,
            median_us: 110,
            p95_us: 119,
            p99_us: 120,
            std_dev_us: 5,
            count: 10,
            coefficient_of_variation: 0.045, // 4.5%, below 20%
        };
        let config = test_config();
        let runner = BenchRunner::new(config);
        // This should not panic or produce errors
        runner.emit_cv_warning(&stats, "test_workload");
    }

    #[tokio::test]
    async fn test_save_and_check_baseline() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config = BenchConfig { baseline_path: baseline_path.clone(), ..test_config() };
        let runner = BenchRunner::new(config);

        // Create a sample result
        let results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        // Save baseline
        runner.save_baseline(&results).unwrap();
        assert!(baseline_path.exists());

        // Check regression with same results should find none
        let regressions = runner.check_regression(&results).unwrap();
        assert!(regressions.is_empty());
    }

    #[tokio::test]
    async fn test_check_regression_detects_timing_increase() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config = BenchConfig {
            baseline_path: baseline_path.clone(),
            tolerance: 0.10, // 10% tolerance
            ..test_config()
        };
        let runner = BenchRunner::new(config);

        // Save baseline with 1000μs cold start
        let baseline_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];
        runner.save_baseline(&baseline_results).unwrap();

        // Current results with 20% worse cold start (1200μs vs 1000μs)
        let current_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-02T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1200)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        let regressions = runner.check_regression(&current_results).unwrap();
        // Should detect regression: (1200 - 1000) / 1000 = 0.20 > 0.10 tolerance
        assert!(!regressions.is_empty(), "expected regression for 20% cold start increase");

        // Verify the report has correct values
        let cold_start_regression = regressions
            .iter()
            .find(|r| r.metric_name.contains("cold_start"))
            .expect("should have cold_start regression");
        assert_eq!(cold_start_regression.workload_name, "test_workload");
        assert!((cold_start_regression.degradation - 0.20).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_check_regression_within_tolerance() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config = BenchConfig {
            baseline_path: baseline_path.clone(),
            tolerance: 0.10, // 10% tolerance
            ..test_config()
        };
        let runner = BenchRunner::new(config);

        // Save baseline with 1000μs cold start
        let baseline_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];
        runner.save_baseline(&baseline_results).unwrap();

        // Current results with 5% worse cold start (1050μs vs 1000μs)
        let current_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-02T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1050)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(105)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        let regressions = runner.check_regression(&current_results).unwrap();
        // 5% increase is within 10% tolerance — no regression
        assert!(
            regressions.is_empty(),
            "expected no regression for 5% increase within 10% tolerance"
        );
    }

    #[tokio::test]
    async fn test_check_regression_improvement_not_flagged() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config =
            BenchConfig { baseline_path: baseline_path.clone(), tolerance: 0.10, ..test_config() };
        let runner = BenchRunner::new(config);

        // Save baseline with 1000μs cold start
        let baseline_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];
        runner.save_baseline(&baseline_results).unwrap();

        // Current results are *better* (lower timing — improvement)
        let current_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-02T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(800)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(80)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        let regressions = runner.check_regression(&current_results).unwrap();
        // Improvements should never be flagged as regression
        assert!(regressions.is_empty(), "improvement should not be flagged as regression");
    }

    #[tokio::test]
    async fn test_check_regression_no_baseline_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("nonexistent-baseline.json");

        let config =
            BenchConfig { baseline_path: baseline_path.clone(), tolerance: 0.10, ..test_config() };
        let runner = BenchRunner::new(config);

        let results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        // No baseline file — should return empty, not error
        let regressions = runner.check_regression(&results).unwrap();
        assert!(regressions.is_empty());
    }

    #[tokio::test]
    async fn test_check_regression_exact_tolerance_boundary() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config = BenchConfig {
            baseline_path: baseline_path.clone(),
            tolerance: 0.10, // exactly 10%
            ..test_config()
        };
        let runner = BenchRunner::new(config);

        // Save baseline with 1000μs cold start
        let baseline_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];
        runner.save_baseline(&baseline_results).unwrap();

        // Current results with exactly 10% degradation (1100μs vs 1000μs)
        // (1100 - 1000) / 1000 = 0.10, which equals tolerance but does NOT exceed it
        let current_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "test_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-02T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1100)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(110)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        let regressions = runner.check_regression(&current_results).unwrap();
        // Exactly at tolerance boundary — should NOT be flagged (strictly greater than)
        assert!(
            regressions.is_empty(),
            "exactly at tolerance boundary should not trigger regression"
        );
    }

    #[tokio::test]
    async fn test_check_regression_multiple_workloads() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config =
            BenchConfig { baseline_path: baseline_path.clone(), tolerance: 0.10, ..test_config() };
        let runner = BenchRunner::new(config);

        // Save baseline with two workloads
        let baseline_results = vec![
            BenchmarkResult {
                schema_version: 1,
                workload_name: "workload_a".to_string(),
                model: "gemini-2.5-flash".to_string(),
                metadata: RunMetadata {
                    timestamp: "2025-01-01T00:00:00Z".to_string(),
                    adk_version: "0.5.0".to_string(),
                    rust_version: "1.85.0".to_string(),
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                },
                cold_start: compute_stats(&[Duration::from_micros(1000)]),
                agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
                tool_invocation: None,
                throughput: None,
                memory: None,
                token_overhead: None,
                reproducibility_rate: None,
                iterations: 5,
            },
            BenchmarkResult {
                schema_version: 1,
                workload_name: "workload_b".to_string(),
                model: "gemini-2.5-flash".to_string(),
                metadata: RunMetadata {
                    timestamp: "2025-01-01T00:00:00Z".to_string(),
                    adk_version: "0.5.0".to_string(),
                    rust_version: "1.85.0".to_string(),
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                },
                cold_start: compute_stats(&[Duration::from_micros(2000)]),
                agent_loop_overhead: compute_stats(&[Duration::from_micros(200)]),
                tool_invocation: None,
                throughput: None,
                memory: None,
                token_overhead: None,
                reproducibility_rate: None,
                iterations: 5,
            },
        ];
        runner.save_baseline(&baseline_results).unwrap();

        // workload_a regresses (30%), workload_b stays the same
        let current_results = vec![
            BenchmarkResult {
                schema_version: 1,
                workload_name: "workload_a".to_string(),
                model: "gemini-2.5-flash".to_string(),
                metadata: RunMetadata {
                    timestamp: "2025-01-02T00:00:00Z".to_string(),
                    adk_version: "0.5.0".to_string(),
                    rust_version: "1.85.0".to_string(),
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                },
                cold_start: compute_stats(&[Duration::from_micros(1300)]),
                agent_loop_overhead: compute_stats(&[Duration::from_micros(100)]),
                tool_invocation: None,
                throughput: None,
                memory: None,
                token_overhead: None,
                reproducibility_rate: None,
                iterations: 5,
            },
            BenchmarkResult {
                schema_version: 1,
                workload_name: "workload_b".to_string(),
                model: "gemini-2.5-flash".to_string(),
                metadata: RunMetadata {
                    timestamp: "2025-01-02T00:00:00Z".to_string(),
                    adk_version: "0.5.0".to_string(),
                    rust_version: "1.85.0".to_string(),
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                },
                cold_start: compute_stats(&[Duration::from_micros(2000)]),
                agent_loop_overhead: compute_stats(&[Duration::from_micros(200)]),
                tool_invocation: None,
                throughput: None,
                memory: None,
                token_overhead: None,
                reproducibility_rate: None,
                iterations: 5,
            },
        ];

        let regressions = runner.check_regression(&current_results).unwrap();
        // Only workload_a should have regression (cold_start 30% increase)
        assert!(!regressions.is_empty());
        let workload_a_regressions: Vec<_> =
            regressions.iter().filter(|r| r.workload_name == "workload_a").collect();
        assert!(!workload_a_regressions.is_empty(), "workload_a should have regressions");

        let workload_b_regressions: Vec<_> =
            regressions.iter().filter(|r| r.workload_name == "workload_b").collect();
        assert!(workload_b_regressions.is_empty(), "workload_b should not have regressions");
    }

    #[tokio::test]
    async fn test_regression_report_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let baseline_path = dir.path().join("test-baseline.json");

        let config =
            BenchConfig { baseline_path: baseline_path.clone(), tolerance: 0.10, ..test_config() };
        let runner = BenchRunner::new(config);

        // Save baseline
        let baseline_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "my_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1000)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(200)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];
        runner.save_baseline(&baseline_results).unwrap();

        // 50% regression on cold start
        let current_results = vec![BenchmarkResult {
            schema_version: 1,
            workload_name: "my_workload".to_string(),
            model: "gemini-2.5-flash".to_string(),
            metadata: RunMetadata {
                timestamp: "2025-01-02T00:00:00Z".to_string(),
                adk_version: "0.5.0".to_string(),
                rust_version: "1.85.0".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
            cold_start: compute_stats(&[Duration::from_micros(1500)]),
            agent_loop_overhead: compute_stats(&[Duration::from_micros(200)]),
            tool_invocation: None,
            throughput: None,
            memory: None,
            token_overhead: None,
            reproducibility_rate: None,
            iterations: 5,
        }];

        let regressions = runner.check_regression(&current_results).unwrap();
        assert!(!regressions.is_empty());

        // Find the cold_start_mean_us regression
        let report = regressions
            .iter()
            .find(|r| r.metric_name == "cold_start_mean_us")
            .expect("should have cold_start_mean_us regression");

        assert_eq!(report.workload_name, "my_workload");
        assert!((report.baseline_value - 1000.0).abs() < 1.0);
        assert!((report.current_value - 1500.0).abs() < 1.0);
        assert!((report.degradation - 0.50).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_estimate_cost_non_zero() {
        let config = test_config();
        let runner = BenchRunner::new(config);
        let workloads = runner.resolve_workloads().unwrap();
        let cost = runner.estimate_cost(&workloads);
        // With default pricing for gemini-2.5-flash and 3 workloads, cost should be > 0
        assert!(cost >= 0.0);
    }

    #[tokio::test]
    async fn test_build_run_metadata() {
        let config = test_config();
        let runner = BenchRunner::new(config);
        let metadata = runner.build_run_metadata();
        assert!(!metadata.timestamp.is_empty());
        assert!(!metadata.adk_version.is_empty());
        assert!(!metadata.os.is_empty());
        assert!(!metadata.arch.is_empty());
    }
}
