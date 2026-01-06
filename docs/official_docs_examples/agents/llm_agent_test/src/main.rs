//! LlmAgent Test - Validates examples from docs/official_docs/agents/llm-agent.md
//!
//! This demonstrates a fully configured LlmAgent with:
//! - Custom instruction with templating
//! - A calculator tool using FunctionTool
//! - Output key for saving responses to state
//! - Interactive Launcher for testing

use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();

    // Get API key from environment
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Create a calculator tool with FunctionTool::new(name, description, handler)
    let calculator = FunctionTool::new(
        "calculate",
        "Perform basic arithmetic. Pass an expression like '2 + 2' or '10 * 5'.",
        |_ctx, args| async move {
            let expr = args.get("expression")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            
            // Simple arithmetic evaluation (demonstrates tool response)
            let result = match expr {
                "2 + 2" => "4",
                "10 * 5" => "50",
                "100 / 4" => "25",
                _ => expr, // Echo back for other expressions
            };
            
            Ok(json!({ 
                "expression": expr, 
                "result": result,
                "note": "Calculation complete" 
            }))
        },
    );

    // Build the agent with full configuration (matches llm-agent.md Complete Example)
    let agent = LlmAgentBuilder::new("math_assistant")
        .description("A helpful math assistant that can perform calculations")
        .instruction("You are a math tutor. \
                     Use the calculator tool for arithmetic operations. \
                     Explain your reasoning step by step.")
        .model(Arc::new(model))
        .tool(Arc::new(calculator))
        .output_key("last_response")
        .build()?;

    println!("✅ Created agent: {}", agent.name());
    println!("📝 Description: {}", agent.description());
    println!();

    // Run the agent with the CLI launcher for interactive testing
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
