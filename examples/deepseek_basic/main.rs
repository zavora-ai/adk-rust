//! Basic DeepSeek example with ADK.
//!
//! This example demonstrates using DeepSeek's chat model with the ADK agent framework.
//!
//! Set DEEPSEEK_API_KEY environment variable before running:
//! ```bash
//! export DEEPSEEK_API_KEY=sk-...
//! cargo run --example deepseek_basic --features deepseek
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::Content;
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Get API key from environment
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    // Create DeepSeek client (deepseek-chat model)
    let model = DeepSeekClient::new(DeepSeekConfig::chat(api_key))?;

    // Build agent
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .instruction("You are a helpful assistant. Be concise in your responses.")
        .build()?;

    // Create session service
    let session_service = Arc::new(InMemorySessionService::new());

    // Create a session
    let session = session_service
        .create(CreateRequest {
            app_name: "deepseek_basic".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: std::collections::HashMap::new(),
        })
        .await?;

    let session_id = session.id().to_string();

    // Create runner
    let runner = Runner::new(RunnerConfig {
        app_name: "deepseek_basic".to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?;

    // Create user message
    let user_content = Content::new("user").with_text("What is the capital of France?");

    // Run agent and stream response
    println!("User: What is the capital of France?\n");
    print!("Assistant: ");

    let mut stream = runner.run("user_1".to_string(), session_id, user_content).await?;

    while let Some(event) = stream.next().await {
        match event {
            Ok(e) => {
                if let Some(content) = e.llm_response.content {
                    for part in content.parts {
                        if let adk_core::Part::Text { text } = part {
                            print!("{}", text);
                        }
                    }
                }
            }
            Err(e) => eprintln!("Error: {}", e),
        }
    }

    println!("\n");
    Ok(())
}
