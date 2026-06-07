//! τ²-bench task quality adapter.
//!
//! Implements the τ²-bench agent interface and routes agent requests
//! through `adk-runner` with real LLM calls. Translates between the
//! τ²-bench protocol format and ADK-Rust's Event/Content model, then
//! reports task completion scores in the τ²-bench standard output format.
//!
//! # Architecture
//!
//! The adapter follows these steps:
//! 1. Load τ²-bench scenarios from a dataset directory
//! 2. For each scenario, translate it into an ADK-Rust agent session
//! 3. Route agent execution through `adk-runner` with a real LLM
//! 4. Evaluate task completion against τ²-bench scoring criteria
//! 5. Aggregate and report scores in τ²-bench standard format
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::adapters::tau2::{Tau2Adapter, Tau2Config};
//!
//! let config = Tau2Config::builder()
//!     .dataset_path("./tau2-bench/scenarios")
//!     .max_scenarios(50)
//!     .build();
//!
//! let adapter = Tau2Adapter::new(config);
//! let result = adapter.run("gemini-2.5-flash").await?;
//! println!("Accuracy: {:.1}%", result.accuracy * 100.0);
//! ```

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{CaseResult, TaskQualityAdapter, TaskQualityResult};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the τ²-bench adapter.
#[derive(Debug, Clone)]
pub struct Tau2Config {
    /// Path to the τ²-bench scenario dataset directory.
    pub dataset_path: PathBuf,
    /// Maximum number of scenarios to execute (None = all).
    pub max_scenarios: Option<usize>,
    /// Maximum number of agent turns allowed per scenario.
    pub max_turns_per_scenario: usize,
    /// Timeout in seconds for each scenario execution.
    pub scenario_timeout_secs: u64,
    /// Whether to include detailed scoring breakdowns in results.
    pub verbose_scoring: bool,
}

impl Default for Tau2Config {
    fn default() -> Self {
        Self {
            dataset_path: PathBuf::from("./tau2-bench/scenarios"),
            max_scenarios: None,
            max_turns_per_scenario: 20,
            scenario_timeout_secs: 120,
            verbose_scoring: false,
        }
    }
}

impl Tau2Config {
    /// Creates a new builder for `Tau2Config`.
    pub fn builder() -> Tau2ConfigBuilder {
        Tau2ConfigBuilder::default()
    }
}

/// Builder for [`Tau2Config`].
#[derive(Debug, Clone, Default)]
pub struct Tau2ConfigBuilder {
    config: Tau2Config,
}

impl Tau2ConfigBuilder {
    /// Sets the path to the τ²-bench scenario dataset.
    pub fn dataset_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.dataset_path = path.into();
        self
    }

    /// Sets the maximum number of scenarios to execute.
    pub fn max_scenarios(mut self, max: usize) -> Self {
        self.config.max_scenarios = Some(max);
        self
    }

    /// Sets the maximum number of agent turns per scenario.
    pub fn max_turns_per_scenario(mut self, max: usize) -> Self {
        self.config.max_turns_per_scenario = max;
        self
    }

    /// Sets the timeout for each scenario execution in seconds.
    pub fn scenario_timeout_secs(mut self, secs: u64) -> Self {
        self.config.scenario_timeout_secs = secs;
        self
    }

    /// Enables or disables verbose scoring output.
    pub fn verbose_scoring(mut self, verbose: bool) -> Self {
        self.config.verbose_scoring = verbose;
        self
    }

    /// Builds the [`Tau2Config`].
    pub fn build(self) -> Tau2Config {
        self.config
    }
}

// ---------------------------------------------------------------------------
// τ²-bench Protocol Types
// ---------------------------------------------------------------------------

/// A τ²-bench scenario definition.
///
/// Represents a single task that an agent must complete, including the
/// environment setup, available actions, and success criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2Scenario {
    /// Unique scenario identifier.
    pub id: String,
    /// Human-readable scenario description.
    pub description: String,
    /// Domain category (e.g., "customer-service", "data-entry", "scheduling").
    pub domain: String,
    /// Initial system state and context provided to the agent.
    pub initial_context: String,
    /// User request that initiates the task.
    pub user_request: String,
    /// Available actions the agent can take in this scenario.
    pub available_actions: Vec<Tau2Action>,
    /// Expected sequence of actions for a correct solution (ground truth).
    pub expected_actions: Vec<Tau2ExpectedAction>,
    /// Success criteria for scoring the scenario.
    pub success_criteria: Tau2SuccessCriteria,
    /// Maximum number of turns allowed for this scenario.
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
}

