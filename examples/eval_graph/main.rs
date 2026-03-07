//! Graph Workflow Evaluation Example
//!
//! This example demonstrates how to evaluate graph-based workflows using adk-eval.
//! It shows how to test:
//! - Node execution order (trajectory)
//! - State transformations
//! - Conditional routing decisions
//! - Final output quality
//!
//! Run with: cargo run --example eval_graph
//!
//! Requires: GOOGLE_API_KEY environment variable for LLM-judged evaluation

use adk_core::Llm;
use adk_eval::criteria::{ResponseMatchConfig, RubricLevel, SimilarityAlgorithm};
use adk_eval::schema::ToolUse;
use adk_eval::{
    EvaluationConfig, EvaluationCriteria, Evaluator, LlmJudge, LlmJudgeConfig, ResponseScorer,
    Rubric, RubricConfig, ToolTrajectoryScorer,
};
use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

/// Represents a recorded node execution for evaluation
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct NodeExecution {
    node_name: String,
    input_state: State,
    output_state: State,
}

/// Records node executions for later evaluation
struct ExecutionRecorder {
    executions: std::sync::Mutex<Vec<NodeExecution>>,
}

impl ExecutionRecorder {
    fn new() -> Self {
        Self { executions: std::sync::Mutex::new(Vec::new()) }
    }

    fn record(&self, name: &str, input: &State, output: &State) {
        self.executions.lock().unwrap().push(NodeExecution {
            node_name: name.to_string(),
            input_state: input.clone(),
            output_state: output.clone(),
        });
    }

    fn get_trajectory(&self) -> Vec<String> {
        self.executions.lock().unwrap().iter().map(|e| e.node_name.clone()).collect()
    }

