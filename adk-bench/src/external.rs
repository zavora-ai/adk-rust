//! External framework comparison via subprocess execution.
//!
//! Implements the External Benchmark Protocol (EBP) for running competitor
//! frameworks against the same workloads. External scripts receive the workload
//! JSON path as their last CLI argument and `BENCH_START_EPOCH_NS` in their
//! environment. They must emit exactly one JSON object on stdout conforming to
//! [`ExternalMetricsOutput`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::{ExternalRunner, ExternalFrameworkConfig};
//!
//! let runner = ExternalRunner::new(300);
//! let config = ExternalFrameworkConfig {
//!     name: "langgraph".to_string(),
//!     command: "python".to_string(),
//!     args: vec!["-m".to_string(), "bench_langgraph".to_string()],
//!     working_dir: None,
//!     env: vec![],
//! };
//! let metrics = runner.run(&config, "/path/to/workload.json").await?;
//! println!("Framework: {}", metrics.framework);
//! ```

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::config::ExternalFrameworkConfig;

/// External Benchmark Protocol (EBP) — the JSON schema that all external
/// framework benchmark scripts MUST emit on stdout.
///
/// This is the contract between adk-bench and any competitor framework harness.
/// External scripts receive: the workload JSON path as last CLI arg, and
/// `BENCH_START_EPOCH_NS` in their environment (monotonic nanosecond timestamp
/// at subprocess spawn time).
///
/// They MUST output exactly one JSON object (no other stdout content):
/// ```json
/// {
///   "framework": "langgraph",
///   "cold_start_us": 45000,
///   "first_llm_call_epoch_ns": 1705312800000045000,
///   "loop_overhead": {
///     "min_us": 120, "max_us": 890, "mean_us": 340,
///     "median_us": 310, "p95_us": 780, "p99_us": 870, "count": 10
///   },
///   "throughput_agents_per_sec": 12.5,
///   "peak_rss_bytes": 52428800,
///   "token_overhead": {
///     "total_tokens": 1200, "user_content_tokens": 950, "overhead_tokens": 250
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalMetricsOutput {
    /// Framework name (e.g., "adk-python", "langgraph", "crewai").
    pub framework: String,

    /// Cold start time in microseconds (subprocess spawn → first LLM call).
    pub cold_start_us: u64,

    /// Monotonic nanosecond timestamp when the first LLM call was made.
    /// Used with `BENCH_START_EPOCH_NS` to compute cold start from the external clock.
    pub first_llm_call_epoch_ns: u64,

    /// Per-turn framework overhead statistics (LLM time subtracted).
    pub loop_overhead: ExternalDurationStats,

    /// Peak RSS in bytes (null if platform doesn't support measurement).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,

    /// Agents completed per second at the requested concurrency (null if not measured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throughput_agents_per_sec: Option<f64>,

    /// Token overhead breakdown (null if not measured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_overhead: Option<ExternalTokenOverhead>,
}

/// Duration statistics reported by external frameworks in the EBP protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalDurationStats {
    /// Minimum overhead in microseconds.
    pub min_us: u64,
    /// Maximum overhead in microseconds.
    pub max_us: u64,
    /// Mean overhead in microseconds.
    pub mean_us: u64,
    /// Median overhead in microseconds.
    pub median_us: u64,
    /// 95th percentile overhead in microseconds.
    pub p95_us: u64,
    /// 99th percentile overhead in microseconds.
    pub p99_us: u64,
    /// Number of measurements.
    pub count: u64,
}

/// Token overhead breakdown reported by external frameworks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalTokenOverhead {
    /// Total tokens sent to LLM.
    pub total_tokens: u64,
    /// Tokens from user content only.
    pub user_content_tokens: u64,
    /// Framework overhead tokens (total - user_content).
    pub overhead_tokens: u64,
}

/// Runs external framework benchmarks as subprocesses.
///
/// Injects `BENCH_START_EPOCH_NS` into the subprocess environment,
/// passes the workload JSON path as the last argument, and parses
/// the EBP JSON output from stdout.
///
/// # Example
///
/// ```rust
/// use adk_bench::ExternalRunner;
///
/// let runner = ExternalRunner::new(300);
/// assert_eq!(runner.timeout(), std::time::Duration::from_secs(300));
/// ```
pub struct ExternalRunner {
    timeout: Duration,
}