fn default_max_turns() -> usize {
    20
}

/// An action available to the agent within a τ²-bench scenario.
///
/// Maps to a tool definition in ADK-Rust's tool system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2Action {
    /// Action name (maps to tool name in ADK-Rust).
    pub name: String,
    /// Human-readable description of what the action does.
    pub description: String,
    /// JSON Schema for the action parameters.
    pub parameters: serde_json::Value,
    /// Whether this action has side effects in the scenario environment.
    #[serde(default)]
    pub has_side_effects: bool,
}

/// An expected action in the ground-truth solution sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2ExpectedAction {
    /// The action name that should be called.
    pub action_name: String,
    /// Expected arguments (partial match — only specified keys are checked).
    pub expected_args: serde_json::Value,
    /// Whether order matters relative to adjacent expected actions.
    #[serde(default = "default_order_matters")]
    pub order_matters: bool,
}

fn default_order_matters() -> bool {
    true
}

/// Success criteria for scoring a τ²-bench scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2SuccessCriteria {
    /// Scoring mode: "exact" requires exact action match, "partial" allows
    /// partial credit for partially correct sequences.
    pub mode: ScoringMode,
    /// Minimum score (0.0–1.0) for the scenario to be considered "passed".
    #[serde(default = "default_pass_threshold")]
    pub pass_threshold: f64,
    /// Whether the final response text must contain specific keywords.
    #[serde(default)]
    pub required_keywords: Vec<String>,
}

fn default_pass_threshold() -> f64 {
    0.5
}

/// Scoring mode for τ²-bench evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScoringMode {
    /// Requires exact match of action sequence.
    Exact,
    /// Awards partial credit for partially correct sequences.
    Partial,
}

/// A response from the simulated τ²-bench environment after an action.
///
/// In a full integration, this would come from the τ²-bench environment
/// simulator. For now, it represents the expected response structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2Response {
    /// Whether the action was successfully executed in the environment.
    pub success: bool,
    /// Response data from the environment (action-specific).
    pub data: serde_json::Value,
    /// Human-readable message about the action result.
    pub message: String,
    /// Whether the scenario is complete after this action.
    #[serde(default)]
    pub scenario_complete: bool,
}

/// τ²-bench standard output format for reporting scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2Report {
    /// Suite identifier.
    pub suite: String,
    /// Model used for execution.
    pub model: String,
    /// Total scenarios attempted.
    pub total_scenarios: usize,
    /// Scenarios that passed the success criteria.
    pub passed_scenarios: usize,
    /// Overall accuracy (passed / total).
    pub accuracy: f64,
    /// Per-scenario results.
    pub scenarios: Vec<Tau2ScenarioResult>,
}

/// Result for a single τ²-bench scenario execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tau2ScenarioResult {
    /// Scenario identifier.
    pub scenario_id: String,
    /// Whether the scenario passed.
    pub passed: bool,
    /// Computed score (0.0–1.0).
    pub score: f64,
    /// Number of actions taken by the agent.
    pub actions_taken: usize,
    /// Number of turns used.
    pub turns_used: usize,
    /// Optional failure reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Adapter Implementation
// ---------------------------------------------------------------------------

/// τ²-bench adapter that routes scenarios through the ADK-Rust runtime.
///
/// Translates between the τ²-bench protocol format and ADK-Rust's
/// Event/Content model, reporting task completion scores in the
/// τ²-bench standard output format.
pub struct Tau2Adapter {
    /// Adapter configuration.
    config: Tau2Config,
}

impl Tau2Adapter {
    /// Creates a new τ²-bench adapter with the given configuration.
    pub fn new(config: Tau2Config) -> Self {
        Self { config }
    }

    /// Creates a new τ²-bench adapter with default configuration.
    pub fn with_defaults() -> Self {
        Self { config: Tau2Config::default() }
    }

    /// Returns a reference to the adapter configuration.
    pub fn config(&self) -> &Tau2Config {
        &self.config
    }

    /// Loads τ²-bench scenarios from the configured dataset path.
    ///
    /// Reads JSON scenario files from the dataset directory and
    /// deserializes them into `Tau2Scenario` instances.
    async fn load_scenarios(&self) -> crate::Result<Vec<Tau2Scenario>> {
        let dataset_path = &self.config.dataset_path;

        if !dataset_path.exists() {
            return Err(crate::BenchError::WorkloadNotFound {
                path: dataset_path.display().to_string(),
            });
        }

        info!(path = %dataset_path.display(), "loading τ²-bench scenarios");

        let scenarios = load_scenarios_from_path(dataset_path).await?;

        let scenarios = match self.config.max_scenarios {
            Some(max) => scenarios.into_iter().take(max).collect(),
            None => scenarios,
        };

        info!(count = scenarios.len(), "loaded τ²-bench scenarios");
        Ok(scenarios)
    }

