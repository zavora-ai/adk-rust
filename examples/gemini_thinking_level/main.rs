//! Gemini 3 Thinking Level Example
//!
//! Demonstrates the new level-based thinking control for Gemini 3 models.
//! Gemini 3 uses discrete levels instead of token budgets.
//!
//! ```bash
//! export GOOGLE_API_KEY=...
//! cargo run --example gemini_thinking_level
//! ```

use adk_gemini::{Gemini, ThinkingLevel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Use Gemini 3 Flash which supports thinking_level
    let client = Gemini::with_model(&api_key, "models/gemini-3-flash-preview".to_string())?;

    println!("=== Gemini 3 Thinking Level Demo ===\n");

    // Level-based thinking (Gemini 3 Flash supports all levels)
    for level in
        [ThinkingLevel::Minimal, ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High]
    {
        println!("--- Level: {level:?} ---");
        let response = client
            .generate_content()
            .with_user_message("What is 127 * 83? Show your work briefly.")
            .with_thinking_level(level)
            .with_thoughts_included(true)
            .execute()
            .await?;

        for thought in response.thoughts() {
            let preview = if thought.len() > 120 {
                format!("{}...", &thought[..120])
            } else {
                thought.to_string()
            };
            println!("  💭 {preview}");
        }
        println!("  📝 {}", response.text());

        if let Some(usage) = &response.usage_metadata {
            if let Some(thinking) = usage.thoughts_token_count {
                println!("  🔢 thinking tokens: {thinking}");
            }
        }
        println!();
    }

    Ok(())
}
