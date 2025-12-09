//! Core evaluator implementation
//!
//! The Evaluator orchestrates test execution and applies evaluation criteria.

use crate::criteria::EvaluationCriteria;
use crate::error::Result;
use crate::llm_judge::LlmJudge;
use crate::report::{EvaluationReport, EvaluationResult, Failure, TurnResult};
use crate::schema::{EvalCase, TestFile, ToolUse, Turn};
use crate::scoring::{ResponseScorer, ToolTrajectoryScorer};

use adk_core::{Agent, Content, Event, Llm};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Configuration for the evaluator
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluationConfig {
    /// Evaluation criteria to apply
    #[serde(default)]
    pub criteria: EvaluationCriteria,
    /// Whether to continue on failure
    #[serde(default)]
    pub continue_on_failure: bool,
    /// Maximum time per test case
    #[serde(default)]
    pub timeout_per_case: Option<Duration>,
    /// Number of retries for flaky tests
    #[serde(default)]
    pub retries: usize,
    /// Whether to collect detailed turn results
    #[serde(default = "default_true")]
    pub collect_turn_details: bool,
}

fn default_true() -> bool {
    true
}

impl EvaluationConfig {
    /// Create config with specific criteria
    pub fn with_criteria(criteria: EvaluationCriteria) -> Self {
        Self { criteria, ..Default::default() }
    }
}

/// The main evaluator struct
pub struct Evaluator {
    config: EvaluationConfig,
    tool_scorer: ToolTrajectoryScorer,
    response_scorer: ResponseScorer,
    llm_judge: Option<LlmJudge>,
}

impl Evaluator {
    /// Create a new evaluator with default configuration
    pub fn new(config: EvaluationConfig) -> Self {
        let tool_scorer = if let Some(tc) = &config.criteria.tool_trajectory_config {
            ToolTrajectoryScorer::with_config(tc.clone())
        } else {
            ToolTrajectoryScorer::new()
        };

        let response_scorer = if let Some(rc) = &config.criteria.response_match_config {
            ResponseScorer::with_config(rc.clone())
        } else {
            ResponseScorer::new()
        };

        Self { config, tool_scorer, response_scorer, llm_judge: None }
    }

    /// Create an evaluator with an LLM judge for semantic matching and rubric evaluation
    pub fn with_llm_judge(config: EvaluationConfig, judge_model: Arc<dyn Llm>) -> Self {
        let tool_scorer = if let Some(tc) = &config.criteria.tool_trajectory_config {
            ToolTrajectoryScorer::with_config(tc.clone())
        } else {
            ToolTrajectoryScorer::new()
        };

        let response_scorer = if let Some(rc) = &config.criteria.response_match_config {
            ResponseScorer::with_config(rc.clone())
        } else {
            ResponseScorer::new()
        };

        Self { config, tool_scorer, response_scorer, llm_judge: Some(LlmJudge::new(judge_model)) }
    }

    /// Set the LLM judge model
    pub fn set_llm_judge(&mut self, judge_model: Arc<dyn Llm>) {
        self.llm_judge = Some(LlmJudge::new(judge_model));
    }

    /// Check if LLM judge is available
    pub fn has_llm_judge(&self) -> bool {
        self.llm_judge.is_some()
    }

    /// Evaluate a test file against an agent
    pub async fn evaluate_file(
        &self,
        agent: Arc<dyn Agent>,
        path: impl AsRef<Path>,
    ) -> Result<EvaluationReport> {
        let test_file = TestFile::load(path)?;
        self.evaluate_test_file(agent, &test_file).await
    }

    /// Evaluate a TestFile struct
    pub async fn evaluate_test_file(
        &self,
        agent: Arc<dyn Agent>,
        test_file: &TestFile,
    ) -> Result<EvaluationReport> {
        let started_at = chrono::Utc::now();
        let run_id = format!("{}_{}", test_file.eval_set_id, uuid::Uuid::new_v4());
        let mut results = Vec::new();

        for eval_case in &test_file.eval_cases {
            let result = self.evaluate_case(agent.clone(), eval_case).await;

            match result {
                Ok(r) => {
                    let passed = r.passed;
                    results.push(r);
                    if !passed && !self.config.continue_on_failure {
                        break;
                    }
                }
                Err(e) => {
                    // Create a failed result for the error
                    results.push(EvaluationResult::failed(
                        &eval_case.eval_id,
                        HashMap::new(),
                        vec![Failure::new(
                            "execution",
                            Value::Null,
                            Value::String(e.to_string()),
                            0.0,
                            1.0,
                        )],
                        Duration::from_secs(0),
                    ));
                    if !self.config.continue_on_failure {
                        break;
                    }
                }
            }
        }

        Ok(EvaluationReport::new(&run_id, results, started_at))
    }

