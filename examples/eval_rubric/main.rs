//! Rubric-Based Evaluation Example
//!
//! This example demonstrates how to define custom rubrics for
//! evaluating agent response quality across multiple dimensions.
//!
//! Run with: cargo run --example eval_rubric

use adk_eval::criteria::RubricLevel;
use adk_eval::{EvaluationCriteria, Rubric, RubricConfig, RubricEvaluationResult, RubricScore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Rubric-Based Evaluation ===\n");

    // -------------------------------------------------------------------------
    // 1. What is a rubric?
    // -------------------------------------------------------------------------
    println!("1. What is a rubric?\n");

    println!("A rubric defines quality criteria with scoring guidelines:");
    println!("  - Name: What aspect is being evaluated");
    println!("  - Description: What makes a good response");
    println!("  - Weight: How important this criterion is (0.0-1.0)");
    println!("  - Levels: Optional scoring scale with descriptions\n");

    // -------------------------------------------------------------------------
    // 2. Creating simple rubrics
    // -------------------------------------------------------------------------
    println!("2. Creating simple rubrics...\n");

    // Basic rubric with just name and description
    let accuracy = Rubric::new("Accuracy", "The response contains factually correct information");

    let helpfulness =
        Rubric::new("Helpfulness", "The response directly addresses the user's question or need");

    let clarity =
        Rubric::new("Clarity", "The response is clear, well-organized, and easy to understand");

    println!("Created rubrics:");
    println!("  - {}: {}", accuracy.name, accuracy.description);
    println!("  - {}: {}", helpfulness.name, helpfulness.description);
    println!("  - {}: {}\n", clarity.name, clarity.description);

    // -------------------------------------------------------------------------
    // 3. Adding weights to rubrics
    // -------------------------------------------------------------------------
    println!("3. Adding weights to rubrics...\n");

    // Weights determine relative importance
    let weighted_rubrics = vec![
        Rubric::new("Accuracy", "Response is factually correct").with_weight(0.5), // 50% of total score
        Rubric::new("Helpfulness", "Addresses user's needs").with_weight(0.3), // 30% of total score
        Rubric::new("Clarity", "Clear and well-organized").with_weight(0.2),   // 20% of total score
    ];

    println!("Weighted rubrics (sum to 1.0):");
    for rubric in &weighted_rubrics {
        println!("  - {} (weight: {:.0}%)", rubric.name, rubric.weight * 100.0);
    }
    println!();

    // -------------------------------------------------------------------------
    // 4. Adding scoring levels
    // -------------------------------------------------------------------------
    println!("4. Adding scoring levels...\n");

    let detailed_rubric = Rubric::new(
        "Code Quality",
        "The generated code is correct, efficient, and follows best practices",
    )
    .with_weight(1.0)
    .with_levels(vec![
        RubricLevel {
            score: 1.0,
            description:
                "Excellent: Code is correct, efficient, well-documented, and handles edge cases"
                    .to_string(),
        },
        RubricLevel {
            score: 0.8,
            description: "Good: Code is correct and reasonably efficient with minor issues"
                .to_string(),
        },
        RubricLevel {
            score: 0.6,
            description: "Acceptable: Code works but has performance or style issues".to_string(),
        },
        RubricLevel {
            score: 0.4,
            description: "Needs improvement: Code has bugs or significant issues".to_string(),
        },
        RubricLevel {
            score: 0.2,
            description: "Poor: Code is largely incorrect or unusable".to_string(),
        },
        RubricLevel { score: 0.0, description: "Unacceptable: No valid code provided".to_string() },
    ]);

    println!("Rubric: {}", detailed_rubric.name);
    println!("Description: {}", detailed_rubric.description);
    println!("Scoring levels:");
    for level in &detailed_rubric.levels {
        println!("  {:.0}%: {}", level.score * 100.0, level.description);
    }
    println!();

    // -------------------------------------------------------------------------
    // 5. Creating a RubricConfig
    // -------------------------------------------------------------------------
    println!("5. Creating a RubricConfig...\n");

    let rubric_config = RubricConfig {
        rubrics: vec![
            Rubric::new("Accuracy", "Response is factually correct").with_weight(0.4),
            Rubric::new("Completeness", "Response fully addresses the question").with_weight(0.3),
            Rubric::new("Conciseness", "Response is appropriately concise").with_weight(0.2),
            Rubric::new("Tone", "Response has appropriate professional tone").with_weight(0.1),
        ],
    };

    println!("RubricConfig with {} rubrics:", rubric_config.rubrics.len());
    for rubric in &rubric_config.rubrics {
        println!("  - {} ({:.0}%)", rubric.name, rubric.weight * 100.0);
    }
    println!();

    // -------------------------------------------------------------------------
    // 6. Using rubrics with EvaluationCriteria
    // -------------------------------------------------------------------------
    println!("6. Using rubrics with EvaluationCriteria...\n");

    let criteria = EvaluationCriteria::default().with_rubrics(
        0.7,
        vec![
            // Require 70% overall rubric score
            Rubric::new("Accuracy", "Response is factually correct").with_weight(0.5),
            Rubric::new("Helpfulness", "Addresses user's needs").with_weight(0.3),
            Rubric::new("Clarity", "Clear and well-organized").with_weight(0.2),
        ],
    );

    println!("Criteria configured:");
    println!("  - Rubric threshold: 70%");
    println!(
        "  - Rubrics: {:?}",
        criteria.rubric_config.as_ref().map(|c| c.rubrics.len()).unwrap_or(0)
    );
    println!();

    // -------------------------------------------------------------------------
    // 7. Example rubric evaluation result
    // -------------------------------------------------------------------------
    println!("7. Example rubric evaluation result...\n");

    // Simulated evaluation result
    let result = RubricEvaluationResult {
        overall_score: 0.78,
        rubric_scores: vec![
            RubricScore {
                name: "Accuracy".to_string(),
                score: 0.9,
                reasoning: "Response contains correct information with no factual errors"
                    .to_string(),
            },
            RubricScore {
                name: "Helpfulness".to_string(),
                score: 0.7,
                reasoning: "Addresses the main question but misses some context".to_string(),
            },
            RubricScore {
                name: "Clarity".to_string(),
                score: 0.6,
                reasoning: "Mostly clear but could be better organized".to_string(),
            },
        ],
    };

    println!("Evaluation Result:");
    println!("  Overall Score: {:.0}%", result.overall_score * 100.0);
    println!("\n  Individual Rubrics:");
    for score in &result.rubric_scores {
        println!("    {}: {:.0}%", score.name, score.score * 100.0);
        println!("      Reason: {}", score.reasoning);
    }
    println!();

    // -------------------------------------------------------------------------
    // 8. Domain-specific rubric examples
    // -------------------------------------------------------------------------
    println!("8. Domain-specific rubric examples...\n");

    println!("Customer Support Agent:");
    let support_rubrics = vec![
        ("Empathy", 0.25, "Acknowledges customer's feelings and shows understanding"),
        ("Solution Quality", 0.35, "Provides accurate and actionable solution"),
        ("Professionalism", 0.20, "Maintains professional and courteous tone"),
        ("Completeness", 0.20, "Addresses all parts of the customer's inquiry"),
    ];
    for (name, weight, desc) in &support_rubrics {
        println!("  - {} ({:.0}%): {}", name, weight * 100.0, desc);
    }

    println!("\nCode Generation Agent:");
    let code_rubrics = vec![
        ("Correctness", 0.40, "Code compiles and produces correct output"),
        ("Efficiency", 0.20, "Algorithm has appropriate time/space complexity"),
        ("Readability", 0.20, "Code is well-formatted with clear variable names"),
        ("Best Practices", 0.20, "Follows language idioms and conventions"),
    ];
    for (name, weight, desc) in &code_rubrics {
        println!("  - {} ({:.0}%): {}", name, weight * 100.0, desc);
    }

    println!("\nResearch Agent:");
    let research_rubrics = vec![
        ("Source Quality", 0.30, "Uses credible, authoritative sources"),
        ("Accuracy", 0.30, "Information is factually correct and up-to-date"),
        ("Synthesis", 0.25, "Effectively combines information from multiple sources"),
        ("Citation", 0.15, "Properly attributes sources"),
    ];
    for (name, weight, desc) in &research_rubrics {
        println!("  - {} ({:.0}%): {}", name, weight * 100.0, desc);
    }
    println!();

    // -------------------------------------------------------------------------
    // 9. Production setup
    // -------------------------------------------------------------------------
    println!("9. Production setup (code example)...\n");

    println!(
        r#"
// Define your rubrics
let criteria = EvaluationCriteria::default()
    .with_rubrics(0.75, vec![
        Rubric::new("Accuracy", "Factually correct")
            .with_weight(0.4)
            .with_levels(vec![
                RubricLevel {{ score: 1.0, description: "Perfect accuracy".into() }},
                RubricLevel {{ score: 0.5, description: "Minor errors".into() }},
                RubricLevel {{ score: 0.0, description: "Major errors".into() }},
            ]),
        Rubric::new("Helpfulness", "Addresses user needs")
            .with_weight(0.4),
        Rubric::new("Tone", "Professional tone")
            .with_weight(0.2),
    ]);

// Create evaluator with LLM judge (required for rubric evaluation)
let judge_model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);
let evaluator = Evaluator::with_llm_judge(
    EvaluationConfig::with_criteria(criteria),
    judge_model,
);
"#
    );

    println!("\n=== Example Complete ===");
    println!("\nKey takeaways:");
    println!("  - Rubrics define multi-dimensional quality criteria");
    println!("  - Use weights to prioritize what matters most");
    println!("  - Scoring levels help LLM judge consistently");
    println!("  - Requires LLM judge (Evaluator::with_llm_judge)");
    println!("  - Great for complex evaluation beyond text matching");

    Ok(())
}
