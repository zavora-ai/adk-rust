use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust::serde_json;
use std::sync::Arc;

/// Simple add tool that adds two numbers
struct AddTool;

#[adk_rust::async_trait]
impl Tool for AddTool {
    fn name(&self) -> &str {
        "add"
    }

    fn description(&self) -> &str {
        "Adds two numbers together. Input should be JSON with 'a' and 'b' fields."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "number", "description": "First number" },
                "b": { "type": "number", "description": "Second number" }
            },
            "required": ["a", "b"]
        }))
    }

    async fn execute(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let a: f64 = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b: f64 = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let result = a + b;
        
        // Store calculation in state via EventActions
        let mut actions = ctx.actions();
        actions.state_delta.insert("last_calculation".to_string(), serde_json::json!(result));
        actions.state_delta.insert("calculation_history".to_string(), serde_json::json!(format!("{} + {} = {}", a, b, result)));
        ctx.set_actions(actions);
        
        Ok(serde_json::json!({ "result": result }))
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Build agent with tools
    let agent = LlmAgentBuilder::new("search_assistant")
        .description("An assistant that can search the web and do math")
        .instruction("You are a helpful assistant. Use the search tool for current info and the add tool for arithmetic. When you learn the user's name, store it as 'user:name'. When you complete a calculation, store the result as 'last_calculation'.")
        .model(Arc::new(model))        
        .tool(Arc::new(AddTool))                   // Math capability
        .output_key("agent_response")              // Store responses in state
        .build()?;

    Launcher::new(Arc::new(agent))
        .with_streaming_mode(adk_rust::StreamingMode::None)
        .run().await?;

    Ok(())
}
