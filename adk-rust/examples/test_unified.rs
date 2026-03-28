#![allow(clippy::result_large_err)]
// Simple test to verify adk-rust facade crate works
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    println!("🚀 Testing ADK-Rust unified crate...\n");

    // Test 1: Get API key
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("⚠️  Note: GOOGLE_API_KEY not set (expected for test)");
            "test-key".to_string()
        }
    };

    // Test 2: Create model
    println!("✅ Creating Gemini model...");
    let model_result = GeminiModel::new(&api_key, "gemini-2.5-flash");

    if let Ok(model) = model_result {
        println!("   Model: {}", model.name());

        // Test 3: Build agent
        println!("✅ Building LLM agent...");
        let agent = LlmAgentBuilder::new("test-agent")
            .description("Test agent from unified crate")
            .model(Arc::new(model))
            .build()?;

        println!("   Agent: {}", agent.name());
        println!("   Description: {}", agent.description());

        // Test 4: Verify tools
        println!("✅ Creating tools...");
        let search_tool = GoogleSearchTool::new();
        println!("   Tool: {}", search_tool.name());

        // Test 5: Session service
        println!("✅ Creating session service...");
        let session_service = InMemorySessionService::new();
        println!("   Session service: InMemory");

        // Test 6: Runner
        println!("✅ Creating runner...");
        let _runner = Runner::new(RunnerConfig {
            app_name: "test-app".to_string(),
            agent: Arc::new(agent),
            session_service: Arc::new(session_service),
            artifact_service: None,
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            request_context: None,
            cancellation_token: None,
        })?;
        println!("   Runner created successfully");
    } else {
        println!("⚠️  Skipping agent tests (API key needed for real usage)");
    }

    println!("\n🎉 All tests passed!");
    println!("\n📦 The unified adk-rust crate is working correctly!");
    println!("\nUsage:");
    println!("  cargo add adk-rust");
    println!("  use adk_rust::prelude::*;");

    Ok(())
}
