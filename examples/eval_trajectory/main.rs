//! Tool Trajectory Evaluation Example
//!
//! This example demonstrates how to validate that an agent calls
//! the expected tools in the expected order with the expected arguments.
//!
//! Run with: cargo run --example eval_trajectory

use adk_eval::scoring::ToolTrajectoryComparison;
use adk_eval::{EvaluationCriteria, ToolTrajectoryConfig, ToolTrajectoryScorer, ToolUse};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Tool Trajectory Matching ===\n");

    // -------------------------------------------------------------------------
    // 1. Basic tool matching
    // -------------------------------------------------------------------------
    println!("1. Basic tool matching (name only)...\n");

    let scorer = ToolTrajectoryScorer::new();

    let expected = vec![ToolUse::new("search"), ToolUse::new("summarize")];

    let actual = vec![ToolUse::new("search"), ToolUse::new("summarize")];

    let score = scorer.score(&expected, &actual);
    println!("Expected: [search, summarize]");
    println!("Actual:   [search, summarize]");
    println!("Score: {:.1}% match\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 2. Partial matches
    // -------------------------------------------------------------------------
    println!("2. Partial matches...\n");

    let expected = vec![ToolUse::new("tool_a"), ToolUse::new("tool_b"), ToolUse::new("tool_c")];

    let actual = vec![
        ToolUse::new("tool_a"),
        ToolUse::new("tool_x"), // Wrong tool!
        ToolUse::new("tool_c"),
    ];

    let score = scorer.score(&expected, &actual);
    println!("Expected: [tool_a, tool_b, tool_c]");
    println!("Actual:   [tool_a, tool_x, tool_c]");
    println!("Score: {:.1}% match (2 out of 3)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 3. Strict order vs unordered matching
    // -------------------------------------------------------------------------
    println!("3. Strict order vs unordered matching...\n");

    // Strict order scorer (default)
    let strict_scorer = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
        strict_order: true,
        strict_args: false,
    });

    // Unordered scorer
    let unordered_scorer = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
        strict_order: false,
        strict_args: false,
    });

    let expected = vec![ToolUse::new("fetch"), ToolUse::new("process"), ToolUse::new("save")];

    // Tools called in wrong order
    let actual = vec![
        ToolUse::new("process"), // Out of order
        ToolUse::new("fetch"),   // Out of order
        ToolUse::new("save"),
    ];

    let strict_score = strict_scorer.score(&expected, &actual);
    let unordered_score = unordered_scorer.score(&expected, &actual);

    println!("Expected: [fetch, process, save]");
    println!("Actual:   [process, fetch, save]");
    println!("  Strict order:   {:.1}% match", strict_score * 100.0);
    println!("  Unordered:      {:.1}% match\n", unordered_score * 100.0);

    // -------------------------------------------------------------------------
    // 4. Argument matching
    // -------------------------------------------------------------------------
    println!("4. Argument matching...\n");

    // Strict args scorer
    let strict_args_scorer = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
        strict_order: false,
        strict_args: true, // Must match exactly
    });

    // Partial args scorer (default)
    let partial_args_scorer = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
        strict_order: false,
        strict_args: false, // Expected args just need to be present
    });

    let expected = vec![ToolUse::new("get_weather").with_args(json!({"location": "NYC"}))];

    // Actual has extra argument
    let actual = vec![ToolUse::new("get_weather").with_args(json!({
        "location": "NYC",
        "units": "fahrenheit"  // Extra arg not in expected
    }))];

    let strict_score = strict_args_scorer.score(&expected, &actual);
    let partial_score = partial_args_scorer.score(&expected, &actual);

    println!("Expected args: {{\"location\": \"NYC\"}}");
    println!("Actual args:   {{\"location\": \"NYC\", \"units\": \"fahrenheit\"}}");
    println!("  Strict args:  {:.1}% match (exact match required)", strict_score * 100.0);
    println!("  Partial args: {:.1}% match (extra args allowed)\n", partial_score * 100.0);

    // -------------------------------------------------------------------------
    // 5. Detailed comparison
    // -------------------------------------------------------------------------
    println!("5. Detailed comparison with diagnostics...\n");

    let expected = vec![
        ToolUse::new("search").with_args(json!({"query": "rust programming"})),
        ToolUse::new("fetch_url").with_args(json!({"url": "https://example.com"})),
        ToolUse::new("summarize"),
    ];

    let actual = vec![
        ToolUse::new("search").with_args(json!({"query": "rust programming"})),
        ToolUse::new("different_tool"), // Unexpected tool
        ToolUse::new("summarize"),
        ToolUse::new("extra_tool"), // Extra tool
    ];

    let comparison: ToolTrajectoryComparison = scorer.compare(&expected, &actual);

    println!("Comparison Results:");
    println!("  Overall Score: {:.1}%", comparison.score * 100.0);
    println!("\n  Matched tools ({}):", comparison.matched.len());
    for (exp, act) in &comparison.matched {
        println!("    - {} matched {}", exp.name, act.name);
    }
    println!("\n  Missing tools ({}):", comparison.missing.len());
    for tool in &comparison.missing {
        println!("    - {} (expected but not called)", tool.name);
    }
    println!("\n  Extra tools ({}):", comparison.extra.len());
    for tool in &comparison.extra {
        println!("    - {} (called but not expected)", tool.name);
    }

    // -------------------------------------------------------------------------
    // 6. Using with EvaluationCriteria
    // -------------------------------------------------------------------------
    println!("\n6. Configuring criteria for evaluation...\n");

    // Require 100% tool match with strict ordering
    let _strict_criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0), // Must be perfect
        tool_trajectory_config: Some(ToolTrajectoryConfig {
            strict_order: true,
            strict_args: true,
        }),
        ..Default::default()
    };

    // More lenient: 80% match, unordered, partial args
    let _lenient_criteria = EvaluationCriteria {
        tool_trajectory_score: Some(0.8), // 80% is acceptable
        tool_trajectory_config: Some(ToolTrajectoryConfig {
            strict_order: false,
            strict_args: false,
        }),
        ..Default::default()
    };

    println!("Strict criteria:");
    println!("  - Threshold: 100%");
    println!("  - Order: strict");
    println!("  - Args: strict");

    println!("\nLenient criteria:");
    println!("  - Threshold: 80%");
    println!("  - Order: any");
    println!("  - Args: partial match");

    // Using the builder pattern
    let criteria = EvaluationCriteria::exact_tools().with_response_similarity(0.8);

    println!("\nBuilder pattern example:");
    println!("  EvaluationCriteria::exact_tools() creates:");
    println!("    - tool_trajectory_score: {:?}", criteria.tool_trajectory_score);
    println!("    - response_similarity: {:?}", criteria.response_similarity);

    println!("\n=== Example Complete ===");
    println!("\nKey takeaways:");
    println!("  - ToolTrajectoryScorer compares expected vs actual tool calls");
    println!("  - strict_order: require tools in exact sequence");
    println!("  - strict_args: require exact argument match");
    println!("  - Use compare() for detailed diagnostics");

    Ok(())
}