impl ExternalRunner {
    /// Creates a new `ExternalRunner` with the specified timeout in seconds.
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout: Duration::from_secs(timeout_secs) }
    }

    /// Returns the configured timeout duration.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Executes the external framework and parses its EBP JSON output.
    ///
    /// The subprocess receives:
    /// - `BENCH_START_EPOCH_NS` env var (monotonic clock nanosecond timestamp at spawn time)
    /// - Workload JSON file path as the last CLI argument
    ///
    /// Returns error if subprocess times out, exits non-zero, or emits invalid JSON.
    ///
    /// # Errors
    ///
    /// - [`BenchError::ExternalTimeout`] if the subprocess exceeds the configured timeout
    /// - [`BenchError::ExternalRunner`] if the subprocess exits with non-zero status or emits invalid JSON
    pub async fn run(
        &self,
        config: &ExternalFrameworkConfig,
        workload_path: &str,
    ) -> crate::Result<ExternalMetricsOutput> {
        info!(
            framework = %config.name,
            command = %config.command,
            workload = %workload_path,
            "starting external framework benchmark"
        );

        // Get current monotonic time as epoch nanoseconds for BENCH_START_EPOCH_NS.
        // We use system time as nanoseconds since UNIX epoch to provide a shared
        // clock reference between the parent and child process.
        let start_epoch_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        // Build the command with args from ExternalFrameworkConfig.
        let mut cmd = Command::new(&config.command);

        // Add configured arguments.
        cmd.args(&config.args);

        // Append workload path as the last argument.
        cmd.arg(workload_path);

        // Inject BENCH_START_EPOCH_NS environment variable.
        cmd.env("BENCH_START_EPOCH_NS", start_epoch_ns.to_string());

        // Add any additional configured environment variables.
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        // Set working directory if configured.
        if let Some(working_dir) = &config.working_dir {
            cmd.current_dir(working_dir);
        }

        // Capture stdout and stderr.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        debug!(
            framework = %config.name,
            start_epoch_ns = start_epoch_ns,
            timeout_secs = self.timeout.as_secs(),
            "spawning external framework subprocess"
        );

        // Execute with timeout using tokio::time::timeout.
        let output = match tokio::time::timeout(self.timeout, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(io_err)) => {
                return Err(crate::BenchError::ExternalRunner {
                    framework: config.name.clone(),
                    reason: format!("failed to spawn subprocess: {io_err}"),
                });
            }
            Err(_elapsed) => {
                warn!(
                    framework = %config.name,
                    timeout_secs = self.timeout.as_secs(),
                    "external framework timed out"
                );
                return Err(crate::BenchError::ExternalTimeout {
                    framework: config.name.clone(),
                    timeout_secs: self.timeout.as_secs(),
                });
            }
        };

        // Check for non-zero exit status.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);
            warn!(
                framework = %config.name,
                exit_code = exit_code,
                stderr = %stderr,
                "external framework exited with non-zero status"
            );
            return Err(crate::BenchError::ExternalRunner {
                framework: config.name.clone(),
                reason: format!("subprocess exited with code {exit_code}: {}", stderr.trim()),
            });
        }

        // Parse JSON stdout into ExternalMetricsOutput.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout_trimmed = stdout.trim();

        debug!(
            framework = %config.name,
            stdout_len = stdout_trimmed.len(),
            "parsing external framework EBP output"
        );

        let mut metrics: ExternalMetricsOutput =
            serde_json::from_str(stdout_trimmed).map_err(|e| {
                crate::BenchError::ExternalRunner {
                    framework: config.name.clone(),
                    reason: format!("failed to parse EBP JSON output: {e}"),
                }
            })?;

        // Compute cold_start from first_llm_call_epoch_ns - BENCH_START_EPOCH_NS
        // if the reported cold_start_us seems like a placeholder (0).
        // The authoritative cold start is always: first_llm_call_epoch_ns - start_epoch_ns.
        let computed_cold_start_ns = metrics.first_llm_call_epoch_ns.saturating_sub(start_epoch_ns);
        let computed_cold_start_us = computed_cold_start_ns / 1000;

        // Use the computed cold start from the external clock source for consistency.
        metrics.cold_start_us = computed_cold_start_us;

        info!(
            framework = %metrics.framework,
            cold_start_us = computed_cold_start_us,
            loop_overhead_mean_us = metrics.loop_overhead.mean_us,
            "external framework benchmark completed"
        );

        Ok(metrics)
    }
}

