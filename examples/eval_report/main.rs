//! Evaluation Reporting Example
//!
//! This example demonstrates the comprehensive reporting capabilities
//! of adk-eval, including summaries, failures, and JSON export.
//!
//! Run with: cargo run --example eval_report

use adk_eval::{EvaluationReport, EvaluationResult, Failure, ToolUse, report::TurnResult};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Detailed Reporting ===\n");

    // -------------------------------------------------------------------------
    // 1. Creating evaluation results
    // -------------------------------------------------------------------------
    println!("1. Creating evaluation results...\n");

    // A passing test result
    let passed_result = EvaluationResult::passed(
        "test_weather_query",
        HashMap::from([
            ("tool_trajectory".to_string(), 1.0),
            ("response_similarity".to_string(), 0.92),
        ]),
        Duration::from_millis(150),
    );

    println!("Passed result: {}", passed_result.eval_id);
    println!("  Passed: {}", passed_result.passed);
    println!("  Scores: {:?}", passed_result.scores);
    println!("  Duration: {:?}\n", passed_result.duration);

    // A failing test result
    let failed_result = EvaluationResult::failed(
        "test_complex_query",
        HashMap::from([
            ("tool_trajectory".to_string(), 0.5),
            ("response_similarity".to_string(), 0.6),
        ]),
        vec![
            Failure::new(
                "tool_trajectory",
                serde_json::json!(["search", "analyze"]),
                serde_json::json!(["search"]),
                0.5,
                1.0,
            )
            .with_details("Expected 2 tool calls, got 1"),
            Failure::new(
                "response_similarity",
                Value::String("Expected detailed analysis".to_string()),
                Value::String("Brief response".to_string()),
                0.6,
                0.8,
            )
            .with_details("Response too brief"),
        ],
        Duration::from_millis(200),
    );

    println!("Failed result: {}", failed_result.eval_id);
    println!("  Passed: {}", failed_result.passed);
    println!("  Failures: {}", failed_result.failures.len());

    // -------------------------------------------------------------------------
    // 2. Understanding failures
    // -------------------------------------------------------------------------
    println!("\n2. Understanding failures...\n");

    for failure in &failed_result.failures {
        println!("Failure: {}", failure.criterion);
        println!(
            "  Score: {:.1}% (threshold: {:.1}%)",
            failure.score * 100.0,
            failure.threshold * 100.0
        );
        println!("  Expected: {:?}", failure.expected);
        println!("  Actual: {:?}", failure.actual);
        if let Some(details) = &failure.details {
            println!("  Details: {}", details);
        }
        println!("  Formatted: {}\n", failure.format());
    }

    // -------------------------------------------------------------------------
    // 3. Creating a full evaluation report
    // -------------------------------------------------------------------------
    println!("3. Creating a full evaluation report...\n");

    let results = vec![
        EvaluationResult::passed(
            "test_1_simple",
            HashMap::from([
                ("tool_trajectory".to_string(), 1.0),
                ("response_similarity".to_string(), 0.95),
            ]),
            Duration::from_millis(100),
        ),
        EvaluationResult::passed(
            "test_2_with_tools",
            HashMap::from([
                ("tool_trajectory".to_string(), 1.0),
                ("response_similarity".to_string(), 0.88),
            ]),
            Duration::from_millis(150),
        ),
        EvaluationResult::failed(
            "test_3_complex",
            HashMap::from([
                ("tool_trajectory".to_string(), 0.67),
                ("response_similarity".to_string(), 0.72),
            ]),
            vec![
                Failure::new("tool_trajectory", Value::Null, Value::Null, 0.67, 1.0)
                    .with_details("Missing one expected tool call"),
            ],
            Duration::from_millis(200),
        ),
        EvaluationResult::passed(
            "test_4_edge_case",
            HashMap::from([
                ("tool_trajectory".to_string(), 1.0),
                ("response_similarity".to_string(), 0.81),
            ]),
            Duration::from_millis(120),
        ),
        EvaluationResult::failed(
            "test_5_error_handling",
            HashMap::from([
                ("tool_trajectory".to_string(), 0.0),
                ("response_similarity".to_string(), 0.45),
            ]),
            vec![
                Failure::new("tool_trajectory", Value::Null, Value::Null, 0.0, 0.8)
                    .with_details("No tools were called"),
                Failure::new("response_similarity", Value::Null, Value::Null, 0.45, 0.7)
                    .with_details("Response did not match expected error message"),
            ],
            Duration::from_millis(180),
        ),
    ];

    let report = EvaluationReport::new(
        "weather_agent_eval_20241208",
        results,
        chrono::Utc::now() - chrono::Duration::seconds(1),
    );

    // -------------------------------------------------------------------------
    // 4. Report summary
    // -------------------------------------------------------------------------
    println!("4. Report summary...\n");

    println!("Report ID: {}", report.run_id);
    println!("Duration: {:?}", report.duration);
    println!("\nSummary:");
    println!("  Total tests: {}", report.summary.total);
    println!("  Passed: {}", report.summary.passed);
    println!("  Failed: {}", report.summary.failed);
    println!("  Pass rate: {:.1}%", report.summary.pass_rate * 100.0);
    println!("\nAverage scores:");
    for (criterion, avg) in &report.summary.avg_scores {
        println!("  {}: {:.1}%", criterion, avg * 100.0);
    }

    // -------------------------------------------------------------------------
    // 5. Format as human-readable string
    // -------------------------------------------------------------------------
    println!("\n5. Formatted report summary...\n");

    println!("{}", report.format_summary());

    // -------------------------------------------------------------------------
    // 6. Access failures directly
    // -------------------------------------------------------------------------
    println!("6. Accessing failed tests...\n");

    let failures = report.failures();
    println!("Failed tests ({}):", failures.len());
    for result in failures {
        println!("\n  Test: {}", result.eval_id);
        println!("  Duration: {:?}", result.duration);
        println!("  Failures:");
        for failure in &result.failures {
            println!(
                "    - {} (score: {:.1}%, threshold: {:.1}%)",
                failure.criterion,
                failure.score * 100.0,
                failure.threshold * 100.0
            );
            if let Some(details) = &failure.details {
                println!("      {}", details);
            }
        }
    }

    // -------------------------------------------------------------------------
    // 7. Check if all tests passed
    // -------------------------------------------------------------------------
    println!("\n7. Checking pass/fail status...\n");

    if report.all_passed() {
        println!("All tests passed!");
    } else {
        println!(
            "Some tests failed. {} of {} passed.",
            report.summary.passed, report.summary.total
        );
    }

    // -------------------------------------------------------------------------
    // 8. Export to JSON
    // -------------------------------------------------------------------------
    println!("\n8. Exporting to JSON...\n");

    let json = report.to_json()?;
    println!("JSON export (first 500 chars):");
    println!("{}", &json[..json.len().min(500)]);
    println!("...\n");

    println!("JSON can be saved to a file for:");
    println!("  - CI/CD integration");
    println!("  - Historical tracking");
    println!("  - Dashboard visualization");
    println!("  - Regression analysis\n");

    // -------------------------------------------------------------------------
    // 9. Turn-level results
    // -------------------------------------------------------------------------
    println!("9. Turn-level details (for multi-turn conversations)...\n");

    let turn_results = vec![
        TurnResult {
            invocation_id: "turn_1".to_string(),
            actual_response: Some("The weather in NYC is 72°F.".to_string()),
            expected_response: Some("The weather in NYC is 72°F and sunny.".to_string()),
            actual_tool_calls: vec![
                ToolUse::new("get_weather").with_args(serde_json::json!({"location": "NYC"})),
            ],
            expected_tool_calls: vec![
                ToolUse::new("get_weather").with_args(serde_json::json!({"location": "NYC"})),
            ],
            scores: HashMap::from([
                ("tool_trajectory".to_string(), 1.0),
                ("response_similarity".to_string(), 0.85),
            ]),
        },
        TurnResult {
            invocation_id: "turn_2".to_string(),
            actual_response: Some("Tomorrow looks rainy.".to_string()),
            expected_response: Some("Tomorrow's forecast shows rain.".to_string()),
            actual_tool_calls: vec![
                ToolUse::new("get_forecast")
                    .with_args(serde_json::json!({"location": "NYC", "days": 1})),
            ],
            expected_tool_calls: vec![
                ToolUse::new("get_forecast")
                    .with_args(serde_json::json!({"location": "NYC", "days": 1})),
            ],
            scores: HashMap::from([
                ("tool_trajectory".to_string(), 1.0),
                ("response_similarity".to_string(), 0.78),
            ]),
        },
    ];

    let result_with_turns = EvaluationResult::passed(
        "test_multi_turn",
        HashMap::from([
            ("tool_trajectory".to_string(), 1.0),
            ("response_similarity".to_string(), 0.815),
        ]),
        Duration::from_millis(300),
    )
    .with_turn_results(turn_results);

    println!("Multi-turn test: {}", result_with_turns.eval_id);
    println!("  Turns: {}", result_with_turns.turn_results.len());
    for turn in &result_with_turns.turn_results {
        println!("\n  Turn {}:", turn.invocation_id);
        println!("    Expected: {:?}", turn.expected_response.as_deref().unwrap_or("N/A"));
        println!("    Actual: {:?}", turn.actual_response.as_deref().unwrap_or("N/A"));
        println!(
            "    Tools expected: {:?}",
            turn.expected_tool_calls.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        println!(
            "    Tools actual: {:?}",
            turn.actual_tool_calls.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    // -------------------------------------------------------------------------
    // 10. Integration with assert
    // -------------------------------------------------------------------------
    println!("\n\n10. Integration with tests...\n");

    println!(
        r#"
// In your test file
#[tokio::test]
async fn test_my_agent() {{
    let agent = create_my_agent().unwrap();
    let evaluator = Evaluator::new(config);

    let report = evaluator
        .evaluate_file(agent, "tests/my_agent.test.json")
        .await
        .unwrap();

    // Assert with helpful failure message
    assert!(
        report.all_passed(),
        "Evaluation failed:\n{{}}",
        report.format_summary()
    );
}}
"#
    );

    println!("\n=== Example Complete ===");
    println!("\nKey takeaways:");
    println!("  - EvaluationReport contains all results and summary");
    println!("  - Use format_summary() for human-readable output");
    println!("  - Use to_json() for machine-readable export");
    println!("  - failures() returns only failed test results");
    println!("  - all_passed() for quick pass/fail check");
    println!("  - Turn results provide conversation-level detail");

    Ok(())
}
