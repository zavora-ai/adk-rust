//! Validates: docs/official_docs/tools/built-in-tools.md
//!
//! This example demonstrates GoogleSearchTool - a built-in tool for web search
//! via Gemini grounding.
//!
//! Run modes:
//!   cargo run --example built_in_google_search -p adk-rust-guide              # Validation mode
//!   cargo run --example built_in_google_search -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example built_in_google_search -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // GoogleSearchTool - Web search via Gemini grounding
    // Note: GoogleSearchTool is handled internally by Gemini models through grounding
    let search_tool = GoogleSearchTool;

    // Build agent with Google Search capability
    let agent = LlmAgentBuilder::new("research_assistant")
        .description("An assistant that can search the web for current information")
        .instruction(
            "You are a research assistant with access to Google Search. \
             When asked about current events, recent news, factual information, \
             or anything that requires up-to-date data, use the google_search tool \
             to find accurate information. Always cite your sources when possible.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(search_tool))
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode - verify the tool and agent were created correctly
        print_validating("tools/built-in-tools.md");

        println!("\n=== GoogleSearchTool ===");
        println!("Agent name: {}", agent.name());
        println!("Agent description: {}", agent.description());

        // Verify GoogleSearchTool properties
        let tool = GoogleSearchTool;
        assert_eq!(tool.name(), "google_search");
        assert!(!tool.description().is_empty());

        println!("Tool name: {}", tool.name());
        println!("Tool description: {}", tool.description());

        // Verify agent was built successfully
        assert_eq!(agent.name(), "research_assistant");
        assert!(!agent.description().is_empty());

        print_success("built_in_google_search");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example built_in_google_search -p adk-rust-guide -- chat");
        println!("\nTry asking: 'What are the latest news headlines today?'");
    }

    Ok(())
}
