//! Shaped Behavior - Demonstrates how instructions shape agent personality
//! 
//! Run with different agent types:
//!   cargo run --bin shaped_behavior -- formal
//!   cargo run --bin shaped_behavior -- tutor
//!   cargo run --bin shaped_behavior -- storyteller

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Get agent type from environment variable or command line args
    let agent_type = std::env::var("AGENT_TYPE")
        .unwrap_or_else(|_| {
            let args: Vec<String> = std::env::args().collect();
            args.get(1).map(|s| s.as_str()).unwrap_or("formal").to_string()
        });

    let agent = match agent_type.as_str() {
        "formal" => {
            println!("🎩 Creating Formal Business Assistant...\n");
            LlmAgentBuilder::new("formal_assistant")
                .instruction("You are a professional business consultant. \
                             Use formal language. Be concise and data-driven.")
                .model(Arc::new(model))
                .build()?
        }
        "tutor" => {
            println!("📚 Creating Friendly Coding Tutor...\n");
            LlmAgentBuilder::new("code_tutor")
                .instruction("You are a friendly coding tutor for beginners. \
                             Explain concepts simply. Use examples. \
                             Encourage questions. Never make the user feel bad for not knowing.")
                .model(Arc::new(model))
                .build()?
        }
        "storyteller" => {
            println!("📖 Creating Creative Storyteller...\n");
            LlmAgentBuilder::new("storyteller")
                .instruction("You are a creative storyteller. \
                             Craft engaging narratives with vivid descriptions. \
                             Use plot twists and memorable characters.")
                .model(Arc::new(model))
                .build()?
        }
        _ => {
            eprintln!("Unknown agent type. Use: formal, tutor, or storyteller");
            std::process::exit(1);
        }
    };

    println!("Try asking: 'What is Rust?' to see different response styles.\n");
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
