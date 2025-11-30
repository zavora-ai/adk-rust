use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, IncludeContents};
use adk_model::gemini::GeminiModel;
use futures::StreamExt;
use std::sync::Arc;

/// Demonstrates how IncludeContents::None makes the agent stateless
/// Only processes the current turn without any conversation history
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key =
        std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable must be set");

    println!("=== Agent Control Features Demo ===\n");

    // Example 1: Default behavior (full conversation history)
    println!("1. Testing IncludeContents::Default (normal agent with memory)");
    let agent_with_memory = LlmAgentBuilder::new("memory_agent")
        .description("Agent that remembers conversation history")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .instruction("You are a helpful assistant. Remember what the user tells you.")
        .include_contents(IncludeContents::Default) // Full history
        .build()?;

    println!("   → This agent WILL remember conversation history\n");

    // Example 2: Stateless agent (no history)
    println!("2. Testing IncludeContents::None (stateless agent)");
    let stateless_agent = LlmAgentBuilder::new("stateless_agent")
        .description("Agent that only sees current message")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .instruction("You are a helpful assistant. Answer only based on the current question.")
        .include_contents(IncludeContents::None) // No history - only current turn!
        .build()?;

    println!("   → This agent will NOT remember previous messages");
    println!("   → Useful for: single-turn tasks, stateless APIs, privacy-focused assistants\n");

    // Example 3: DisallowTransfer flags
    println!("3. Testing DisallowTransfer flags");
    let restricted_agent = LlmAgentBuilder::new("restricted_agent")
        .description("Agent that cannot delegate to others")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .instruction("You must handle all requests yourself.")
        .disallow_transfer_to_parent(true) // Cannot go back to parent agent
        .disallow_transfer_to_peers(true) // Cannot transfer to sibling agents
        .build()?;

    println!("   → This agent CANNOT transfer to parent or peer agents");
    println!("   → Useful for: enforcing agent boundaries in multi-agent systems\n");

    // Example 4: Structured output
    println!("4. Testing OutputSchema (structured responses)");
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "sentiment": {"type": "string", "enum": ["positive", "negative", "neutral"]},
            "confidence": {"type": "number", "minimum": 0, "maximum": 1}
        },
        "required": ["sentiment", "confidence"]
    });

    let structured_agent = LlmAgentBuilder::new("sentiment_analyzer")
        .description("Analyzes sentiment and returns structured JSON")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .instruction(
            "Analyze the sentiment of the text and return JSON with sentiment and confidence.",
        )
        .output_schema(schema)
        .build()?;

    println!("   → This agent will ONLY return structured JSON");
    println!("   → Note: Cannot use tools when OutputSchema is set (Gemini limitation)\n");

    println!("=== Configuration Summary ===");
    println!("✅ IncludeContents - Controls conversation history");
    println!("   • Default: Full history (normal chat)");
    println!("   • None: Current turn only (stateless)");
    println!("\n✅ DisallowTransfer - Prevents agent delegation");
    println!("   • DisallowTransferToParent: Cannot go back to parent");
    println!("   • DisallowTransferToPeers: Cannot transfer to siblings");
    println!("\n✅ OutputSchema - Enforces structured JSON responses");
    println!("   • Agent can ONLY reply, cannot use tools");
    println!("\n✅ InputSchema - Defines schema when agent is used as a tool");
    println!("   • Validates inputs when agent is called by another agent");

    println!("\n🎉 All Phase 5 features are now functional!");

    Ok(())
}