    /// Evaluate a single test case
    pub async fn evaluate_case(
        &self,
        agent: Arc<dyn Agent>,
        eval_case: &EvalCase,
    ) -> Result<EvaluationResult> {
        let start = Instant::now();
        let mut all_scores: HashMap<String, f64> = HashMap::new();
        let mut all_failures: Vec<Failure> = Vec::new();
        let mut turn_results: Vec<TurnResult> = Vec::new();

        // Execute each turn in the conversation
        for turn in &eval_case.conversation {
            let turn_result = self.execute_turn(agent.clone(), turn).await?;

            // Score this turn
            let (scores, failures) = self.score_turn(turn, &turn_result).await;

            // Merge scores
            for (criterion, score) in &scores {
                all_scores
                    .entry(criterion.clone())
                    .and_modify(|s| *s = (*s + score) / 2.0)
                    .or_insert(*score);
            }
            all_failures.extend(failures);

            if self.config.collect_turn_details {
                turn_results.push(turn_result);
            }
        }

        let duration = start.elapsed();
        let passed = all_failures.is_empty();

        let mut result = if passed {
            EvaluationResult::passed(&eval_case.eval_id, all_scores, duration)
        } else {
            EvaluationResult::failed(&eval_case.eval_id, all_scores, all_failures, duration)
        };

        if self.config.collect_turn_details {
            result = result.with_turn_results(turn_results);
        }

        Ok(result)
    }

    /// Execute a single turn and collect results
    async fn execute_turn(&self, agent: Arc<dyn Agent>, turn: &Turn) -> Result<TurnResult> {
        // Create input content
        let input_content = turn.user_content.to_adk_content();

        // Run the agent
        let events = self.run_agent(agent, input_content).await?;

        // Extract response and tool calls from events
        let (actual_response, actual_tool_calls) = self.extract_from_events(&events);

        // Get expected values
        let expected_response = turn.final_response.as_ref().map(|c| c.get_text());
        let expected_tool_calls =
            turn.intermediate_data.as_ref().map(|d| d.tool_uses.clone()).unwrap_or_default();

        Ok(TurnResult {
            invocation_id: turn.invocation_id.clone(),
            actual_response,
            expected_response,
            actual_tool_calls,
            expected_tool_calls,
            scores: HashMap::new(),
        })
    }

    /// Run agent and collect events
    async fn run_agent(&self, agent: Arc<dyn Agent>, input: Content) -> Result<Vec<Event>> {
        // Create a minimal invocation context for evaluation
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let ctx = Arc::new(EvalInvocationContext::new(invocation_id, input, agent.clone()));

        // Run the agent and collect all events
        let stream = agent.run(ctx).await.map_err(|e| {
            crate::error::EvalError::ExecutionError(format!("Agent run failed: {}", e))
        })?;

        // Collect all events from the stream
        let events: Vec<Event> = stream.filter_map(|r| async { r.ok() }).collect().await;

        Ok(events)
    }

    /// Extract response text and tool calls from events
    fn extract_from_events(&self, events: &[Event]) -> (Option<String>, Vec<ToolUse>) {
        let mut response_text = String::new();
        let mut tool_calls = Vec::new();

        for event in events {
            // Extract text content
            if let Some(content) = event.content() {
                for part in &content.parts {
                    // Extract text content
                    if let Some(text) = part.text() {
                        response_text.push_str(text);
                    }
                    // Extract function calls using pattern matching
                    if let adk_core::Part::FunctionCall { name, args, .. } = part {
                        tool_calls.push(ToolUse {
                            name: name.clone(),
                            args: args.clone(),
                            expected_response: None,
                        });
                    }
                }
            }
        }

        let response = if response_text.is_empty() { None } else { Some(response_text) };

        (response, tool_calls)
    }

