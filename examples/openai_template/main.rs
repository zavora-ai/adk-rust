//! OpenAI Template example with ADK.
//!
//! This example demonstrates using OpenAI with dynamic instruction templates
//! that are populated from session state.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_template --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    // Define the agent with dynamic instructions using template placeholders
    // The placeholders {user:name}, {user:language}, etc. will be replaced
    // by the values from the session state at runtime.
    let agent = LlmAgentBuilder::new("multilingual_assistant")
        .description("An assistant that adapts to user preferences")
        .instruction(
            "You are assisting {user:name} who prefers {user:language}. \
             Respond in {user:language}. \
             User expertise level: {user:expertise}. \
             Adjust your explanations accordingly. Be concise.",
        )
        .model(Arc::new(model))
        .build()?;

    println!("Multilingual OpenAI agent created: {}", agent.name());

    let app_name = "openai_template_app";
    let user_id = "user_123";

    let session_service = Arc::new(InMemorySessionService::new());

    // Prepare initial state with user preferences
    let mut state = HashMap::new();
    state.insert("user:name".to_string(), "Alice".into());
    state.insert("user:language".to_string(), "French".into());
    state.insert("user:expertise".to_string(), "intermediate".into());

    // Create session with initial state
    let session = session_service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: None,
            state,
        })
        .await?;

    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: app_name.to_string(),
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

    println!("\nSession configured with:");
    println!("  - Name: Alice");
    println!("  - Language: French");
    println!("  - Expertise: intermediate");
    println!("\nType your questions (or 'exit' to quit).\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let input = line?;
        if input.trim().eq_ignore_ascii_case("exit") {
            break;
        }
        if input.trim().is_empty() {
            continue;
        }

        let content = Content::new("user").with_text(&input);
        let mut events = runner.run(user_id.to_string(), session_id.clone(), content).await?;

        print!("Assistant: ");
        stdout.flush()?;

        while let Some(event) = events.next().await {
            match event {
                Ok(e) => {
                    if let Some(content) = e.llm_response.content {
                        for part in content.parts {
                            if let adk_core::Part::Text { text } = part {
                                print!("{}", text);
                                stdout.flush()?;
                            }
                        }
                    }
                }
                Err(e) => eprintln!("\nError: {}", e),
            }
        }
        println!("\n");
    }

    Ok(())
}
