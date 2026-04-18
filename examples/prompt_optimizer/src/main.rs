//! # Prompt Optimizer Example
//!
//! Demonstrates the prompt optimization workflow from `adk-eval` — iteratively
//! improving an agent's system instructions using an optimizer LLM and an
//! evaluation set.
//!
//! ## What This Shows
//!
//! - Defining an initial agent with intentionally suboptimal instructions
//! - Loading an evaluation set of test cases from JSON files
//! - Configuring a `PromptOptimizer` with a separate optimizer LLM, evaluator,
//!   and optimization parameters (`max_iterations`, `target_threshold`)
//! - Running the optimization loop and printing iteration scores and proposed
//!   changes
//! - Early stopping when the target threshold is met
//! - Writing the best-performing instructions to `optimized_instructions.txt`
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/prompt_optimizer/Cargo.toml
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use adk_core::model::Llm;
use adk_eval::evaluator::{EvaluationConfig, Evaluator};
use adk_eval::optimizer::{OptimizerConfig, PromptOptimizer};
use adk_eval::schema::{EvalCase, EvalSet, Turn};
use adk_eval::schema::ContentData;
use adk_model::GeminiModel;
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Helper: require an environment variable or exit with a descriptive message
// ---------------------------------------------------------------------------

fn require_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "Missing required environment variable: {name}\n\
             Set it in your .env file or export it in your shell.\n\
             See .env.example for all required variables."
        )
    })
}

// ---------------------------------------------------------------------------
// Eval set test case format (loaded from JSON files in eval_set/)
// ---------------------------------------------------------------------------