    /// Executes a single τ²-bench scenario through the ADK-Rust runtime.
    ///
    /// This translates the scenario into an agent session, routes it
    /// through `adk-runner`, and scores the result.
    async fn execute_scenario(
        &self,
        scenario: &Tau2Scenario,
        model: &str,
    ) -> crate::Result<Tau2ScenarioResult> {
        debug!(
            scenario_id = %scenario.id,
            domain = %scenario.domain,
            "executing τ²-bench scenario"
        );

        // TODO: Full implementation would:
        // 1. Create an LlmAgent with tools derived from scenario.available_actions
        // 2. Configure the agent with scenario.initial_context as system instructions
        // 3. Execute via Runner with scenario.user_request as the user message
        // 4. Track actions taken during execution
        // 5. Compare against expected_actions for scoring
        //
        // For now, we implement the structural scoring logic with placeholder
        // execution. The actual LLM execution path requires wiring up:
        //   - adk_runner::Runner
        //   - adk_model provider (selected by `model` parameter)
        //   - Tool implementations derived from Tau2Action definitions

        let agent_actions = self.run_agent_session(scenario, model).await?;

        let score = self.score_scenario(scenario, &agent_actions);
        let passed = score >= scenario.success_criteria.pass_threshold;

        let failure_reason = if !passed {
            Some(format!(
                "Score {score:.2} below threshold {:.2}",
                scenario.success_criteria.pass_threshold
            ))
        } else {
            None
        };

        Ok(Tau2ScenarioResult {
            scenario_id: scenario.id.clone(),
            passed,
            score,
            actions_taken: agent_actions.len(),
            turns_used: agent_actions.len(),
            failure_reason,
        })
    }

    /// Runs an agent session for the given scenario.
    ///
    /// TODO: Wire to `adk-runner` with a real LLM. Currently returns an
    /// empty action list as a placeholder.
    async fn run_agent_session(
        &self,
        scenario: &Tau2Scenario,
        model: &str,
    ) -> crate::Result<Vec<AgentAction>> {
        // TODO: Real implementation steps:
        // 1. Build tool definitions from scenario.available_actions
        //    - Each Tau2Action becomes an FunctionTool with matching schema
        // 2. Create LlmAgent with:
        //    - System instructions from scenario.initial_context
        //    - Tools from step 1
        //    - Model selected by `model` parameter via adk-model
        // 3. Create a Runner and execute with scenario.user_request
        // 4. Collect tool call events from the event stream
        // 5. For each tool call, simulate environment response (Tau2Response)
        // 6. Continue until scenario_complete or max_turns reached

        debug!(
            model = model,
            scenario_id = %scenario.id,
            max_turns = self.config.max_turns_per_scenario,
            "agent session placeholder — real LLM execution not yet wired"
        );

        // Placeholder: return empty actions (scenario will score 0.0)
        Ok(Vec::new())
    }

    /// Scores a scenario execution against the expected actions.
    ///
    /// Implements τ²-bench scoring logic:
    /// - Exact mode: full credit only if action sequence matches exactly
    /// - Partial mode: credit proportional to correct actions
    fn score_scenario(&self, scenario: &Tau2Scenario, agent_actions: &[AgentAction]) -> f64 {
        if scenario.expected_actions.is_empty() {
            // No expected actions defined — check keywords only
            return if scenario.success_criteria.required_keywords.is_empty() { 1.0 } else { 0.0 };
        }

        match scenario.success_criteria.mode {
            ScoringMode::Exact => self.score_exact(scenario, agent_actions),
            ScoringMode::Partial => self.score_partial(scenario, agent_actions),
        }
    }

    /// Exact scoring: 1.0 if action sequence matches, 0.0 otherwise.
    fn score_exact(&self, scenario: &Tau2Scenario, agent_actions: &[AgentAction]) -> f64 {
        let expected = &scenario.expected_actions;

        if agent_actions.len() != expected.len() {
            return 0.0;
        }

        for (agent_action, expected_action) in agent_actions.iter().zip(expected.iter()) {
            if agent_action.name != expected_action.action_name {
                return 0.0;
            }
            if !partial_json_match(&expected_action.expected_args, &agent_action.arguments) {
                return 0.0;
            }
        }

        1.0
    }

