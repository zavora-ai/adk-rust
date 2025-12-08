//! LLM-Judged Semantic Evaluation Example
//!
//! This example demonstrates how to use an LLM as a judge to evaluate
//! semantic similarity between expected and actual responses.
//!
//! Run with: cargo run --example eval_semantic
//!
//! Note: Requires GOOGLE_API_KEY environment variable for actual LLM calls.
//! This example shows the API and parses mock responses for demonstration.

use adk_core::{Content, LlmResponse};
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator, LlmJudge, LlmJudgeConfig};
use adk_model::MockLlm;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: LLM-Judged Semantic Evaluation ===\n");

    // -------------------------------------------------------------------------
    // 1. Creating an LLM Judge
    // -------------------------------------------------------------------------
    println!("1. Creating an LLM Judge...\n");

    // For this example, we use a mock LLM
    // In production, use GeminiModel, OpenAIClient, or AnthropicClient
    let mock_response = r#"EQUIVALENT: YES
SCORE: 0.92
REASONING: Both responses correctly state that Python is a programming language, though the actual response provides slightly more detail about it being high-level and interpreted."#;

    let mock_llm = MockLlm::new("mock-judge")
        .with_response(LlmResponse::new(Content::new("assistant").with_text(mock_response)));

    let _judge = LlmJudge::new(Arc::new(mock_llm));

    println!("Created LlmJudge with mock model");
    println!("  In production, use:");
    println!("    - GeminiModel::new(&api_key, \"gemini-2.0-flash\")");
    println!("    - OpenAIClient::new(config)");
    println!("    - AnthropicClient::new(config)\n");

    // -------------------------------------------------------------------------
    // 2. Understanding semantic matching
    // -------------------------------------------------------------------------
    println!("2. What is semantic matching?\n");

    println!("Text similarity (e.g., Jaccard) compares words:");
    println!("  Expected: \"The answer is 42\"");
    println!("  Actual:   \"42 is the answer\"");
    println!("  -> Different word positions = lower text similarity\n");

    println!("Semantic matching judges meaning:");
    println!("  Expected: \"The answer is 42\"");
    println!("  Actual:   \"The result equals forty-two\"");
    println!("  -> Same meaning = high semantic similarity\n");

    // -------------------------------------------------------------------------
    // 3. Response format from LLM judge
    // -------------------------------------------------------------------------
    println!("3. LLM Judge response format...\n");

    println!("The LLM judge is prompted to respond in this format:");
    println!("  EQUIVALENT: [YES/NO/PARTIAL]");
    println!("  SCORE: [0.0-1.0]");
    println!("  REASONING: [Brief explanation]\n");

    println!("Example judge response:");
    println!("{}\n", mock_response);

    // -------------------------------------------------------------------------
    // 4. Configuring the LLM Judge
    // -------------------------------------------------------------------------
    println!("4. Configuring the LLM Judge...\n");

    let config = LlmJudgeConfig {
        max_tokens: 256,  // Limit response length
        temperature: 0.0, // Deterministic for consistency
    };

    println!("LlmJudgeConfig:");
    println!("  max_tokens: {} (keep judge responses concise)", config.max_tokens);
    println!("  temperature: {} (0.0 = deterministic)\n", config.temperature);

    // Using custom config
    let mock_llm2 = MockLlm::new("judge-v2")
        .with_response(LlmResponse::new(Content::new("assistant").with_text(mock_response)));
    let _judge_with_config = LlmJudge::with_config(Arc::new(mock_llm2), config);

    // -------------------------------------------------------------------------
    // 5. Setting up Evaluator with LLM Judge
    // -------------------------------------------------------------------------
    println!("5. Setting up Evaluator with LLM Judge...\n");

    // Create criteria requiring semantic matching
    let criteria = EvaluationCriteria {
        semantic_match_score: Some(0.85), // Require 85% semantic similarity
        // Can combine with text-based criteria
        response_similarity: Some(0.7), // Also check text similarity
        ..Default::default()
    };

    println!("Evaluation criteria:");
    println!("  - Semantic match threshold: 85%");
    println!("  - Text similarity threshold: 70%\n");

    // Create evaluator with LLM judge
    let mock_llm3 = MockLlm::new("eval-judge")
        .with_response(LlmResponse::new(Content::new("assistant").with_text(mock_response)));
    let _evaluator =
        Evaluator::with_llm_judge(EvaluationConfig::with_criteria(criteria), Arc::new(mock_llm3));

    println!("Created Evaluator with LLM judge enabled");

    // -------------------------------------------------------------------------
    // 6. Use cases for semantic matching
    // -------------------------------------------------------------------------
    println!("\n6. When to use semantic matching...\n");

    println!("GOOD use cases:");
    println!("  - Responses with equivalent meaning but different wording");
    println!("  - Validating factual correctness (not just text match)");
    println!("  - Multi-language or paraphrase scenarios");
    println!("  - Open-ended questions with multiple valid answers\n");

    println!("Consider text similarity instead when:");
    println!("  - Exact format is required (e.g., JSON, code)");
    println!("  - Speed is critical (LLM calls are slower)");
    println!("  - Cost is a concern (each eval = LLM API call)");
    println!("  - Simple keyword matching is sufficient\n");

    // -------------------------------------------------------------------------
    // 7. Example semantic comparisons
    // -------------------------------------------------------------------------
    println!("7. Example semantic comparisons...\n");

    let examples = [
        (
            "What is Python?",
            "Python is a programming language.",
            "Python is a high-level, interpreted programming language known for readability.",
            "HIGH - Both describe Python as a programming language",
        ),
        ("What's 2+2?", "The answer is 4.", "Four.", "HIGH - Same answer, different formats"),
        (
            "Is the sky blue?",
            "Yes, the sky is blue.",
            "No, the sky is green.",
            "LOW - Factually contradictory",
        ),
        (
            "Explain recursion",
            "Recursion is when a function calls itself.",
            "A recursive function references itself in its definition.",
            "HIGH - Equivalent explanations",
        ),
    ];

    for (question, expected, actual, similarity) in examples {
        println!("  Q: {}", question);
        println!("    Expected: \"{}\"", expected);
        println!("    Actual:   \"{}\"", actual);
        println!("    Semantic: {}\n", similarity);
    }

    // -------------------------------------------------------------------------
    // 8. Production setup example
    // -------------------------------------------------------------------------
    println!("8. Production setup (code example)...\n");

    println!(
        r#"
// Production setup with real LLM
use adk_model::GeminiModel;

let api_key = std::env::var("GOOGLE_API_KEY")?;
let judge_model = Arc::new(
    GeminiModel::new(&api_key, "gemini-2.0-flash")?
);

let criteria = EvaluationCriteria::semantic_match(0.85)
    .with_response_similarity(0.7);

let evaluator = Evaluator::with_llm_judge(
    EvaluationConfig::with_criteria(criteria),
    judge_model,
);

// Run evaluation
let report = evaluator
    .evaluate_file(agent, "tests/my_agent.test.json")
    .await?;
"#
    );

    println!("\n=== Example Complete ===");
    println!("\nKey takeaways:");
    println!("  - LlmJudge uses an LLM to evaluate semantic equivalence");
    println!("  - More accurate than text similarity for meaning comparison");
    println!("  - Slower and costs money (API calls)");
    println!("  - Configure with Evaluator::with_llm_judge()");
    println!("  - Use temperature=0.0 for consistent judgments");

    Ok(())
}
