//! Validates: docs/official_docs/deployment/server.md
//!
//! This example demonstrates running an agent as an HTTP server.

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("deployment/server.md");

    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent
    let agent = LlmAgentBuilder::new("server_agent")
        .description("An agent for HTTP server deployment")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;

    // Create launcher
    let launcher = Launcher::new(Arc::new(agent));

    println!("Launcher created for server mode");
    println!("Server endpoints:");
    println!("  POST /run_sse - Run agent with SSE streaming");
    println!("  GET /sessions - List sessions");
    println!("  POST /sessions - Create session");
    println!("To start server: launcher.serve(8080).await");

    // Note: We don't actually start the server in validation
    // as it would block

    print_success("server_mode");
    Ok(())
}