/// A single test case loaded from the eval_set/ directory.
///
/// Each JSON file contains an input prompt and expected output criteria
/// that the optimizer uses to score the agent's performance.
#[derive(Debug, Deserialize)]
struct TestCaseFile {
    /// The user prompt to send to the agent
    input: String,
    /// Description of what a good response should contain
    expected: String,
    /// Optional category tags for the test case
    #[serde(default)]
    tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Load eval set from the eval_set/ directory
// ---------------------------------------------------------------------------

/// Load test case JSON files from the `eval_set/` directory and convert them
/// into an `EvalSet` that the optimizer can evaluate against.
fn load_eval_set(dir: &str) -> anyhow::Result<EvalSet> {
    let path = std::path::Path::new(dir);
    if !path.is_dir() {
        anyhow::bail!("Eval set directory not found: {dir}");
    }

    let mut eval_cases = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "json")
        })
        .collect();

    // Sort by filename for deterministic ordering
    entries.sort_by_key(|e| e.file_name());

    for (idx, entry) in entries.iter().enumerate() {
        let file_path = entry.path();
        let content = std::fs::read_to_string(&file_path)?;
        let test_case: TestCaseFile = serde_json::from_str(&content).map_err(|e| {
            anyhow::anyhow!("Failed to parse {}: {e}", file_path.display())
        })?;

        // Convert the test case file into an EvalCase with a single-turn
        // conversation: user sends the input, agent should produce a response
        // matching the expected criteria.
        let eval_case = EvalCase {
            eval_id: format!("test_case_{}", idx + 1),
            description: format!("Test: {}", test_case.input),
            conversation: vec![Turn {
                invocation_id: format!("inv_{}", idx + 1),
                user_content: ContentData::text(&test_case.input),
                final_response: Some(ContentData::model_response(&test_case.expected)),
                intermediate_data: None,
            }],
            session_input: Default::default(),
            tags: test_case.tags,
        };

        eval_cases.push(eval_case);
    }

    Ok(EvalSet {
        eval_set_id: "prompt_optimizer_eval_set".to_string(),
        name: "Prompt Optimizer Evaluation Set".to_string(),
        description: "Test cases for evaluating and optimizing agent instructions".to_string(),
        test_files: vec![],
        eval_cases,
    })
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  Prompt Optimizer — ADK-Rust v0.7.0      ║");
    println!("╚══════════════════════════════════════════╝\n");

    let api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // 1. Define initial agent with intentionally suboptimal instructions
    // -----------------------------------------------------------------------
    // The instructions below are deliberately vague — "Answer questions" gives
    // the agent no guidance on tone, depth, or domain focus. The optimizer
    // will iteratively improve these instructions.

    let initial_instructions = "Answer questions";

    println!("📝 Initial agent instructions:");
    println!("   \"{initial_instructions}\"\n");
    println!("   (intentionally vague — the optimizer will improve these)\n");

    // Build the agent under test using LlmAgent
    let agent_model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);
    let agent: Arc<dyn adk_core::Agent> = Arc::new(
        adk_agent::LlmAgentBuilder::new("rust-tutor")
            .model(agent_model)
            .instruction(initial_instructions)
            .description(initial_instructions)
            .build()?,
    );

    // -----------------------------------------------------------------------
    // 2. Load eval set from eval_set/ directory
    // -----------------------------------------------------------------------

    let eval_set_dir = format!("{}/eval_set", env!("CARGO_MANIFEST_DIR"));
    let eval_set = load_eval_set(&eval_set_dir)?;
    println!(
        "📋 Loaded {} test case(s) from eval_set/:\n",
        eval_set.eval_cases.len()
    );
    for case in &eval_set.eval_cases {
        let input_text = case
            .conversation
            .first()
            .map(|t| t.user_content.get_text())
            .unwrap_or_default();
        let tags = case.tags.join(", ");
        println!("   • {} — {}", case.eval_id, input_text);
        if !tags.is_empty() {
            println!("     Tags: [{tags}]");
        }
    }
    println!();

    // -----------------------------------------------------------------------
    // 3. Configure the PromptOptimizer
    // -----------------------------------------------------------------------
    // The optimizer uses a separate LLM instance to propose instruction
    // improvements. The evaluator scores the agent against the eval set.

    let optimizer_llm: Arc<dyn Llm> =
        Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    let evaluator = Evaluator::new(EvaluationConfig {
        criteria: adk_eval::EvaluationCriteria {
            response_similarity: Some(0.5),
            ..Default::default()
        },
        continue_on_failure: true,
        ..Default::default()
    });

    let config = OptimizerConfig {
        max_iterations: 3,
        target_threshold: 0.8,
        output_path: PathBuf::from("optimized_instructions.txt"),
    };

    println!("⚙️  Optimizer configuration:");
    println!("   Max iterations:    {}", config.max_iterations);
    println!("   Target threshold:  {}", config.target_threshold);
    println!("   Output path:       {}\n", config.output_path.display());

    let optimizer = PromptOptimizer::new(optimizer_llm, evaluator, config);

    // -----------------------------------------------------------------------
    // 4. Run the optimization loop
    // -----------------------------------------------------------------------
    // The optimizer evaluates the agent, proposes improved instructions via
    // the optimizer LLM, applies the best improvement, and repeats. It stops
    // early if the target threshold is reached.

    println!("🚀 Starting optimization loop...\n");
    println!("═══════════════════════════════════════════");

    let result = optimizer.optimize(agent, &eval_set).await.map_err(|e| {
        anyhow::anyhow!("Optimization failed: {e}")
    })?;

    println!("═══════════════════════════════════════════\n");

    // -----------------------------------------------------------------------
    // 5. Print results
    // -----------------------------------------------------------------------

    println!("📊 Optimization Results:");
    println!("   Initial score:     {:.2}", result.initial_score);
    println!("   Final score:       {:.2}", result.final_score);
    println!("   Iterations run:    {}", result.iterations_run);
    println!();

    if result.iterations_run == 0 {
        println!("   ⚡ No optimization needed — initial score already met the target threshold.");
    } else if result.final_score >= 0.8 {
        println!("   🎯 Target threshold reached! Early stopping triggered.");
    } else {
        println!(
            "   ⏱️  Max iterations reached. Best score: {:.2}",
            result.final_score
        );
    }
    println!();

    // -----------------------------------------------------------------------
    // 6. Show the optimized instructions
    // -----------------------------------------------------------------------

    println!("📝 Best instructions found:");
    println!("   ────────────────────────────────────────");
    for line in result.best_instructions.lines() {
        println!("   {line}");
    }
    println!("   ────────────────────────────────────────\n");

    println!(
        "💾 Optimized instructions written to: optimized_instructions.txt"
    );

    // -----------------------------------------------------------------------
    // Summary
    // -----------------------------------------------------------------------

    println!();
    println!("═══════════════════════════════════════════");
    println!("  Summary");
    println!("═══════════════════════════════════════════\n");
    println!("  The prompt optimizer iteratively improved the agent's");
    println!("  system instructions from a vague starting point:");
    println!("    Before: \"Answer questions\"");
    println!(
        "    After:  (see optimized_instructions.txt)"
    );
    println!(
        "    Score:  {:.2} → {:.2}",
        result.initial_score, result.final_score
    );
    println!();
    println!("  Key concepts demonstrated:");
    println!("    • PromptOptimizer with separate optimizer LLM");
    println!("    • Evaluation set loaded from JSON files");
    println!("    • Configurable max_iterations and target_threshold");
    println!("    • Early stopping when target score is reached");
    println!("    • Best instructions persisted to output file\n");

    println!("✅ Prompt Optimizer example completed successfully.");
    Ok(())
}
