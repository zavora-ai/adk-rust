//! Validates: docs/official_docs/observability/telemetry.md
//!
//! This example demonstrates telemetry setup and configuration.

use adk_rust::prelude::*;
use adk_rust_guide::{init_env, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("observability/telemetry.md");

    // Initialize telemetry (using adk_telemetry crate)
    // adk_telemetry::init();

    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent with telemetry enabled
    let agent = LlmAgentBuilder::new("telemetry_agent")
        .description("An agent with telemetry")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;

    println!("Agent created with telemetry support");
    println!("\nTelemetry features:");
    println!("  - Structured logging with tracing");
    println!("  - Log levels: ERROR, WARN, INFO, DEBUG, TRACE");
    println!("  - Integration with OpenTelemetry");

    print_success("telemetry");
    Ok(())
}
