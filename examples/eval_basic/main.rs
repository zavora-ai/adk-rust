//! Basic Evaluation Example
//!
//! This example demonstrates how to create and load test definition files
//! for agent evaluation. Test files use the `.test.json` format.
//!
//! Run with: cargo run --example eval_basic

use adk_eval::schema::ContentData;
use adk_eval::{EvalCase, IntermediateData, TestFile, ToolUse, Turn};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Test Definition Basics ===\n");

    // -------------------------------------------------------------------------
    // 1. Create a test file programmatically
    // -------------------------------------------------------------------------
    println!("1. Creating a test file programmatically...\n");

    let test_file = TestFile {
        eval_set_id: "weather_agent_tests".to_string(),
        name: "Weather Agent Test Suite".to_string(),
        description: "Tests for the weather lookup agent".to_string(),
        eval_cases: vec![
            // Test Case 1: Simple weather query
            EvalCase {
                eval_id: "test_simple_weather".to_string(),
                description: "User asks for current weather".to_string(),
                conversation: vec![Turn {
                    invocation_id: "turn_1".to_string(),
                    user_content: ContentData::text("What's the weather in San Francisco?"),
                    final_response: Some(ContentData::model_response(
                        "The weather in San Francisco is 65°F and partly cloudy.",
                    )),
                    intermediate_data: Some(IntermediateData {
                        tool_uses: vec![ToolUse::new("get_weather")
                            .with_args(json!({"location": "San Francisco"}))],
                        ..Default::default()
                    }),
                }],
                session_input: Default::default(),
                tags: vec!["weather".to_string(), "basic".to_string()],
            },
            // Test Case 2: Multi-turn conversation
            EvalCase {
                eval_id: "test_followup_question".to_string(),
                description: "User asks follow-up question".to_string(),
                conversation: vec![
                    Turn {
                        invocation_id: "turn_1".to_string(),
                        user_content: ContentData::text("What's the weather in NYC?"),
                        final_response: Some(ContentData::model_response(
                            "The weather in New York City is 72°F and sunny.",
                        )),
                        intermediate_data: Some(IntermediateData {
                            tool_uses: vec![
                                ToolUse::new("get_weather").with_args(json!({"location": "NYC"}))
                            ],
                            ..Default::default()
                        }),
                    },
                    Turn {
                        invocation_id: "turn_2".to_string(),
                        user_content: ContentData::text("Will it rain tomorrow?"),
                        final_response: Some(ContentData::model_response(
                            "Tomorrow's forecast for NYC shows a 30% chance of rain.",
                        )),
                        intermediate_data: Some(IntermediateData {
                            tool_uses: vec![ToolUse::new("get_forecast")
                                .with_args(json!({"location": "NYC", "days": 1}))],
                            ..Default::default()
                        }),
                    },
                ],
                session_input: Default::default(),
                tags: vec!["weather".to_string(), "multi-turn".to_string()],
            },
        ],
    };

    println!("Created test file: {}", test_file.name);
    println!("  ID: {}", test_file.eval_set_id);
    println!("  Test cases: {}", test_file.eval_cases.len());

    // -------------------------------------------------------------------------
    // 2. Serialize to JSON (for saving to file)
    // -------------------------------------------------------------------------
    println!("\n2. Serializing to JSON format...\n");

    let json_output = serde_json::to_string_pretty(&test_file)?;
    println!("JSON output (first 500 chars):");
    println!("{}", &json_output[..json_output.len().min(500)]);
    println!("...\n");

    // -------------------------------------------------------------------------
    // 3. Parse JSON back to TestFile
    // -------------------------------------------------------------------------
    println!("3. Parsing JSON back to TestFile...\n");

    let parsed: TestFile = serde_json::from_str(&json_output)?;
    println!("Parsed successfully!");
    println!("  Name: {}", parsed.name);
    println!("  Cases: {}", parsed.eval_cases.len());

    // -------------------------------------------------------------------------
    // 4. Examine test case structure
    // -------------------------------------------------------------------------
    println!("\n4. Examining test case structure...\n");

    for case in &parsed.eval_cases {
        println!("Test Case: {} ({})", case.eval_id, case.description);
        println!("  Tags: {:?}", case.tags);
        println!("  Turns: {}", case.conversation.len());

        for turn in &case.conversation {
            println!("    Turn {}", turn.invocation_id);
            println!("      User: {}", turn.user_content.get_text());
            if let Some(resp) = &turn.final_response {
                println!("      Expected: {}", resp.get_text());
            }
            if let Some(data) = &turn.intermediate_data {
                println!(
                    "      Expected tools: {:?}",
                    data.tool_uses.iter().map(|t| &t.name).collect::<Vec<_>>()
                );
            }
        }
        println!();
    }

    // -------------------------------------------------------------------------
    // 5. Create minimal test case JSON
    // -------------------------------------------------------------------------
    println!("5. Minimal test case JSON example:\n");

    let minimal_json = r#"{
  "eval_set_id": "minimal_test",
  "name": "Minimal Test",
  "description": "A minimal test example",
  "eval_cases": [
    {
      "eval_id": "test_1",
      "conversation": [
        {
          "invocation_id": "inv_1",
          "user_content": {
            "parts": [{"text": "Hello!"}],
            "role": "user"
          },
          "final_response": {
            "parts": [{"text": "Hi there! How can I help you?"}],
            "role": "model"
          }
        }
      ]
    }
  ]
}"#;

    println!("{}", minimal_json);

    let minimal: TestFile = serde_json::from_str(minimal_json)?;
    println!("\nParsed minimal test: {} cases", minimal.eval_cases.len());

    println!("\n=== Example Complete ===");
    println!("\nKey takeaways:");
    println!("  - Test files use .test.json extension");
    println!("  - Each file contains multiple eval_cases");
    println!("  - Each case has a conversation with turns");
    println!("  - Turns specify user input, expected response, and expected tools");

    Ok(())
}