    /// Partial scoring: proportional credit for correct actions.
    fn score_partial(&self, scenario: &Tau2Scenario, agent_actions: &[AgentAction]) -> f64 {
        let expected = &scenario.expected_actions;

        if expected.is_empty() {
            return 1.0;
        }

        let mut correct_count = 0usize;

        for expected_action in expected {
            let matched = agent_actions.iter().any(|a| {
                a.name == expected_action.action_name
                    && partial_json_match(&expected_action.expected_args, &a.arguments)
            });
            if matched {
                correct_count += 1;
            }
        }

        correct_count as f64 / expected.len() as f64
    }

    /// Generates the τ²-bench standard output report.
    pub fn generate_report(&self, model: &str, results: &[Tau2ScenarioResult]) -> Tau2Report {
        let total_scenarios = results.len();
        let passed_scenarios = results.iter().filter(|r| r.passed).count();
        let accuracy = if total_scenarios > 0 {
            passed_scenarios as f64 / total_scenarios as f64
        } else {
            0.0
        };

        Tau2Report {
            suite: "tau2-bench".to_string(),
            model: model.to_string(),
            total_scenarios,
            passed_scenarios,
            accuracy,
            scenarios: results.to_vec(),
        }
    }
}

impl Default for Tau2Adapter {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[async_trait]
impl TaskQualityAdapter for Tau2Adapter {
    fn name(&self) -> &str {
        "tau2"
    }

    async fn run(&self, model: &str) -> crate::Result<TaskQualityResult> {
        info!(model = model, "starting τ²-bench task quality evaluation");

        let scenarios = self.load_scenarios().await?;

        if scenarios.is_empty() {
            warn!("no τ²-bench scenarios found — returning empty result");
            return Ok(TaskQualityResult {
                adapter_name: self.name().to_string(),
                model: model.to_string(),
                total_cases: 0,
                passed_cases: 0,
                accuracy: 0.0,
                cases: Vec::new(),
            });
        }

        let mut scenario_results = Vec::with_capacity(scenarios.len());

        for scenario in &scenarios {
            match self.execute_scenario(scenario, model).await {
                Ok(result) => scenario_results.push(result),
                Err(e) => {
                    warn!(
                        scenario_id = %scenario.id,
                        error = %e,
                        "scenario execution failed — marking as failed"
                    );
                    scenario_results.push(Tau2ScenarioResult {
                        scenario_id: scenario.id.clone(),
                        passed: false,
                        score: 0.0,
                        actions_taken: 0,
                        turns_used: 0,
                        failure_reason: Some(format!("Execution error: {e}")),
                    });
                }
            }
        }

        // Generate the τ²-bench standard report
        let report = self.generate_report(model, &scenario_results);

        debug!(
            accuracy = report.accuracy,
            passed = report.passed_scenarios,
            total = report.total_scenarios,
            "τ²-bench evaluation complete"
        );

        // Convert to the generic TaskQualityResult format
        let cases = scenario_results
            .iter()
            .map(|r| CaseResult {
                case_id: r.scenario_id.clone(),
                passed: r.passed,
                score: r.score,
                details: r.failure_reason.clone(),
            })
            .collect();

        let total_cases = scenario_results.len();
        let passed_cases = scenario_results.iter().filter(|r| r.passed).count();
        let accuracy = if total_cases > 0 { passed_cases as f64 / total_cases as f64 } else { 0.0 };

        Ok(TaskQualityResult {
            adapter_name: self.name().to_string(),
            model: model.to_string(),
            total_cases,
            passed_cases,
            accuracy,
            cases,
        })
    }
}

// ---------------------------------------------------------------------------
// Internal Types
// ---------------------------------------------------------------------------

/// An action taken by the agent during scenario execution.
///
/// Corresponds to a tool call captured from the agent's event stream.
#[derive(Debug, Clone)]
struct AgentAction {
    /// Tool/action name that was called.
    name: String,
    /// Arguments passed to the tool.
    arguments: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Helper Functions
// ---------------------------------------------------------------------------

/// Loads τ²-bench scenario files from a directory.
///
/// Reads all `.json` files in the directory and attempts to deserialize
/// them as `Tau2Scenario` instances.
async fn load_scenarios_from_path(path: &Path) -> crate::Result<Vec<Tau2Scenario>> {
    let mut scenarios = Vec::new();

    if path.is_file() {
        // Single file — load as one scenario
        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            crate::BenchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read scenario file {}: {e}", path.display()),
            ))
        })?;

        let scenario: Tau2Scenario =
            serde_json::from_str(&content).map_err(|e| crate::BenchError::WorkloadValidation {
                field: "scenario".to_string(),
                reason: format!("failed to parse τ²-bench scenario {}: {e}", path.display()),
            })?;

        scenarios.push(scenario);
    } else if path.is_dir() {
        // Directory — load all JSON files
        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            if entry_path.extension().and_then(|e| e.to_str()) == Some("json") {
                let content = tokio::fs::read_to_string(&entry_path).await?;

                match serde_json::from_str::<Tau2Scenario>(&content) {
                    Ok(scenario) => scenarios.push(scenario),
                    Err(e) => {
                        warn!(
                            path = %entry_path.display(),
                            error = %e,
                            "skipping invalid τ²-bench scenario file"
                        );
                    }
                }
            }
        }

        // Sort by ID for deterministic ordering
        scenarios.sort_by(|a, b| a.id.cmp(&b.id));
    }

    Ok(scenarios)
}

