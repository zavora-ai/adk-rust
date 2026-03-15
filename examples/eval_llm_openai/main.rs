//! LLM-Judged Evaluation with OpenAI
//!
//! This example demonstrates running actual LLM-judged evaluations using
//! OpenAI's GPT models as the judge for semantic matching and rubric scoring.
//!
//! Run with: cargo run --example eval_llm_openai --features openai
//!
//! Requires: OPENAI_API_KEY environment variable

use adk_core::Llm;
use adk_eval::criteria::RubricLevel;
use adk_eval::{
    EvaluationConfig, EvaluationCriteria, Evaluator, LlmJudge, LlmJudgeConfig, Rubric, RubricConfig,
};
use adk_model::OpenAIClient;
use adk_model::openai::OpenAIConfig;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: LLM-Judged Evaluation with OpenAI ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("❌ OPENAI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export OPENAI_API_KEY=your_api_key");
            println!("  cargo run --example eval_llm_openai --features openai");
            return Ok(());
        }
    };

    println!("✅ API key loaded\n");

    // -------------------------------------------------------------------------
    // 1. Create the OpenAI judge model
    // -------------------------------------------------------------------------
    println!("1. Creating OpenAI judge model...\n");

    let config = OpenAIConfig::new(api_key, "gpt-5-mini"); // Cost-effective for evaluation
    let judge_model: Arc<dyn Llm> = Arc::new(OpenAIClient::new(config)?);
    println!("   Model: gpt-5-mini");
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
            "What programming language is Rust similar to?",
            "Rust is similar to C++ in terms of performance and memory management.",
            "Rust shares similarities with C++ as both are systems programming languages focused on performance.",
        ),
        (
            "What is machine learning?",
            "Machine learning is a type of AI where computers learn from data.",
            "ML is a subset of artificial intelligence that enables systems to learn and improve from experience.",
        ),
        (
            "Is Python compiled or interpreted?",
            "Python is an interpreted language.",
            "Python is a compiled language that runs directly on hardware.",
        ),
    ];

    for (question, expected, actual) in &test_cases {
        println!("   Question: {}", question);
        println!("   Expected: \"{}\"", expected);
        println!("   Actual:   \"{}\"", actual);

        let result = judge.semantic_match(expected, actual, None).await?;

        println!("   Result:");
        println!("      Equivalent: {}", if result.score >= 0.7 { "YES" } else { "NO" });
        println!("      Score: {:.0}%", result.score * 100.0);
        println!("      Reasoning: {}", result.reasoning);
        println!();
    }

    // -------------------------------------------------------------------------
    // 4. Test rubric evaluation for code review
    // -------------------------------------------------------------------------
    println!("4. Testing rubric evaluation (code review scenario)...\n");

    let code_rubrics = vec![
        Rubric::new("Correctness", "Code is functionally correct").with_weight(0.4).with_levels(
            vec![
                RubricLevel {
                    score: 1.0,
                    description: "Code works correctly for all cases".to_string(),
                },
                RubricLevel { score: 0.7, description: "Code works for most cases".to_string() },
                RubricLevel { score: 0.3, description: "Code has significant bugs".to_string() },
                RubricLevel { score: 0.0, description: "Code does not work".to_string() },
            ],
        ),
        Rubric::new("Efficiency", "Code has good time/space complexity")
            .with_weight(0.3)
            .with_levels(vec![
                RubricLevel { score: 1.0, description: "Optimal solution".to_string() },
                RubricLevel { score: 0.5, description: "Acceptable efficiency".to_string() },
                RubricLevel { score: 0.0, description: "Inefficient solution".to_string() },
            ]),
        Rubric::new("Readability", "Code is clean and well-documented")
            .with_weight(0.3)
            .with_levels(vec![
                RubricLevel { score: 1.0, description: "Excellent readability".to_string() },
                RubricLevel { score: 0.5, description: "Acceptable readability".to_string() },
                RubricLevel { score: 0.0, description: "Poor readability".to_string() },
            ]),
    ];

    let rubric_config = RubricConfig { rubrics: code_rubrics };

    let question = "Write a function to check if a number is prime.";
    let response = r#"
fn is_prime(n: u64) -> bool {
    if n < 2 { return false; }
    if n == 2 { return true; }
    if n % 2 == 0 { return false; }
    let sqrt_n = (n as f64).sqrt() as u64;
    for i in (3..=sqrt_n).step_by(2) {
        if n % i == 0 { return false; }
    }
    true
}
"#;

    println!("   Task: {}", question);
    println!("   Response:{}\n", response);

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
    // 5. Compare different response qualities
    // -------------------------------------------------------------------------
    println!("5. Comparing response qualities...\n");

    let quality_rubrics = vec![Rubric::new("Quality", "Overall response quality").with_weight(1.0)];
    let quality_config = RubricConfig { rubrics: quality_rubrics };

    let responses = vec![
        (
            "Good",
            "Recursion is a programming technique where a function calls itself to solve smaller instances of the same problem. It requires a base case to prevent infinite loops.",
        ),
        ("Medium", "Recursion is when a function calls itself."),
        ("Poor", "Recursion is complicated loops."),
    ];

    let question = "Explain recursion in programming.";
    println!("   Question: {}\n", question);

    for (quality, response) in responses {
        let result = judge.evaluate_rubrics(response, question, &quality_config).await?;
        println!("   {} response: \"{}\"", quality, response);
        println!("      Score: {:.0}%", result.overall_score * 100.0);
        println!();
    }

    // -------------------------------------------------------------------------
    // 6. Create production evaluator
    // -------------------------------------------------------------------------
    println!("6. Creating production Evaluator...\n");

    let criteria = EvaluationCriteria {
        semantic_match_score: Some(0.85),
        rubric_quality_score: Some(0.75),
        rubric_config: Some(RubricConfig {
            rubrics: vec![
                Rubric::new("Accuracy", "Factually correct").with_weight(0.4),
                Rubric::new("Completeness", "Fully addresses query").with_weight(0.3),
                Rubric::new("Clarity", "Clear and understandable").with_weight(0.3),
            ],
        }),
        ..Default::default()
    };

    let _evaluator =
        Evaluator::with_llm_judge(EvaluationConfig::with_criteria(criteria), judge_model);

    println!("   Production evaluator created!");
    println!("   Ready to evaluate agent test files with:");
    println!("   - Semantic matching (85% threshold)");
    println!("   - Rubric scoring (75% threshold)");
    println!("   - 3 weighted rubrics\n");

    // -------------------------------------------------------------------------
    // 7. Summary
    // -------------------------------------------------------------------------
    println!("=== Example Complete ===\n");
    println!("Key takeaways:");
    println!("  - OpenAI models (gpt-5-mini) are cost-effective judges");
    println!("  - Semantic matching handles paraphrasing well");
    println!("  - Rubrics enable multi-dimensional quality assessment");
    println!("  - Use temperature=0.0 for reproducible evaluations");
    println!("  - Combine multiple criteria for comprehensive testing");

    Ok(())
}
