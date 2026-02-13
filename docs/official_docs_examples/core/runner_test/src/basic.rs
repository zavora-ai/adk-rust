//! Runner doc-test - validates runner.md documentation

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_core::{Content, RunConfig, StreamingMode};
use adk_model::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::InMemorySessionService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Runner Doc-Test ===\n");

    // Validate RunConfig structure from docs
    let config = RunConfig::default();
    assert_eq!(config.streaming_mode, StreamingMode::SSE);
    println!("✓ RunConfig::default() uses SSE streaming");

    // Validate StreamingMode variants from docs
    let _none = StreamingMode::None;
    let _sse = StreamingMode::SSE;
    let _bidi = StreamingMode::Bidi;
    println!("✓ StreamingMode variants: None, SSE, Bidi");

    // Validate RunnerConfig structure from docs
    let sessions = Arc::new(InMemorySessionService::new());
    let artifacts = Arc::new(InMemoryArtifactService::new());

    // Create a mock agent (won't actually call LLM)
    let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_else(|_| "test-key".to_string());
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    let agent =
        Arc::new(LlmAgentBuilder::new("test_agent").model(model).instruction("Test").build()?);

    // From docs: RunnerConfig fields
    let config = RunnerConfig {
        app_name: "my_app".to_string(),
        agent: agent.clone(),
        session_service: sessions.clone(),
        artifact_service: Some(artifacts.clone()),
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
    };
    println!("✓ RunnerConfig with all fields");

    // From docs: Runner::new()
    let _runner = Runner::new(config)?;
    println!("✓ Runner::new(config) works");

    // From docs: runner.run() signature
    // Note: We don't actually run it (would need real API key)
    // Just validate the types compile
    let _user_content = Content::new("user").with_text("Hello!");
    println!("✓ Content for runner.run() works");

    // Validate runner.run() compiles with correct signature
    // runner.run(user_id: String, session_id: String, user_content: Content)
    // We can't run without a real API key, but we validated the types

    println!("\n=== All runner tests passed! ===");
    Ok(())
}
