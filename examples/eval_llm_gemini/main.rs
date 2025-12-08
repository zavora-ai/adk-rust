//! LLM-Judged Evaluation with Gemini
//!
//! This example demonstrates running actual LLM-judged evaluations using
//! Google's Gemini model as the judge for semantic matching and rubric scoring.
//!
//! Run with: cargo run --example eval_llm_gemini
//!
//! Requires: GOOGLE_API_KEY environment variable

use adk_core::Llm;
use adk_eval::criteria::RubricLevel;
use adk_eval::{
    EvaluationConfig, EvaluationCriteria, Evaluator, LlmJudge, LlmJudgeConfig, Rubric, RubricConfig,
};
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: LLM-Judged Evaluation with Gemini ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("❌ GOOGLE_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example eval_llm_gemini");
            return Ok(());
        }
    };

    println!("✅ API key loaded\n");

    // -------------------------------------------------------------------------
    // 1. Create the Gemini judge model
    // -------------------------------------------------------------------------
    println!("1. Creating Gemini judge model...\n");

    let judge_model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);
    println!("   Model: gemini-2.0-flash");
    println!("   Purpose: Semantic evaluation and rubric scoring\n");

    // -------------------------------------------------------------------------
    // 2. Create LLM Judge with configuration
    // -------------------------------------------------------------------------
    println!("2. Creating LLM Judge...\n");

    let judge_config = LlmJudgeConfig {
        max_tokens: 512,
        temperature: 0.0, // Deterministic for consistent scoring
    };

    let judge = LlmJudge::with_config(Arc::clone(&judge_model), judge_config);
    println!("   Config: max_tokens=512, temperature=0.0\n");

    // -------------------------------------------------------------------------
    // 3. Test semantic matching
    // -------------------------------------------------------------------------
    println!("3. Testing semantic matching...\n");

    let test_cases = vec![
        (
            "What is the capital of France?",
            "The capital of France is Paris.",
            "Paris is the capital city of France, located in the north of the country.",
        ),
        ("What is 2 + 2?", "The answer is 4.", "Four."),
        (
            "Is water wet?",
            "Yes, water is wet.",
            "No, water is not wet because wetness is a property of solid surfaces.",
        ),
    ];

    for (question, expected, actual) in &test_cases {
        println!("   Question: {}", question);
        println!("   Expected: \"{}\"", expected);
        println!("   Actual:   \"{}\"", actual);

        // semantic_match takes (expected, actual, optional config)
        let result = judge.semantic_match(expected, actual, None).await?;

        println!("   Result:");
        println!("      Equivalent: {}", if result.score >= 0.7 { "YES" } else { "NO" });
        println!("      Score: {:.0}%", result.score * 100.0);
        println!("      Reasoning: {}", result.reasoning);
        println!();
    }

    // -------------------------------------------------------------------------
    // 4. Test rubric evaluation
    // -------------------------------------------------------------------------
    println!("4. Testing rubric evaluation...\n");

    let rubrics = vec![
        Rubric::new("Accuracy", "Response is factually correct").with_weight(0.4).with_levels(
            vec![
                RubricLevel { score: 1.0, description: "Completely accurate".to_string() },
                RubricLevel { score: 0.5, description: "Partially accurate".to_string() },
                RubricLevel { score: 0.0, description: "Inaccurate".to_string() },
            ],
        ),
        Rubric::new("Helpfulness", "Response addresses the user's question")
            .with_weight(0.3)
            .with_levels(vec![
                RubricLevel { score: 1.0, description: "Fully addresses question".to_string() },
                RubricLevel { score: 0.5, description: "Partially addresses question".to_string() },
                RubricLevel { score: 0.0, description: "Does not address question".to_string() },
            ]),
        Rubric::new("Clarity", "Response is clear and well-organized")
            .with_weight(0.3)
            .with_levels(vec![
                RubricLevel { score: 1.0, description: "Very clear".to_string() },
                RubricLevel { score: 0.5, description: "Somewhat clear".to_string() },
                RubricLevel { score: 0.0, description: "Unclear".to_string() },
            ]),
    ];

    let rubric_config = RubricConfig { rubrics };

    let question = "Explain what recursion is in programming.";
    let response = "Recursion is when a function calls itself to solve a problem. \
        It breaks down complex problems into smaller, similar subproblems. \
        A recursive function needs a base case to stop the recursion.";

    println!("   Question: {}", question);
    println!("   Response: \"{}\"\n", response);

    // evaluate_rubrics takes (response, context, rubric_config)
    let rubric_result = judge.evaluate_rubrics(response, question, &rubric_config).await?;

    println!("   Rubric Results:");
    println!("      Overall Score: {:.0}%", rubric_result.overall_score * 100.0);
    println!();
    for score in &rubric_result.rubric_scores {
        println!("      {}: {:.0}%", score.name, score.score * 100.0);
        println!("         {}", score.reasoning);
    }
    println!();

    // -------------------------------------------------------------------------
    // 5. Test safety evaluation
    // -------------------------------------------------------------------------
    println!("5. Testing safety evaluation...\n");

    let safe_response =
        "The weather in Paris is typically mild with an average temperature of 15°C.";
    let safety_result = judge.evaluate_safety(safe_response).await?;

    println!("   Response: \"{}\"", safe_response);
    println!("   Safe: {}", if safety_result.is_safe { "YES" } else { "NO" });
    println!("   Score: {:.0}%", safety_result.score * 100.0);
    if !safety_result.issues.is_empty() {
        println!("   Issues: {:?}", safety_result.issues);
    }
    println!();

    // -------------------------------------------------------------------------
    // 6. Test hallucination detection
    // -------------------------------------------------------------------------
    println!("6. Testing hallucination detection...\n");

    let context = "The Eiffel Tower is located in Paris, France. It was built in 1889.";
    let response_with_facts = "The Eiffel Tower, built in 1889, is a famous landmark in Paris.";

    println!("   Context: \"{}\"", context);
    println!("   Response: \"{}\"", response_with_facts);

    let hallucination_result = judge
        .detect_hallucinations(
            response_with_facts,
            context,
            None, // No additional ground truth
        )
        .await?;

    println!(
        "   Hallucination-free: {}",
        if hallucination_result.hallucination_free { "YES" } else { "NO" }
    );
    println!("   Score: {:.0}%", hallucination_result.score * 100.0);
    if !hallucination_result.issues.is_empty() {
        println!("   Issues: {:?}", hallucination_result.issues);
    }
    println!();

    // -------------------------------------------------------------------------
    // 7. Create full Evaluator with LLM judge
    // -------------------------------------------------------------------------
    println!("7. Creating full Evaluator with LLM judge...\n");

    let criteria = EvaluationCriteria {
        semantic_match_score: Some(0.8), // Require 80% semantic similarity
        response_similarity: Some(0.5),  // Also check basic text similarity
        rubric_quality_score: Some(0.7), // Require 70% rubric score
        rubric_config: Some(RubricConfig {
            rubrics: vec![
                Rubric::new("Accuracy", "Response is factually correct").with_weight(0.5),
                Rubric::new("Helpfulness", "Response is helpful").with_weight(0.5),
            ],
        }),
        safety_score: Some(0.9), // Require 90% safety score
        ..Default::default()
    };

    let _evaluator =
        Evaluator::with_llm_judge(EvaluationConfig::with_criteria(criteria), judge_model);

    println!("   Evaluator created with:");
    println!("   - Semantic match threshold: 80%");
    println!("   - Response similarity threshold: 50%");
    println!("   - Rubric quality threshold: 70%");
    println!("   - Safety threshold: 90%");
    println!("   - 2 rubrics (Accuracy, Helpfulness)\n");

    // -------------------------------------------------------------------------
    // 8. Summary
    // -------------------------------------------------------------------------
    println!("=== Example Complete ===\n");
    println!("Key takeaways:");
    println!("  - Use GeminiModel as the LLM judge");
    println!("  - semantic_match() compares meaning, not just text");
    println!("  - evaluate_rubrics() scores across multiple dimensions");
    println!("  - evaluate_safety() checks for harmful content");
    println!("  - detect_hallucinations() finds made-up facts");
    println!("  - Combine with text similarity for comprehensive evaluation");
    println!("  - Use temperature=0.0 for consistent results");

    Ok(())
}
