//! Validates adk-cli README examples compile correctly

use adk_cli::Launcher;
use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_core::StreamingMode;
use std::sync::Arc;

// Validate: Basic Interactive Mode
fn _basic_example() {
    let agent = LlmAgentBuilder::new("test").build().unwrap();
    
    // This is the basic pattern from README
    let _launcher = Launcher::new(Arc::new(agent));
    // .run().await would start the REPL
}

// Validate: Custom Configuration (corrected API)
fn _custom_config_example() {
    let agent = LlmAgentBuilder::new("test").build().unwrap();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    
    // Actual available methods
    let _launcher = Launcher::new(Arc::new(agent))
        .app_name("my_app")
        .with_artifact_service(artifacts)
        .with_streaming_mode(StreamingMode::SSE);
    // .run().await would start based on CLI args
}

fn main() {
    println!("✓ Launcher::new() compiles");
    println!("✓ .app_name() compiles");
    println!("✓ .with_artifact_service() compiles");
    println!("✓ .with_streaming_mode() compiles");
    println!("\nadk-cli README validation passed!");
}