/// Performs a partial JSON match: checks that all keys in `expected`
/// are present in `actual` with matching values.
///
/// This allows the agent to provide additional fields beyond what is
/// expected without penalizing the score.
fn partial_json_match(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    match (expected, actual) {
        (serde_json::Value::Object(exp_map), serde_json::Value::Object(act_map)) => {
            for (key, exp_value) in exp_map {
                match act_map.get(key) {
                    Some(act_value) => {
                        if !partial_json_match(exp_value, act_value) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            true
        }
        (serde_json::Value::Array(exp_arr), serde_json::Value::Array(act_arr)) => {
            if exp_arr.len() != act_arr.len() {
                return false;
            }
            exp_arr.iter().zip(act_arr.iter()).all(|(e, a)| partial_json_match(e, a))
        }
        _ => expected == actual,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tau2_config_builder() {
        let config = Tau2Config::builder()
            .dataset_path("/tmp/tau2-scenarios")
            .max_scenarios(10)
            .max_turns_per_scenario(15)
            .scenario_timeout_secs(60)
            .verbose_scoring(true)
            .build();

        assert_eq!(config.dataset_path, PathBuf::from("/tmp/tau2-scenarios"));
        assert_eq!(config.max_scenarios, Some(10));
        assert_eq!(config.max_turns_per_scenario, 15);
        assert_eq!(config.scenario_timeout_secs, 60);
        assert!(config.verbose_scoring);
    }

    #[test]
    fn test_tau2_config_defaults() {
        let config = Tau2Config::default();
        assert_eq!(config.dataset_path, PathBuf::from("./tau2-bench/scenarios"));
        assert_eq!(config.max_scenarios, None);
        assert_eq!(config.max_turns_per_scenario, 20);
        assert_eq!(config.scenario_timeout_secs, 120);
        assert!(!config.verbose_scoring);
    }

    #[test]
    fn test_adapter_name() {
        let adapter = Tau2Adapter::with_defaults();
        assert_eq!(adapter.name(), "tau2");
    }

    #[test]
    fn test_partial_json_match_exact() {
        let expected = serde_json::json!({"key": "value"});
        let actual = serde_json::json!({"key": "value", "extra": 42});
        assert!(partial_json_match(&expected, &actual));
    }

    #[test]
    fn test_partial_json_match_missing_key() {
        let expected = serde_json::json!({"key": "value", "required": true});
        let actual = serde_json::json!({"key": "value"});
        assert!(!partial_json_match(&expected, &actual));
    }

    #[test]
    fn test_partial_json_match_nested() {
        let expected = serde_json::json!({"nested": {"inner": "val"}});
        let actual = serde_json::json!({"nested": {"inner": "val", "extra": 1}, "top": true});
        assert!(partial_json_match(&expected, &actual));
    }

    #[test]
    fn test_partial_json_match_array() {
        let expected = serde_json::json!([1, 2, 3]);
        let actual = serde_json::json!([1, 2, 3]);
        assert!(partial_json_match(&expected, &actual));

        let actual_diff = serde_json::json!([1, 2, 4]);
        assert!(!partial_json_match(&expected, &actual_diff));
    }

    #[test]
    fn test_scoring_exact_match() {
        let adapter = Tau2Adapter::with_defaults();
        let scenario = make_test_scenario(ScoringMode::Exact);

        let actions = vec![
            AgentAction {
                name: "lookup_customer".to_string(),
                arguments: serde_json::json!({"customer_id": "C123"}),
            },
            AgentAction {
                name: "update_record".to_string(),
                arguments: serde_json::json!({"field": "email", "value": "new@example.com"}),
            },
        ];

        let score = adapter.score_scenario(&scenario, &actions);
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_scoring_exact_wrong_order() {
        let adapter = Tau2Adapter::with_defaults();
        let scenario = make_test_scenario(ScoringMode::Exact);

        let actions = vec![
            AgentAction {
                name: "update_record".to_string(),
                arguments: serde_json::json!({"field": "email", "value": "new@example.com"}),
            },
            AgentAction {
                name: "lookup_customer".to_string(),
                arguments: serde_json::json!({"customer_id": "C123"}),
            },
        ];

        let score = adapter.score_scenario(&scenario, &actions);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_scoring_partial_credit() {
        let adapter = Tau2Adapter::with_defaults();
        let scenario = make_test_scenario(ScoringMode::Partial);

        // Only first action matches
        let actions = vec![AgentAction {
            name: "lookup_customer".to_string(),
            arguments: serde_json::json!({"customer_id": "C123"}),
        }];

        let score = adapter.score_scenario(&scenario, &actions);
        assert_eq!(score, 0.5); // 1 of 2 expected actions matched
    }

    #[test]
    fn test_scoring_empty_actions() {
        let adapter = Tau2Adapter::with_defaults();
        let scenario = make_test_scenario(ScoringMode::Partial);

        let actions: Vec<AgentAction> = Vec::new();
        let score = adapter.score_scenario(&scenario, &actions);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_generate_report() {
        let adapter = Tau2Adapter::with_defaults();
        let results = vec![
            Tau2ScenarioResult {
                scenario_id: "s1".to_string(),
                passed: true,
                score: 1.0,
                actions_taken: 2,
                turns_used: 2,
                failure_reason: None,
            },
            Tau2ScenarioResult {
                scenario_id: "s2".to_string(),
                passed: false,
                score: 0.3,
                actions_taken: 1,
                turns_used: 3,
                failure_reason: Some("Score 0.30 below threshold 0.50".to_string()),
            },
        ];

        let report = adapter.generate_report("gemini-2.5-flash", &results);
        assert_eq!(report.suite, "tau2-bench");
        assert_eq!(report.model, "gemini-2.5-flash");
        assert_eq!(report.total_scenarios, 2);
        assert_eq!(report.passed_scenarios, 1);
        assert_eq!(report.accuracy, 0.5);
    }

    #[test]
    fn test_scenario_serialization_roundtrip() {
        let scenario = make_test_scenario(ScoringMode::Partial);
        let json = serde_json::to_string(&scenario).unwrap();
        let deserialized: Tau2Scenario = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, scenario.id);
        assert_eq!(deserialized.domain, scenario.domain);
        assert_eq!(deserialized.available_actions.len(), 2);
    }

    /// Helper to create a test scenario for scoring tests.
    fn make_test_scenario(mode: ScoringMode) -> Tau2Scenario {
        Tau2Scenario {
            id: "test-scenario-1".to_string(),
            description: "Test customer service scenario".to_string(),
            domain: "customer-service".to_string(),
            initial_context: "You are a customer service agent.".to_string(),
            user_request: "Update my email address.".to_string(),
            available_actions: vec![
                Tau2Action {
                    name: "lookup_customer".to_string(),
                    description: "Look up customer by ID".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "customer_id": {"type": "string"}
                        },
                        "required": ["customer_id"]
                    }),
                    has_side_effects: false,
                },
                Tau2Action {
                    name: "update_record".to_string(),
                    description: "Update a customer record field".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "field": {"type": "string"},
                            "value": {"type": "string"}
                        },
                        "required": ["field", "value"]
                    }),
                    has_side_effects: true,
                },
            ],
            expected_actions: vec![
                Tau2ExpectedAction {
                    action_name: "lookup_customer".to_string(),
                    expected_args: serde_json::json!({"customer_id": "C123"}),
                    order_matters: true,
                },
                Tau2ExpectedAction {
                    action_name: "update_record".to_string(),
                    expected_args: serde_json::json!({"field": "email", "value": "new@example.com"}),
                    order_matters: true,
                },
            ],
            success_criteria: Tau2SuccessCriteria {
                mode,
                pass_threshold: 0.5,
                required_keywords: Vec::new(),
            },
            max_turns: 10,
        }
    }
}