    fn clear(&self) {
        self.executions.lock().unwrap().clear();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Graph Workflow Evaluation ===\n");

    // -------------------------------------------------------------------------
    // 1. Build a graph workflow to evaluate
    // -------------------------------------------------------------------------
    println!("1. Building graph workflow...\n");

    let recorder = Arc::new(ExecutionRecorder::new());
    let recorder_classify = recorder.clone();
    let recorder_process = recorder.clone();
    let recorder_format = recorder.clone();

    // Build a simple classification -> processing -> formatting graph
    let graph = StateGraph::with_channels(&["input", "category", "processed", "output"])
        // Classify input
        .add_node_fn("classify", move |ctx| {
            let recorder = recorder_classify.clone();
            async move {
                let input_state = ctx.state.clone();
                let input = ctx.get("input").and_then(|v| v.as_str()).unwrap_or("");

                // Simple classification logic
                let category = if input.contains("error") || input.contains("bug") {
                    "error"
                } else if input.contains("feature") || input.contains("add") {
                    "feature"
                } else {
                    "general"
                };

                let output = NodeOutput::new().with_update("category", json!(category));

                // Record for evaluation
                let mut output_state = input_state.clone();
                output_state.insert("category".to_string(), json!(category));
                recorder.record("classify", &input_state, &output_state);

                Ok(output)
            }
        })
        // Process based on category
        .add_node_fn("process", move |ctx| {
            let recorder = recorder_process.clone();
            async move {
                let input_state = ctx.state.clone();
                let input = ctx.get("input").and_then(|v| v.as_str()).unwrap_or("");
                let category = ctx.get("category").and_then(|v| v.as_str()).unwrap_or("");

                let processed = match category {
                    "error" => format!("[BUG REPORT] {}", input.to_uppercase()),
                    "feature" => format!("[FEATURE REQUEST] {}", input),
                    _ => format!("[INFO] {}", input),
                };

                let output = NodeOutput::new().with_update("processed", json!(processed));

                let mut output_state = input_state.clone();
                output_state.insert("processed".to_string(), json!(processed));
                recorder.record("process", &input_state, &output_state);

                Ok(output)
            }
        })
        // Format final output
        .add_node_fn("format", move |ctx| {
            let recorder = recorder_format.clone();
            async move {
                let input_state = ctx.state.clone();
                let processed = ctx.get("processed").and_then(|v| v.as_str()).unwrap_or("");
                let category = ctx.get("category").and_then(|v| v.as_str()).unwrap_or("");

                let output = format!("Category: {} | {}", category, processed);

                let result = NodeOutput::new().with_update("output", json!(output));

                let mut output_state = input_state.clone();
                output_state.insert("output".to_string(), json!(output));
                recorder.record("format", &input_state, &output_state);

                Ok(result)
            }
        })
        .add_edge(START, "classify")
        .add_edge("classify", "process")
        .add_edge("process", "format")
        .add_edge("format", END)
        .compile()?;

    println!("   Graph: START -> classify -> process -> format -> END\n");

    // -------------------------------------------------------------------------
    // 2. Define test cases for the graph
    // -------------------------------------------------------------------------
    println!("2. Defining test cases...\n");

    let test_cases = vec![
        (
            "There's an error in the login page",
            vec!["classify", "process", "format"],
            "error",
            "[BUG REPORT]",
        ),
        (
            "Please add a dark mode feature",
            vec!["classify", "process", "format"],
            "feature",
            "[FEATURE REQUEST]",
        ),
        ("How do I reset my password?", vec!["classify", "process", "format"], "general", "[INFO]"),
    ];

    println!("   Test cases: {}", test_cases.len());
    for (input, _, category, _) in &test_cases {
        println!("   - \"{}\" -> expected category: {}", input, category);
    }
    println!();

    // -------------------------------------------------------------------------
    // 3. Run graph and evaluate trajectory
    // -------------------------------------------------------------------------
    println!("3. Evaluating node trajectory...\n");

    let trajectory_scorer = ToolTrajectoryScorer::new();

    for (i, (input, expected_trajectory, expected_category, expected_prefix)) in
        test_cases.iter().enumerate()
    {
        recorder.clear();

        // Run the graph
        let mut state = State::new();
        state.insert("input".to_string(), json!(input));

        let result = graph.invoke(state, ExecutionConfig::new(format!("test-{}", i))).await?;

        // Get actual trajectory
        let actual_trajectory = recorder.get_trajectory();

        // Convert to ToolUse for scoring
        let expected_tools: Vec<ToolUse> =
            expected_trajectory.iter().map(|name| ToolUse::new(name)).collect();
        let actual_tools: Vec<ToolUse> =
            actual_trajectory.iter().map(|name| ToolUse::new(name)).collect();

        let trajectory_score = trajectory_scorer.score(&expected_tools, &actual_tools);

        // Check category
        let actual_category = result.get("category").and_then(|v| v.as_str()).unwrap_or("");
        let category_match = actual_category == *expected_category;

        // Check output prefix
        let actual_output = result.get("output").and_then(|v| v.as_str()).unwrap_or("");
        let prefix_match = actual_output.contains(expected_prefix);

        let status =
            if trajectory_score >= 1.0 && category_match && prefix_match { "PASS" } else { "FAIL" };

        println!("   Test {}: \"{}\"", i + 1, input);
        println!(
            "      Trajectory: {:?} (score: {:.0}%)",
            actual_trajectory,
            trajectory_score * 100.0
        );
        println!(
            "      Category: {} (expected: {}) {}",
            actual_category,
            expected_category,
            if category_match { "✓" } else { "✗" }
        );
        println!(
            "      Output: {} {}",
            &actual_output[..actual_output.len().min(50)],
            if prefix_match { "✓" } else { "✗" }
        );
        println!("      Status: {}\n", status);
    }

    // -------------------------------------------------------------------------
    // 4. Evaluate output quality with response similarity
    // -------------------------------------------------------------------------
    println!("4. Evaluating output quality with similarity scoring...\n");

    let response_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Jaccard,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: false,
    });

    let quality_tests = vec![
        ("Fix the crash bug", "Category: error | [BUG REPORT] FIX THE CRASH BUG"),
        ("Add user profiles", "Category: feature | [FEATURE REQUEST] Add user profiles"),
    ];

    for (input, expected_output) in &quality_tests {
        let mut state = State::new();
        state.insert("input".to_string(), json!(input));

        let result = graph.invoke(state, ExecutionConfig::new("quality-test".to_string())).await?;

        let actual_output = result.get("output").and_then(|v| v.as_str()).unwrap_or("");
        let similarity = response_scorer.score(expected_output, actual_output);

        println!("   Input: \"{}\"", input);
        println!("   Expected: \"{}\"", expected_output);
        println!("   Actual:   \"{}\"", actual_output);
        println!("   Similarity: {:.0}%\n", similarity * 100.0);
    }

    // -------------------------------------------------------------------------
    // 5. LLM-judged evaluation (if API key available)
    // -------------------------------------------------------------------------
    println!("5. LLM-judged evaluation...\n");

    let _ = dotenvy::dotenv();
    if let Ok(api_key) = std::env::var("GOOGLE_API_KEY") {
        let judge_model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
        let judge = LlmJudge::with_config(
            judge_model.clone(),
            LlmJudgeConfig { max_tokens: 512, temperature: 0.0 },
        );

        // Define rubrics for graph output quality
        let rubrics = vec![
            Rubric::new("Classification", "Input was correctly classified")
                .with_weight(0.4)
                .with_levels(vec![
                    RubricLevel {
                        score: 1.0,
                        description: "Correct category assigned".to_string(),
                    },
                    RubricLevel { score: 0.0, description: "Wrong category".to_string() },
                ]),
            Rubric::new("Formatting", "Output follows expected format")
                .with_weight(0.3)
                .with_levels(vec![
                    RubricLevel {
                        score: 1.0,
                        description: "Proper format with category and tag".to_string(),
                    },
                    RubricLevel { score: 0.5, description: "Partial formatting".to_string() },
                    RubricLevel { score: 0.0, description: "No formatting".to_string() },
                ]),
            Rubric::new("Completeness", "All information preserved").with_weight(0.3).with_levels(
                vec![
                    RubricLevel { score: 1.0, description: "Original input preserved".to_string() },
                    RubricLevel { score: 0.5, description: "Partial information".to_string() },
                    RubricLevel { score: 0.0, description: "Information lost".to_string() },
                ],
            ),
        ];

        let rubric_config = RubricConfig { rubrics };

        // Test with LLM judge
        let test_input = "There's a critical error in the payment system";
        let mut state = State::new();
        state.insert("input".to_string(), json!(test_input));

        let result = graph.invoke(state, ExecutionConfig::new("llm-eval".to_string())).await?;

        let actual_output = result.get("output").and_then(|v| v.as_str()).unwrap_or("");
        let context = format!(
            "Input: '{}'\nExpected: Error classification with [BUG REPORT] tag",
            test_input
        );

        let rubric_result = judge.evaluate_rubrics(actual_output, &context, &rubric_config).await?;

        println!("   Input: \"{}\"", test_input);
        println!("   Output: \"{}\"", actual_output);
        println!("\n   Rubric Evaluation:");
        println!("      Overall Score: {:.0}%", rubric_result.overall_score * 100.0);
        for score in &rubric_result.rubric_scores {
            println!("      {}: {:.0}% - {}", score.name, score.score * 100.0, score.reasoning);
        }
    } else {
        println!("   Skipped (GOOGLE_API_KEY not set)");
    }

    // -------------------------------------------------------------------------
    // 6. Create production evaluator for graph workflows
    // -------------------------------------------------------------------------
    println!("\n6. Production evaluator setup...\n");

    let criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0), // Require exact node execution order
        response_similarity: Some(0.7),   // 70% output similarity
        ..Default::default()
    };

    let _evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));

    println!("   Evaluator configured for graph workflows:");
    println!("   - Node trajectory matching (100% required)");
    println!("   - Output similarity (70% threshold)");
    println!("   - Can add LLM judge for semantic evaluation");

    // -------------------------------------------------------------------------
    // Summary
    // -------------------------------------------------------------------------
    println!("\n=== Example Complete ===\n");
    println!("Key takeaways for graph evaluation:");
    println!("  - Record node executions to verify trajectory");
    println!("  - Use ToolTrajectoryScorer for node order validation");
    println!("  - Check state transformations at each step");
    println!("  - Verify conditional routing decisions");
    println!("  - Use LLM judge for semantic quality assessment");
    println!("  - Combine multiple criteria for comprehensive testing");

    Ok(())
}
