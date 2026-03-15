//! Full Agent Evaluation Example
//!
//! This example demonstrates the complete workflow of evaluating an AI agent:
//! 1. Create an agent with tools
//! 2. Define test cases
//! 3. Run the evaluation
//! 4. Analyze the results
//!
//! Run with: cargo run --example eval_agent
//!
//! Requires: GOOGLE_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Tool, ToolContext};
use adk_eval::schema::ToolUse;
use adk_eval::{
    EvaluationConfig, EvaluationCriteria, Evaluator, ToolTrajectoryConfig,
    schema::{ContentData, EvalCase, IntermediateData, TestFile, Turn},
};
use adk_model::GeminiModel;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

// =============================================================================
// Custom Tools for the Weather Agent
// =============================================================================

/// A simple weather lookup tool
struct GetWeatherTool;

#[async_trait]
impl Tool for GetWeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Get the current weather for a location"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city name, e.g., 'San Francisco'"
                }
            },
            "required": ["location"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("Unknown");

        // Simulated weather data
        let weather = match location.to_lowercase().as_str() {
            l if l.contains("san francisco") => json!({
                "location": "San Francisco",
                "temperature": 65,
                "unit": "fahrenheit",
                "condition": "partly cloudy"
            }),
            l if l.contains("new york") || l.contains("nyc") => json!({
                "location": "New York",
                "temperature": 72,
                "unit": "fahrenheit",
                "condition": "sunny"
            }),
            l if l.contains("london") => json!({
                "location": "London",
                "temperature": 55,
                "unit": "fahrenheit",
                "condition": "rainy"
            }),
            _ => json!({
                "location": location,
                "temperature": 70,
                "unit": "fahrenheit",
                "condition": "clear"
            }),
        };

        Ok(weather)
    }
}

/// A forecast tool for future weather
struct GetForecastTool;

#[async_trait]
impl Tool for GetForecastTool {
    fn name(&self) -> &str {
        "get_forecast"
    }

    fn description(&self) -> &str {
        "Get the weather forecast for upcoming days"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city name"
                },
                "days": {
                    "type": "integer",
                    "description": "Number of days to forecast (1-7)"
                }
            },
            "required": ["location"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let days = args.get("days").and_then(|v| v.as_i64()).unwrap_or(1) as usize;

        // Simulated forecast
        let conditions = ["sunny", "partly cloudy", "cloudy", "rainy", "clear"];
        let forecast: Vec<Value> = (0..days.min(7))
            .map(|i| {
                json!({
                    "day": i + 1,
                    "condition": conditions[i % conditions.len()],
                    "high": 70 + (i as i32 * 2),
                    "low": 55 + (i as i32),
                })
            })
            .collect();

        Ok(json!({
            "location": location,
            "forecast": forecast
        }))
    }
}

