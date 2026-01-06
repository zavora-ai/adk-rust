//! Basic evaluation doc-test - validates test file creation and parsing
//! from evaluation.md documentation

use adk_eval::{EvalCase, IntermediateData, TestFile, ToolUse, Turn};
use adk_eval::schema::ContentData;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Evaluation Doc-Test: Basic ===\n");

    // Test file format from documentation
    let test_json = r#"{
      "eval_set_id": "weather_agent_tests",
      "name": "Weather Agent Tests",
      "description": "Test weather agent functionality",
      "eval_cases": [
        {
          "eval_id": "test_current_weather",
          "conversation": [
            {
              "invocation_id": "inv_001",
              "user_content": {
                "parts": [{"text": "What's the weather in NYC?"}],
                "role": "user"
              },
              "final_response": {
                "parts": [{"text": "The weather in NYC is 65°F and sunny."}],
                "role": "model"
              },
              "intermediate_data": {
                "tool_uses": [
                  {
                    "name": "get_weather",
                    "args": {"location": "NYC"}
                  }
                ]
              }
            }
          ]
        }
      ]
    }"#;

    // Parse test file
    let test_file: TestFile = serde_json::from_str(test_json)?;
    println!("✓ Parsed test file: {}", test_file.name);
    println!("  eval_set_id: {}", test_file.eval_set_id);
    println!("  cases: {}", test_file.eval_cases.len());

    // Verify structure
    let case = &test_file.eval_cases[0];
    assert_eq!(case.eval_id, "test_current_weather");
    println!("✓ eval_id matches");

    let turn = &case.conversation[0];
    assert_eq!(turn.invocation_id, "inv_001");
    println!("✓ invocation_id matches");

    // Check tool uses
    let tool_uses = turn.intermediate_data.as_ref().unwrap();
    assert_eq!(tool_uses.tool_uses[0].name, "get_weather");
    println!("✓ tool_uses parsed correctly");

    // Create test file programmatically (as shown in docs)
    let programmatic_file = TestFile {
        eval_set_id: "my_tests".to_string(),
        name: "My Test Suite".to_string(),
        description: "Tests created programmatically".to_string(),
        eval_cases: vec![
            EvalCase {
                eval_id: "test_1".to_string(),
                description: "Simple test".to_string(),
                conversation: vec![
                    Turn {
                        invocation_id: "turn_1".to_string(),
                        user_content: ContentData::text("Hello"),
                        final_response: Some(ContentData::model_response("Hi there!")),
                        intermediate_data: Some(IntermediateData {
                            tool_uses: vec![
                                ToolUse::new("greet").with_args(json!({"name": "user"}))
                            ],
                            ..Default::default()
                        }),
                    }
                ],
                session_input: Default::default(),
                tags: vec!["basic".to_string()],
            }
        ],
    };

    // Serialize and verify round-trip
    let json_output = serde_json::to_string_pretty(&programmatic_file)?;
    let _parsed_back: TestFile = serde_json::from_str(&json_output)?;
    println!("✓ Programmatic creation and round-trip works");

    println!("\n=== All basic tests passed! ===");
    Ok(())
}
