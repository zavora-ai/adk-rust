//! HeyGen LiveAvatar integration example.
//!
//! Demonstrates creating a realtime voice agent with a HeyGen video avatar.
//! The agent's audio output is routed through HeyGen for lip-synced video
//! rendering via LiveKit.
//!
//! Requires:
//! - `HEYGEN_API_KEY` environment variable
//! - `OPENAI_API_KEY` or `GEMINI_API_KEY` for the realtime model
//!
//! ```bash
//! HEYGEN_API_KEY=... cargo run -p adk-realtime --features heygen-avatar,openai --example heygen_avatar
//! ```

use std::sync::Arc;

use adk_realtime::avatar::heygen::{HeyGenConfig, HeyGenProvider, HeyGenQuality};
use adk_realtime::avatar::{AvatarConfig, AvatarProviderKind, LipSyncConfig};

fn main() {
    // This example demonstrates the configuration API only.
    // A full working example requires a realtime model connection.

    println!("=== HeyGen LiveAvatar Configuration Example ===\n");

    // Step 1: Create the HeyGen provider
    let api_key = std::env::var("HEYGEN_API_KEY").unwrap_or_else(|_| "demo-key".to_string());

    let provider = Arc::new(HeyGenProvider::new(
        HeyGenConfig::new(&api_key)
            .with_quality(HeyGenQuality::High)
            .with_push_to_talk(false)
            .with_idle_timeout(300),
    ));

    println!("Provider: {:?}", provider);

    // Step 2: Create the avatar configuration
    let avatar_config = AvatarConfig {
        source_url: "avatar_id_from_heygen_dashboard".to_string(),
        lip_sync: Some(LipSyncConfig { enabled: true, sync_mode: Some("viseme".to_string()) }),
        rendering: None,
        provider: Some(AvatarProviderKind::HeyGen),
    };

    println!("Avatar config: {}", serde_json::to_string_pretty(&avatar_config).unwrap());

    // Step 3: Show how to wire into RealtimeAgentBuilder
    println!("\n// Usage with RealtimeAgentBuilder:");
    println!("// let agent = RealtimeAgentBuilder::new(\"assistant\")");
    println!("//     .model(model)");
    println!("//     .avatar(avatar_config)");
    println!("//     .avatar_provider(provider)");
    println!("//     .build()?;");

    println!("\n✅ HeyGen avatar configuration complete.");
    println!("   Audio flow: Agent TTS → HeyGen LiveKit → Lip-synced video → Client");
}
