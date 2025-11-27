use adk_agent::LlmAgentBuilder;
use adk_core::Agent;
use adk_model::gemini::GeminiModel;
use adk_telemetry::{init_telemetry, info};
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize telemetry
    init_telemetry("telemetry-demo")?;
    
    info!("=== ADK Telemetry Demo ===");
    
    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("GEMINI_API_KEY environment variable must be set");

    // Create a simple agent
    let agent = LlmAgentBuilder::new("telemetry_agent")
        .description("Demo agent with telemetry")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .instruction("You are a helpful assistant. Be concise.")
        .build()?;

    // Create a simple test context
    mod test_context;
    let ctx = Arc::new(test_context::TestContext::new("Hello! Tell me a short joke."));

    info!("Running agent with telemetry enabled...");
    
    // Run the agent - all operations will be traced
    let mut stream = agent.run(ctx).await?;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(content) = &event.content {
                    for part in &content.parts {
                        if let adk_core::Part::Text { text } = part {
                            if !text.is_empty() {
                                println!("Response: {}", text);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    info!("Agent execution completed");
    
    // Shutdown telemetry to flush traces
    adk_telemetry::shutdown_telemetry();

    Ok(())
}
