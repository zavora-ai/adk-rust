//! D-ID Realtime Agents integration example.
//!
//! Demonstrates creating a realtime voice agent with a D-ID video avatar.
//! D-ID handles its own LLM and TTS internally — the ADK agent configures
//! the D-ID agent and manages the session lifecycle. Video streams directly
//! from D-ID to the client via WebRTC.
//!
//! Requires:
//! - `DID_API_KEY` environment variable
//! - A D-ID agent ID (created via D-ID dashboard)
//!
//! ```bash
//! DID_API_KEY=... cargo run -p adk-realtime --features did-avatar --example did_avatar
//! ```

use std::sync::Arc;

use adk_realtime::avatar::did::{DIDConfig, DIDLlmConfig, DIDProvider};
use adk_realtime::avatar::{AvatarConfig, AvatarProviderKind, LipSyncConfig};

fn main() {
    // This example demonstrates the configuration API only.
    // A full working example requires a D-ID agent ID and API key.

    println!("=== D-ID Realtime Agents Configuration Example ===\n");

    // Step 1: Create the D-ID provider
    let api_key = std::env::var("DID_API_KEY").unwrap_or_else(|_| "demo-key".to_string());
    let agent_id = std::env::var("DID_AGENT_ID").unwrap_or_else(|_| "agt_example123".to_string());

    let provider = Arc::new(DIDProvider::new(
        DIDConfig::new(&api_key, &agent_id)
            .with_llm_config(DIDLlmConfig {
                provider: "openai".to_string(),
                model: "gpt-4".to_string(),
                instructions: Some("You are a helpful assistant.".to_string()),
            })
            .with_knowledge_id("kb_docs_v2"),
    ));

    println!("Provider: {:?}", provider);

    // Step 2: Create the avatar configuration
    let avatar_config = AvatarConfig {
        source_url: "https://example.com/avatar-photo.jpg".to_string(),
        lip_sync: Some(LipSyncConfig { enabled: true, sync_mode: None }),
        rendering: None,
        provider: Some(AvatarProviderKind::DId),
    };

    println!("Avatar config: {}", serde_json::to_string_pretty(&avatar_config).unwrap());

    // Step 3: Show how to wire into RealtimeAgentBuilder
    println!("\n// Usage with RealtimeAgentBuilder:");
    println!("// let agent = RealtimeAgentBuilder::new(\"assistant\")");
    println!("//     .model(model)");
    println!("//     .avatar(avatar_config)");
    println!("//     .avatar_provider(provider)");
    println!("//     .build()?;");

    println!("\n✅ D-ID avatar configuration complete.");
    println!("   Architecture: D-ID handles LLM + TTS internally.");
    println!("   Video flow: D-ID WebRTC peer → Client (direct connection)");
    println!("   ADK role: session lifecycle management only.");
}
