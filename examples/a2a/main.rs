// A2A (Agent-to-Agent) Protocol Example
//
// This example demonstrates the A2A protocol integration
// for agent-to-agent communication

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_server::a2a::build_agent_card;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let agent = LlmAgentBuilder::new("weather_agent")
        .description("Agent to answer questions about weather")
        .instruction("Answer questions about weather in cities")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    let agent_card = build_agent_card(&agent, "http://localhost:8081");
    
    println!("A2A Protocol Example");
    println!("====================\n");
    println!("✅ Agent Card Generated:");
    println!("   Name: {}", agent_card.name);
    println!("   URL: {}", agent_card.url);
    println!("   Skills: {} available", agent_card.skills.len());
    
    println!("\nA2A Integration Pattern:");
    println!("1. Create agent with tools");
    println!("2. Build agent card with build_agent_card()");
    println!("3. Create A2A Executor with RunnerConfig");
    println!("4. Expose via HTTP with A2A protocol handlers");
    println!("5. Remote agents can discover and invoke");
    
    println!("\nNote: Full A2A server setup requires HTTP handler integration.");
    println!("See server.rs for REST API example.");

    Ok(())
}
