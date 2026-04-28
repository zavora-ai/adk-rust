//! # 05 — Minimal with Memory
//!
//! Memory is included in the minimal tier. This example shows multi-turn
//! conversation with session state preserved across turns.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.0"
//! ```

use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let agent = LlmAgentBuilder::new("memory-agent")
        .instruction("You are a helpful assistant. Remember what the user tells you.")
        .model(model)
        .build()?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "memory-demo".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("memory-demo")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    // Turn 1
    println!("User: My name is Alice and I like Rust.\n");
    let mut stream = runner
        .run_str("user", "s1", Content::new("user").with_text("My name is Alice and I like Rust."))
        .await?;
    print!("Agent: ");
    while let Some(Ok(event)) = stream.next().await {
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!("\n");

    // Turn 2 — agent should remember
    println!("User: What is my name and what do I like?\n");
    let mut stream = runner
        .run_str(
            "user",
            "s1",
            Content::new("user").with_text("What is my name and what do I like?"),
        )
        .await?;
    print!("Agent: ");
    while let Some(Ok(event)) = stream.next().await {
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!("\n");

    println!("✅ Multi-turn memory works with minimal tier.");
    Ok(())
}
