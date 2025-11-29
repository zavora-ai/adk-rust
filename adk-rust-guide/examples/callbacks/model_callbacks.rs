//! Validates: docs/official_docs/callbacks/callbacks.md
//!
//! This example demonstrates before_model and after_model callbacks.
//! Model callbacks intercept LLM requests and responses:
//! - before_model: Can modify requests or return cached responses
//! - after_model: Can modify or filter model responses

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent with model callbacks
    let agent = LlmAgentBuilder::new("model_callback_demo")
        .description("Agent demonstrating model callbacks")
        .instruction("You are a helpful assistant.")
        .model(model)
        // Add before_model callback - executes before LLM call
        .before_model_callback(Box::new(|ctx, request| {
            Box::pin(async move {
                println!("[BEFORE_MODEL] About to call model for agent: {}", ctx.agent_name());
                println!("[BEFORE_MODEL] Request has {} content items", request.contents.len());

                // Return Continue(request) to proceed with the (possibly modified) request
                // Return Skip(response) to skip the model and use a cached response
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        // Add after_model callback - executes after LLM response
        .after_model_callback(Box::new(|ctx, response| {
            Box::pin(async move {
                println!("[AFTER_MODEL] Received response for agent: {}", ctx.agent_name());
                if let Some(ref content) = response.content {
                    println!("[AFTER_MODEL] Response has {} parts", content.parts.len());
                }
                
                // Return None to keep the original response
                // Return Some(LlmResponse) to modify the response
                Ok(Some(response))
            })
        }))
        .build()?;

    if is_interactive_mode() {
        // Run in interactive mode
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        print_validating("callbacks/callbacks.md");
        
        // Validation: verify agent was created with callbacks
        println!("Created agent with model callbacks: {}", agent.name());
        println!("Agent description: {}", agent.description());
        
        print_success("model_callbacks");
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example model_callbacks -p adk-rust-guide -- chat");
    }

    Ok(())
}