// =============================================================================
// Main Evaluation
// =============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Full Agent Evaluation ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("❌ GOOGLE_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example eval_agent");
            return Ok(());
        }
    };

    println!("✅ API key loaded\n");

    // -------------------------------------------------------------------------
    // 1. Create the Weather Agent
    // -------------------------------------------------------------------------
    println!("1. Creating weather agent...\n");

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let agent = LlmAgentBuilder::new("weather_agent")
        .model(model.clone())
        .instruction("You are a helpful weather assistant. Use the available tools to look up weather information. Always respond with the temperature and conditions.")
        .tool(Arc::new(GetWeatherTool))
        .tool(Arc::new(GetForecastTool))
        .build()?;

    println!("   Agent: {}", agent.name());
    println!("   Tools: get_weather, get_forecast");
    println!("   Model: gemini-2.5-flash\n");

    // -------------------------------------------------------------------------
    // 2. Define Test Cases
    // -------------------------------------------------------------------------
    println!("2. Defining test cases...\n");

    let test_file = TestFile {
        eval_set_id: "weather_agent_tests".to_string(),
        name: "Weather Agent Evaluation".to_string(),
        description: "Test the weather agent's ability to use tools correctly".to_string(),
        eval_cases: vec![
            // Test 1: Simple weather query
            EvalCase {
                eval_id: "test_simple_weather".to_string(),
                description: "User asks for current weather".to_string(),
                tags: vec!["weather".to_string(), "basic".to_string()],
                conversation: vec![Turn {
                    invocation_id: "turn_1".to_string(),
                    user_content: ContentData::text("What's the weather in San Francisco?"),
                    intermediate_data: Some(IntermediateData {
                        tool_uses: vec![
                            ToolUse::new("get_weather")
                                .with_args(json!({"location": "San Francisco"})),
                        ],
                        intermediate_responses: vec![],
                    }),
                    final_response: Some(ContentData::model_response(
                        "The weather in San Francisco is 65°F and partly cloudy.",
                    )),
                }],
                session_input: Default::default(),
            },
            // Test 2: Forecast query
            EvalCase {
                eval_id: "test_forecast".to_string(),
                description: "User asks for forecast".to_string(),
                tags: vec!["forecast".to_string()],
                conversation: vec![Turn {
                    invocation_id: "turn_1".to_string(),
                    user_content: ContentData::text("What's the 3-day forecast for NYC?"),
                    intermediate_data: Some(IntermediateData {
                        tool_uses: vec![
                            ToolUse::new("get_forecast")
                                .with_args(json!({"location": "NYC", "days": 3})),
                        ],
                        intermediate_responses: vec![],
                    }),
                    final_response: Some(ContentData::model_response(
                        "The forecast for NYC shows sunny weather for the next 3 days.",
                    )),
                }],
                session_input: Default::default(),
            },
            // Test 3: Location not recognized
            EvalCase {
                eval_id: "test_unknown_location".to_string(),
                description: "User asks for unknown location".to_string(),
                tags: vec!["edge-case".to_string()],
                conversation: vec![Turn {
                    invocation_id: "turn_1".to_string(),
                    user_content: ContentData::text("What's the weather in Atlantis?"),
                    intermediate_data: Some(IntermediateData {
                        tool_uses: vec![
                            ToolUse::new("get_weather").with_args(json!({"location": "Atlantis"})),
                        ],
                        intermediate_responses: vec![],
                    }),
                    final_response: Some(ContentData::model_response(
                        "The weather in Atlantis is 70°F and clear.",
                    )),
                }],
                session_input: Default::default(),
            },
        ],
    };

    println!("   Test cases defined: {}", test_file.eval_cases.len());
    for case in &test_file.eval_cases {
        println!("   - {}: {}", case.eval_id, case.description);
    }
    println!();

    // -------------------------------------------------------------------------
    // 3. Configure Evaluation Criteria
    // -------------------------------------------------------------------------
    println!("3. Configuring evaluation criteria...\n");

    let criteria = EvaluationCriteria {
        // Tool usage evaluation
        tool_trajectory_score: Some(0.8), // 80% tool match required
        tool_trajectory_config: Some(ToolTrajectoryConfig {
            strict_order: false, // Order doesn't matter
            strict_args: false,  // Extra args allowed
        }),
        // Response similarity (basic text matching)
        response_similarity: Some(0.5), // 50% similarity required
        ..Default::default()
    };

    println!("   Tool trajectory threshold: 80%");
    println!("   Response similarity threshold: 50%");
    println!("   Strict order: false");
    println!("   Strict args: false\n");

    // -------------------------------------------------------------------------
    // 4. Create Evaluator
    // -------------------------------------------------------------------------
    println!("4. Creating evaluator...\n");

    let config = EvaluationConfig::with_criteria(criteria);
    let evaluator = Evaluator::new(config);

    println!("   Evaluator created (text-based scoring)\n");

    // -------------------------------------------------------------------------
    // 5. Run Evaluation
    // -------------------------------------------------------------------------
    println!("5. Running evaluation...\n");

    let report = evaluator.evaluate_test_file(Arc::new(agent), &test_file).await?;

    // -------------------------------------------------------------------------
    // 6. Display Results
    // -------------------------------------------------------------------------
    println!("6. Evaluation Results\n");
    println!("{}", report.format_summary());

    // Show detailed results
    println!("\nDetailed Results:\n");
    for result in &report.results {
        let status = if result.passed { "✅ PASS" } else { "❌ FAIL" };
        println!("  {} - {}", status, result.eval_id);
        println!("     Duration: {:?}", result.duration);

        if !result.scores.is_empty() {
            println!("     Scores:");
            for (criterion, score) in &result.scores {
                println!("       - {}: {:.0}%", criterion, score * 100.0);
            }
        }

        if !result.failures.is_empty() {
            println!("     Failures:");
            for failure in &result.failures {
                println!(
                    "       - {}: {:.0}% < {:.0}%",
                    failure.criterion,
                    failure.score * 100.0,
                    failure.threshold * 100.0
                );
                if let Some(details) = &failure.details {
                    println!("         {}", details);
                }
            }
        }
        println!();
    }

    // -------------------------------------------------------------------------
    // 7. Export to JSON
    // -------------------------------------------------------------------------
    println!("7. JSON Export (truncated):\n");
    let json = report.to_json()?;
    println!("{}\n", &json[..json.len().min(600)]);

    // -------------------------------------------------------------------------
    // Summary
    // -------------------------------------------------------------------------
    println!("=== Example Complete ===\n");
    println!("Key takeaways:");
    println!("  - Define test cases with expected tool calls and responses");
    println!("  - Configure criteria thresholds based on your needs");
    println!("  - Run evaluation against your actual agent");
    println!("  - Review detailed results and failures");
    println!("  - Export to JSON for CI/CD integration");

    if report.all_passed() {
        println!("\n🎉 All tests passed!");
    } else {
        println!("\n⚠️  Some tests failed. Review the failures above.");
    }

    Ok(())
}
