//! Input Guardrails Callback Example
//!
//! Demonstrates using before_callback to block inappropriate content.
//!
//! Run:
//!   cd doc-test/callbacks/callbacks_test
//!   GOOGLE_API_KEY=your_key cargo run --bin guardrails

use adk_core::Part;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    println!("Input Guardrails Example");
    println!("========================\n");
    println!("Try typing 'blocked_word' to see the guardrail in action.\n");

    let agent = LlmAgentBuilder::new("guarded_agent")
        .model(model)
        .instruction("You are a helpful assistant. Be brief.")
        // Input guardrail callback
        .before_callback(Box::new(|ctx| {
            Box::pin(async move {
                // Check user input for blocked content
                let user_content = ctx.user_content();
                for part in &user_content.parts {
                    if let Part::Text { text } = part {
                        if text.to_lowercase().contains("blocked_word") {
                            println!("[GUARDRAIL] Blocked content detected!");
                            // Return early with rejection message
                            return Ok(Some(Content {
                                role: "model".to_string(),
                                parts: vec![Part::Text {
                                    text: "I cannot process that request.".to_string(),
                                }],
                            }));
                        }
                    }
                }
                Ok(None) // Continue normal execution
            })
        }))
        .build()?;

    // Run with console
    adk_cli::console::run_console(
        Arc::new(agent),
        "guardrails_demo".to_string(),
        "user".to_string(),
    )
    .await?;

    Ok(())
}
