//! OpenAI Responses API — Conversations API example.
//!
//! Demonstrates the full Conversations API lifecycle: create a conversation,
//! send multiple messages with server-managed history, retrieve metadata,
//! and delete the conversation.
//!
//! # Running
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_conversations/Cargo.toml
//! ```

use adk_model::openai::{ConversationsClient, OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::prelude::*;
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("═══════════════════════════════════════════════════");
    println!("  OpenAI Conversations API — Lifecycle Example");
    println!("═══════════════════════════════════════════════════");
    println!();

    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY must be set — see .env.example");

    // Create the Conversations client for lifecycle management
    let conversations = ConversationsClient::new(&api_key, None);

    // Create the Responses client for sending messages
    let config = OpenAIResponsesConfig::new(&api_key, "gpt-4.1-nano");
    let client = OpenAIResponsesClient::new(config)?;

    // ─── Step 1: Create a new conversation ───────────────────────────────
    println!("📝 Creating a new conversation...");
    let conversation_id = conversations.create().await?;
    println!("✅ Created conversation: {conversation_id}");
    println!();

    // ─── Step 2: Send first message with conversation_id ─────────────────
    println!("💬 Sending first message...");
    let mut gen_config = adk_rust::GenerateContentConfig::default();
    gen_config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({
            "conversation_id": conversation_id
        }),
    );

    let request = LlmRequest {
        model: "gpt-4.1-nano".to_string(),
        contents: vec![Content::new("user").with_text(
            "My name is Alice and I'm building a Rust project called Starlight. Remember this.",
        )],
        config: Some(gen_config),
        tools: Default::default(),
        previous_response_id: None,
    };

    let mut stream = client.generate_content(request, false).await?;
    if let Some(response) = stream.next().await {
        let response = response?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    println!("🤖 Response: {text}");
                }
            }
        }
    }
    println!();

    // ─── Step 3: Send follow-up message (server retains history) ─────────
    println!("💬 Sending follow-up message (server manages history)...");
    let mut gen_config = adk_rust::GenerateContentConfig::default();
    gen_config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({
            "conversation_id": conversation_id
        }),
    );

    let request = LlmRequest {
        model: "gpt-4.1-nano".to_string(),
        contents: vec![Content::new("user").with_text(
            "What is my name and what project am I working on?",
        )],
        config: Some(gen_config),
        tools: Default::default(),
        previous_response_id: None,
    };

    let mut stream = client.generate_content(request, false).await?;
    if let Some(response) = stream.next().await {
        let response = response?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    println!("🤖 Response: {text}");
                }
            }
        }
    }
    println!();
    println!("   ↑ The model remembers context without the client re-sending history!");
    println!();

    // ─── Step 4: Retrieve conversation metadata ──────────────────────────
    println!("📋 Retrieving conversation metadata...");
    let metadata = conversations.get(&conversation_id).await?;
    println!("   Metadata: {}", serde_json::to_string_pretty(&metadata)?);
    println!();

    // ─── Step 5: Delete the conversation ─────────────────────────────────
    println!("🗑️  Deleting conversation...");
    conversations.delete(&conversation_id).await?;
    println!("✅ Conversation deleted.");

    println!();
    println!("✅ Example completed successfully.");
    Ok(())
}