    /// Score a turn against criteria
    async fn score_turn(
        &self,
        turn: &Turn,
        result: &TurnResult,
    ) -> (HashMap<String, f64>, Vec<Failure>) {
        let mut scores = HashMap::new();
        let mut failures = Vec::new();

        // Tool trajectory scoring
        if let Some(threshold) = self.config.criteria.tool_trajectory_score {
            let score =
                self.tool_scorer.score(&result.expected_tool_calls, &result.actual_tool_calls);
            scores.insert("tool_trajectory".to_string(), score);

            if score < threshold {
                failures.push(
                    Failure::new(
                        "tool_trajectory",
                        serde_json::to_value(&result.expected_tool_calls).unwrap_or_default(),
                        serde_json::to_value(&result.actual_tool_calls).unwrap_or_default(),
                        score,
                        threshold,
                    )
                    .with_details(&format!(
                        "Expected {} tool calls, got {}",
                        result.expected_tool_calls.len(),
                        result.actual_tool_calls.len()
                    )),
                );
            }
        }

        // Response similarity scoring (text-based)
        if let Some(threshold) = self.config.criteria.response_similarity {
            if let (Some(expected), Some(actual)) =
                (&result.expected_response, &result.actual_response)
            {
                let score = self.response_scorer.score(expected, actual);
                scores.insert("response_similarity".to_string(), score);

                if score < threshold {
                    failures.push(
                        Failure::new(
                            "response_similarity",
                            Value::String(expected.clone()),
                            Value::String(actual.clone()),
                            score,
                            threshold,
                        )
                        .with_details("Response text differs from expected"),
                    );
                }
            } else if result.expected_response.is_some() && result.actual_response.is_none() {
                scores.insert("response_similarity".to_string(), 0.0);
                failures.push(
                    Failure::new(
                        "response_similarity",
                        Value::String(result.expected_response.clone().unwrap_or_default()),
                        Value::Null,
                        0.0,
                        threshold,
                    )
                    .with_details("No response received"),
                );
            }
        }

        // LLM-judged semantic matching
        if let Some(threshold) = self.config.criteria.semantic_match_score {
            if let Some(judge) = &self.llm_judge {
                if let (Some(expected), Some(actual)) =
                    (&result.expected_response, &result.actual_response)
                {
                    match judge
                        .semantic_match(
                            expected,
                            actual,
                            self.config.criteria.semantic_match_config.as_ref(),
                        )
                        .await
                    {
                        Ok(semantic_result) => {
                            scores.insert("semantic_match".to_string(), semantic_result.score);
                            if semantic_result.score < threshold {
                                failures.push(
                                    Failure::new(
                                        "semantic_match",
                                        Value::String(expected.clone()),
                                        Value::String(actual.clone()),
                                        semantic_result.score,
                                        threshold,
                                    )
                                    .with_details(&semantic_result.reasoning),
                                );
                            }
                        }
                        Err(e) => {
                            // Record error but don't fail the whole evaluation
                            failures.push(
                                Failure::new(
                                    "semantic_match",
                                    Value::String(expected.clone()),
                                    Value::String(actual.clone()),
                                    0.0,
                                    threshold,
                                )
                                .with_details(&format!("LLM judge error: {}", e)),
                            );
                        }
                    }
                }
            }
        }

        // Rubric-based evaluation
        if let Some(threshold) = self.config.criteria.rubric_quality_score {
            if let Some(judge) = &self.llm_judge {
                if let Some(rubric_config) = &self.config.criteria.rubric_config {
                    if let Some(actual) = &result.actual_response {
                        // Use user input as context for rubric evaluation
                        let context = turn.user_content.get_text();
                        match judge.evaluate_rubrics(actual, &context, rubric_config).await {
                            Ok(rubric_result) => {
                                scores.insert(
                                    "rubric_quality".to_string(),
                                    rubric_result.overall_score,
                                );
                                // Also store individual rubric scores
                                for rs in &rubric_result.rubric_scores {
                                    scores.insert(format!("rubric_{}", rs.name), rs.score);
                                }
                                if rubric_result.overall_score < threshold {
                                    let details = rubric_result
                                        .rubric_scores
                                        .iter()
                                        .map(|rs| {
                                            format!(
                                                "{}: {:.2} - {}",
                                                rs.name, rs.score, rs.reasoning
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join("; ");
                                    failures.push(
                                        Failure::new(
                                            "rubric_quality",
                                            Value::Number(
                                                serde_json::Number::from_f64(threshold)
                                                    .unwrap_or(serde_json::Number::from(0)),
                                            ),
                                            Value::Number(
                                                serde_json::Number::from_f64(
                                                    rubric_result.overall_score,
                                                )
                                                .unwrap_or(serde_json::Number::from(0)),
                                            ),
                                            rubric_result.overall_score,
                                            threshold,
                                        )
                                        .with_details(&details),
                                    );
                                }
                            }
                            Err(e) => {
                                failures.push(
                                    Failure::new(
                                        "rubric_quality",
                                        Value::Null,
                                        Value::Null,
                                        0.0,
                                        threshold,
                                    )
                                    .with_details(&format!("LLM judge error: {}", e)),
                                );
                            }
                        }
                    }
                }
            }
        }

        // Safety evaluation
        if let Some(threshold) = self.config.criteria.safety_score {
            if let Some(judge) = &self.llm_judge {
                if let Some(actual) = &result.actual_response {
                    match judge.evaluate_safety(actual).await {
                        Ok(safety_result) => {
                            scores.insert("safety".to_string(), safety_result.score);
                            if safety_result.score < threshold {
                                failures.push(
                                    Failure::new(
                                        "safety",
                                        Value::Number(
                                            serde_json::Number::from_f64(threshold)
                                                .unwrap_or(serde_json::Number::from(0)),
                                        ),
                                        Value::Number(
                                            serde_json::Number::from_f64(safety_result.score)
                                                .unwrap_or(serde_json::Number::from(0)),
                                        ),
                                        safety_result.score,
                                        threshold,
                                    )
                                    .with_details(&format!(
                                        "Safety issues: {}",
                                        safety_result.issues.join(", ")
                                    )),
                                );
                            }
                        }
                        Err(e) => {
                            failures.push(
                                Failure::new("safety", Value::Null, Value::Null, 0.0, threshold)
                                    .with_details(&format!("LLM judge error: {}", e)),
                            );
                        }
                    }
                }
            }
        }

        // Hallucination detection
        if let Some(threshold) = self.config.criteria.hallucination_score {
            if let Some(judge) = &self.llm_judge {
                if let Some(actual) = &result.actual_response {
                    let context = turn.user_content.get_text();
                    let ground_truth = result.expected_response.as_deref();
                    match judge.detect_hallucinations(actual, &context, ground_truth).await {
                        Ok(hallucination_result) => {
                            scores.insert("hallucination".to_string(), hallucination_result.score);
                            if hallucination_result.score < threshold {
                                failures.push(
                                    Failure::new(
                                        "hallucination",
                                        Value::Number(
                                            serde_json::Number::from_f64(threshold)
                                                .unwrap_or(serde_json::Number::from(0)),
                                        ),
                                        Value::Number(
                                            serde_json::Number::from_f64(
                                                hallucination_result.score,
                                            )
                                            .unwrap_or(serde_json::Number::from(0)),
                                        ),
                                        hallucination_result.score,
                                        threshold,
                                    )
                                    .with_details(&format!(
                                        "Hallucinations detected: {}",
                                        hallucination_result.issues.join(", ")
                                    )),
                                );
                            }
                        }
                        Err(e) => {
                            failures.push(
                                Failure::new(
                                    "hallucination",
                                    Value::Null,
                                    Value::Null,
                                    0.0,
                                    threshold,
                                )
                                .with_details(&format!("LLM judge error: {}", e)),
                            );
                        }
                    }
                }
            }
        }

        (scores, failures)
    }

    /// Evaluate multiple test cases in parallel
    pub async fn evaluate_cases_parallel(
        &self,
        agent: Arc<dyn Agent>,
        cases: &[EvalCase],
        concurrency: usize,
    ) -> Vec<Result<EvaluationResult>> {
        use futures::stream::{self, StreamExt};

        let results: Vec<_> = stream::iter(cases)
            .map(|case| {
                let agent = agent.clone();
                async move { self.evaluate_case(agent, case).await }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        results
    }

    /// Evaluate a directory of test files
    pub async fn evaluate_directory(
        &self,
        agent: Arc<dyn Agent>,
        dir: impl AsRef<Path>,
    ) -> Result<Vec<EvaluationReport>> {
        let mut reports = Vec::new();

        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.ends_with(".test.json") {
                        let report = self.evaluate_file(agent.clone(), &path).await?;
                        reports.push(report);
                    }
                }
            }
        }

        Ok(reports)
    }
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new(EvaluationConfig::default())
    }
}

// ============================================================================
// EvalInvocationContext - Minimal context for running agents during evaluation
// ============================================================================

/// Minimal InvocationContext implementation for evaluation
struct EvalInvocationContext {
    invocation_id: String,
    user_content: Content,
    agent: Arc<dyn Agent>,
    session: EvalSession,
    run_config: adk_core::RunConfig,
    ended: std::sync::atomic::AtomicBool,
}

impl EvalInvocationContext {
    fn new(invocation_id: String, user_content: Content, agent: Arc<dyn Agent>) -> Self {
        let session_id = format!("eval-session-{}", uuid::Uuid::new_v4());
        Self {
            invocation_id,
            user_content,
            agent,
            session: EvalSession::new(session_id),
            run_config: adk_core::RunConfig::default(),
            ended: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl adk_core::ReadonlyContext for EvalInvocationContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    fn agent_name(&self) -> &str {
        self.agent.name()
    }

    fn user_id(&self) -> &str {
        "eval_user"
    }

    fn app_name(&self) -> &str {
        "eval_app"
    }

    fn session_id(&self) -> &str {
        &self.session.id
    }

    fn branch(&self) -> &str {
        "main"
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for EvalInvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl adk_core::InvocationContext for EvalInvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }

    fn session(&self) -> &dyn adk_core::Session {
        &self.session
    }

    fn run_config(&self) -> &adk_core::RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Minimal Session implementation for evaluation
struct EvalSession {
    id: String,
    state: EvalState,
}

impl EvalSession {
    fn new(id: String) -> Self {
        Self { id, state: EvalState::new() }
    }
}

impl adk_core::Session for EvalSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        "eval_app"
    }

    fn user_id(&self) -> &str {
        "eval_user"
    }

    fn state(&self) -> &dyn adk_core::State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<Content> {
        vec![]
    }
}

/// Minimal State implementation for evaluation
struct EvalState {
    data: std::sync::RwLock<HashMap<String, serde_json::Value>>,
}

impl EvalState {
    fn new() -> Self {
        Self { data: std::sync::RwLock::new(HashMap::new()) }
    }
}

impl adk_core::State for EvalState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.data.read().ok()?.get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        if let Ok(mut data) = self.data.write() {
            data.insert(key, value);
        }
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.data.read().ok().map(|d| d.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluator_creation() {
        let config = EvaluationConfig::with_criteria(
            EvaluationCriteria::exact_tools().with_response_similarity(0.8),
        );
        let evaluator = Evaluator::new(config);
        assert!(evaluator.config.criteria.tool_trajectory_score.is_some());
        assert!(evaluator.config.criteria.response_similarity.is_some());
    }

    #[tokio::test]
    async fn test_turn_scoring() {
        let config = EvaluationConfig::with_criteria(EvaluationCriteria {
            tool_trajectory_score: Some(1.0),
            response_similarity: Some(0.8),
            ..Default::default()
        });
        let evaluator = Evaluator::new(config);

        let turn = Turn {
            invocation_id: "test".to_string(),
            user_content: crate::schema::ContentData::text("Hello"),
            final_response: Some(crate::schema::ContentData::model_response("Hi there!")),
            intermediate_data: Some(crate::schema::IntermediateData {
                tool_uses: vec![ToolUse::new("greet")],
                ..Default::default()
            }),
        };

        let result = TurnResult {
            invocation_id: "test".to_string(),
            actual_response: Some("Hi there!".to_string()),
            expected_response: Some("Hi there!".to_string()),
            actual_tool_calls: vec![ToolUse::new("greet")],
            expected_tool_calls: vec![ToolUse::new("greet")],
            scores: HashMap::new(),
        };

        let (scores, failures) = evaluator.score_turn(&turn, &result).await;
        assert!(failures.is_empty());
        assert_eq!(scores.get("tool_trajectory"), Some(&1.0));
        assert_eq!(scores.get("response_similarity"), Some(&1.0));
    }
}
