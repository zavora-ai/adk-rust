//! Validates: docs/official_docs/tools/built-in-tools.md
//!
//! This example demonstrates LoadArtifactsTool - a built-in tool for loading
//! artifacts from storage.
//!
//! Run modes:
//!   cargo run --example built_in_load_artifacts -p adk-rust-guide              # Validation mode
//!   cargo run --example built_in_load_artifacts -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example built_in_load_artifacts -p adk-rust-guide -- serve     # Web server mode

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

    // LoadArtifactsTool - Artifact loading from storage
    let load_artifacts_tool = LoadArtifactsTool::new();

    // Build agent with LoadArtifactsTool capability
    let agent = LlmAgentBuilder::new("document_analyzer")
        .description("Analyzes stored documents and artifacts")
        .instruction(
            "You can load and analyze stored artifacts. \
             Use the load_artifacts tool to retrieve documents by name. \
             The tool accepts an array of artifact names.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(load_artifacts_tool))
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode - verify the tool and agent were created correctly
        print_validating("tools/built-in-tools.md");

        println!("\n=== LoadArtifactsTool ===");
        println!("Agent name: {}", agent.name());
        println!("Agent description: {}", agent.description());

        // Verify LoadArtifactsTool properties
        let tool = LoadArtifactsTool::new();
        assert_eq!(tool.name(), "load_artifacts");
        assert!(!tool.description().is_empty());

        println!("Tool name: {}", tool.name());
        println!("Tool description: {}", tool.description());

        // Verify agent was built successfully
        assert_eq!(agent.name(), "document_analyzer");
        assert!(!agent.description().is_empty());

        print_success("built_in_load_artifacts");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example built_in_load_artifacts -p adk-rust-guide -- chat");
        println!("\nNote: LoadArtifactsTool requires an ArtifactService to be configured.");
        println!("In interactive mode, artifacts must be pre-populated in the service.");
    }

    Ok(())
}
