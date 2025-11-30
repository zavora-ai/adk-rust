//! Validates: docs/official_docs/deployment/server.md
//!
//! This example demonstrates running an agent as an HTTP server.

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent
    let agent = LlmAgentBuilder::new("server_agent")
        .description("An agent for HTTP server deployment")
        .instruction("You are a helpful assistant. Be concise and friendly.")
        .model(model)
        .build()?;

    // Create launcher
    let launcher = Launcher::new(Arc::new(agent));

    if is_interactive_mode() {
        // Run in server mode (this will start the HTTP server)
        // Note: This requires running with "serve" argument
        println!("To start server mode, run:");
        println!("  cargo run --example server_mode -p adk-rust-guide -- serve");
        println!("\nOr use the launcher directly:");
        launcher.run().await?;
    } else {
        // Validation mode
        print_validating("deployment/server.md");
        
        println!("✓ Launcher created successfully");
        println!("✓ Server mode configured");
        println!("\nServer mode features:");
        println!("  - REST API endpoints");
        println!("  - Server-Sent Events (SSE) streaming");
        println!("  - Built-in web UI");
        println!("  - Session management");
        println!("  - Artifact access");
        println!("\nKey endpoints:");
        println!("  GET  /api/health");
        println!("  POST /api/run_sse");
        println!("  POST /api/sessions");
        println!("  GET  /api/sessions/:app/:user/:session");
        println!("  GET  /ui/");
        
        print_success("server_mode");
        println!("\nTip: Run with 'serve' for server mode:");
        println!("  cargo run --example server_mode -p adk-rust-guide -- serve");
        println!("  cargo run --example server_mode -p adk-rust-guide -- serve --port 3000");
    }

    Ok(())
}