/// Configuration file format for loading multiple external framework configs.
///
/// # Example JSON
///
/// ```json
/// {
///   "frameworks": [
///     {
///       "name": "adk-python",
///       "command": "python",
///       "args": ["-m", "adk_bench", "--workload"],
///       "workingDir": "../adk-python",
///       "env": [["GOOGLE_API_KEY", "${GOOGLE_API_KEY}"]]
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalConfigFile {
    /// List of external framework configurations.
    pub frameworks: Vec<ExternalFrameworkConfig>,
}

/// Loads external framework configurations from a JSON config file.
///
/// # Errors
///
/// Returns [`BenchError::Io`] if the file cannot be read, or
/// [`BenchError::Serialization`] if the JSON is invalid.
///
/// # Example
///
/// ```rust,ignore
/// use adk_bench::external::load_external_configs;
/// use std::path::Path;
///
/// let configs = load_external_configs(Path::new("external-bench.json"))?;
/// for config in &configs {
///     println!("Framework: {}", config.name);
/// }
/// ```
pub fn load_external_configs(path: &Path) -> crate::Result<Vec<ExternalFrameworkConfig>> {
    let content = std::fs::read_to_string(path)?;
    let config_file: ExternalConfigFile = serde_json::from_str(&content).map_err(|e| {
        crate::BenchError::Serialization(format!(
            "failed to parse external config file '{}': {e}",
            path.display()
        ))
    })?;
    Ok(config_file.frameworks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_external_metrics_output_deserialize() {
        let json = r#"{
            "framework": "langgraph",
            "cold_start_us": 45000,
            "first_llm_call_epoch_ns": 1705312800000045000,
            "loop_overhead": {
                "min_us": 120,
                "max_us": 890,
                "mean_us": 340,
                "median_us": 310,
                "p95_us": 780,
                "p99_us": 870,
                "count": 10
            },
            "throughput_agents_per_sec": 12.5,
            "peak_rss_bytes": 52428800,
            "token_overhead": {
                "total_tokens": 1200,
                "user_content_tokens": 950,
                "overhead_tokens": 250
            }
        }"#;

        let metrics: ExternalMetricsOutput = serde_json::from_str(json).unwrap();
        assert_eq!(metrics.framework, "langgraph");
        assert_eq!(metrics.cold_start_us, 45000);
        assert_eq!(metrics.first_llm_call_epoch_ns, 1705312800000045000);
        assert_eq!(metrics.loop_overhead.min_us, 120);
        assert_eq!(metrics.loop_overhead.max_us, 890);
        assert_eq!(metrics.loop_overhead.mean_us, 340);
        assert_eq!(metrics.loop_overhead.median_us, 310);
        assert_eq!(metrics.loop_overhead.p95_us, 780);
        assert_eq!(metrics.loop_overhead.p99_us, 870);
        assert_eq!(metrics.loop_overhead.count, 10);
        assert_eq!(metrics.throughput_agents_per_sec, Some(12.5));
        assert_eq!(metrics.peak_rss_bytes, Some(52428800));
        let token_overhead = metrics.token_overhead.unwrap();
        assert_eq!(token_overhead.total_tokens, 1200);
        assert_eq!(token_overhead.user_content_tokens, 950);
        assert_eq!(token_overhead.overhead_tokens, 250);
    }

    #[test]
    fn test_external_metrics_output_deserialize_minimal() {
        let json = r#"{
            "framework": "crewai",
            "cold_start_us": 120000,
            "first_llm_call_epoch_ns": 1705312800000120000,
            "loop_overhead": {
                "min_us": 500,
                "max_us": 2000,
                "mean_us": 1000,
                "median_us": 900,
                "p95_us": 1800,
                "p99_us": 1950,
                "count": 5
            }
        }"#;

        let metrics: ExternalMetricsOutput = serde_json::from_str(json).unwrap();
        assert_eq!(metrics.framework, "crewai");
        assert_eq!(metrics.cold_start_us, 120000);
        assert_eq!(metrics.peak_rss_bytes, None);
        assert_eq!(metrics.throughput_agents_per_sec, None);
        assert_eq!(metrics.token_overhead, None);
    }

    #[test]
    fn test_external_metrics_output_serialize_roundtrip() {
        let metrics = ExternalMetricsOutput {
            framework: "test-framework".to_string(),
            cold_start_us: 5000,
            first_llm_call_epoch_ns: 1000000005000000,
            loop_overhead: ExternalDurationStats {
                min_us: 100,
                max_us: 500,
                mean_us: 250,
                median_us: 230,
                p95_us: 450,
                p99_us: 490,
                count: 20,
            },
            peak_rss_bytes: Some(1024 * 1024 * 50),
            throughput_agents_per_sec: Some(8.5),
            token_overhead: Some(ExternalTokenOverhead {
                total_tokens: 1000,
                user_content_tokens: 800,
                overhead_tokens: 200,
            }),
        };

        let json = serde_json::to_string(&metrics).unwrap();
        let deserialized: ExternalMetricsOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(metrics, deserialized);
    }

    #[test]
    fn test_external_runner_new() {
        let runner = ExternalRunner::new(120);
        assert_eq!(runner.timeout(), Duration::from_secs(120));
    }

    #[test]
    fn test_external_runner_default_timeout() {
        let runner = ExternalRunner::new(300);
        assert_eq!(runner.timeout(), Duration::from_secs(300));
    }

    #[test]
    fn test_external_config_file_deserialize() {
        let json = r#"{
            "frameworks": [
                {
                    "name": "adk-python",
                    "command": "python",
                    "args": ["-m", "adk_bench", "--workload"],
                    "workingDir": "../adk-python",
                    "env": [["GOOGLE_API_KEY", "test-key"]]
                },
                {
                    "name": "langgraph",
                    "command": "python",
                    "args": ["bench_runner.py"],
                    "env": []
                }
            ]
        }"#;

        let config_file: ExternalConfigFile = serde_json::from_str(json).unwrap();
        assert_eq!(config_file.frameworks.len(), 2);
        assert_eq!(config_file.frameworks[0].name, "adk-python");
        assert_eq!(config_file.frameworks[0].command, "python");
        assert_eq!(config_file.frameworks[0].args, vec!["-m", "adk_bench", "--workload"]);
        assert_eq!(
            config_file.frameworks[0].working_dir,
            Some(std::path::PathBuf::from("../adk-python"))
        );
        assert_eq!(config_file.frameworks[1].name, "langgraph");
        assert_eq!(config_file.frameworks[1].working_dir, None);
    }

    #[test]
    fn test_load_external_configs_file_not_found() {
        let result = load_external_configs(Path::new("/nonexistent/path/config.json"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_external_runner_spawn_failure() {
        let runner = ExternalRunner::new(10);
        let config = ExternalFrameworkConfig {
            name: "nonexistent".to_string(),
            command: "/this/command/does/not/exist/anywhere".to_string(),
            args: vec![],
            working_dir: None,
            env: vec![],
        };

        let result = runner.run(&config, "/tmp/workload.json").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            crate::BenchError::ExternalRunner { framework, reason } => {
                assert_eq!(framework, "nonexistent");
                assert!(reason.contains("failed to spawn subprocess"));
            }
            _ => panic!("expected ExternalRunner error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_external_runner_non_zero_exit() {
        let runner = ExternalRunner::new(10);
        let config = ExternalFrameworkConfig {
            name: "failing-script".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            working_dir: None,
            env: vec![],
        };

        let result = runner.run(&config, "/tmp/workload.json").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            crate::BenchError::ExternalRunner { framework, .. } => {
                assert_eq!(framework, "failing-script");
            }
            _ => panic!("expected ExternalRunner error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_external_runner_invalid_json() {
        let runner = ExternalRunner::new(10);
        let config = ExternalFrameworkConfig {
            name: "bad-json".to_string(),
            command: "echo".to_string(),
            args: vec!["not valid json".to_string()],
            working_dir: None,
            env: vec![],
        };

        let result = runner.run(&config, "/tmp/workload.json").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            crate::BenchError::ExternalRunner { framework, reason } => {
                assert_eq!(framework, "bad-json");
                assert!(reason.contains("failed to parse EBP JSON output"));
            }
            _ => panic!("expected ExternalRunner error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_external_runner_timeout() {
        let runner = ExternalRunner::new(1); // 1 second timeout
        let config = ExternalFrameworkConfig {
            name: "slow-script".to_string(),
            command: "sh".to_string(),
            // The workload path will be appended as the last arg, but the script ignores it.
            args: vec!["-c".to_string(), "sleep 10; #".to_string()],
            working_dir: None,
            env: vec![],
        };

        let result = runner.run(&config, "/tmp/workload.json").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            crate::BenchError::ExternalTimeout { framework, timeout_secs } => {
                assert_eq!(framework, "slow-script");
                assert_eq!(timeout_secs, 1);
            }
            _ => panic!("expected ExternalTimeout error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_external_runner_valid_output() {
        // Use a shell command that outputs valid EBP JSON, ignoring extra args.
        let ebp_json = r#"{"framework":"test","cold_start_us":1000,"first_llm_call_epoch_ns":99999999999999999,"loop_overhead":{"min_us":10,"max_us":100,"mean_us":50,"median_us":45,"p95_us":90,"p99_us":95,"count":5}}"#;

        let runner = ExternalRunner::new(10);
        let config = ExternalFrameworkConfig {
            name: "test-framework".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), format!("echo '{}'; #", ebp_json)],
            working_dir: None,
            env: vec![],
        };

        let result = runner.run(&config, "/tmp/workload.json").await;
        assert!(result.is_ok());
        let metrics = result.unwrap();
        assert_eq!(metrics.framework, "test");
        assert_eq!(metrics.loop_overhead.min_us, 10);
        assert_eq!(metrics.loop_overhead.count, 5);
        assert_eq!(metrics.peak_rss_bytes, None);
        assert_eq!(metrics.throughput_agents_per_sec, None);
        assert_eq!(metrics.token_overhead, None);
    }

    #[tokio::test]
    async fn test_external_runner_env_injection() {
        // Verify that BENCH_START_EPOCH_NS is injected and custom env vars are passed.
        let runner = ExternalRunner::new(10);
        let config = ExternalFrameworkConfig {
            name: "env-test".to_string(),
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                // Output EBP JSON using the injected env var as first_llm_call_epoch_ns.
                // The workload path will be the next positional arg but -c ignores it.
                r#"FIRST_CALL=$(expr $BENCH_START_EPOCH_NS + 5000000); echo "{\"framework\":\"env-test\",\"cold_start_us\":0,\"first_llm_call_epoch_ns\":$FIRST_CALL,\"loop_overhead\":{\"min_us\":1,\"max_us\":2,\"mean_us\":1,\"median_us\":1,\"p95_us\":2,\"p99_us\":2,\"count\":1}}"; #"#.to_string(),
            ],
            working_dir: None,
            env: vec![("CUSTOM_VAR".to_string(), "hello".to_string())],
        };

        let result = runner.run(&config, "/tmp/workload.json").await;
        assert!(result.is_ok(), "run failed: {:?}", result.unwrap_err());
        let metrics = result.unwrap();
        assert_eq!(metrics.framework, "env-test");
        // cold_start should be computed as first_llm_call_epoch_ns - start_epoch_ns
        // which should be approximately 5000000ns = 5000us
        assert_eq!(metrics.cold_start_us, 5000);
    }
}
