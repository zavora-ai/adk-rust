//! OpenAI Reasoning Effort Example
//!
//! Demonstrates the new `ReasoningEffort` control for OpenAI reasoning models
//! (o1, o3, etc.). Controls how much reasoning effort the model applies.
//!
//! ```bash
//! export OPENAI_API_KEY=...
//! cargo run --example openai_reasoning --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_model::openai::{OpenAIClient, OpenAIConfig, ReasoningEffort};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    println!("=== OpenAI Reasoning Effort Demo ===\n");

    for effort in [ReasoningEffort::Low, ReasoningEffort::Medium, ReasoningEffort::High] {
        println!("--- Effort: {effort:?} ---");

        let config = OpenAIConfig::new(&api_key, "o3-mini").with_reasoning_effort(effort);
        let model = OpenAIClient::new(config)?;

        let agent = LlmAgentBuilder::new("reasoner")
            .description("Reasoning assistant")
            .instruction("You are a concise math assistant. Answer in one sentence.")
            .model(Arc::new(model))
            .build()?;

        let session_service = Arc::new(InMemorySessionService::new());
        let session = session_service
            .create(CreateRequest {
                app_name: "reasoning_demo".to_string(),
                user_id: "user".to_string(),
                session_id: None,
                state: HashMap::new(),
            })
            .await?;

        let runner = Runner::new(RunnerConfig {
            app_name: "reasoning_demo".to_string(),
            agent: Arc::new(agent),
            session_service,
            artifact_service: None,
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            request_context: None,
            cancellation_token: None,
        })?;

        let content = adk_core::Content::new("user")
            .with_text("What is the sum of the first 50 prime numbers?");

        let mut stream = runner.run("user".to_string(), session.id().to_string(), content).await?;

        while let Some(event) = stream.next().await {
            if let Ok(e) = event {
                if let Some(content) = e.llm_response.content {
                    for part in &content.parts {
                        if let adk_core::Part::Text { text } = part {
                            print!("{text}");
                        }
                    }
                }
            }
        }
        println!("\n");
    }

    Ok(())
}
