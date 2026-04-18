//! # Video Avatar Example
//!
//! Demonstrates the video avatar configuration API from ADK-Rust v0.7.0.
//!
//! ## What This Shows
//! - Building an `AvatarConfig` with source URL, lip-sync, and rendering settings
//! - Serializing avatar configuration to JSON (what would be sent to a realtime provider)
//! - Attaching avatar config to a `RealtimeAgentBuilder`
//! - Graceful fallback when the provider doesn't support video avatars
//!
//! ## Prerequisites
//! - No LLM provider or API keys required — this is a pure configuration API demo
//!
//! ## Run
//! ```bash
//! cargo run --manifest-path examples/video_avatar/Cargo.toml
//! ```

use adk_realtime::avatar::{AvatarConfig, LipSyncConfig, RenderingConfig};
use adk_realtime::RealtimeAgentBuilder;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  Video Avatar — ADK-Rust v0.7.0          ║");
    println!("╚══════════════════════════════════════════╝\n");

    // =========================================================================
    // Step 1: Build an AvatarConfig
    // =========================================================================
    // AvatarConfig specifies the visual avatar attached to a realtime voice
    // session. The config is serialized to JSON and included in the session
    // setup payload sent to the realtime provider.

    let avatar_config = AvatarConfig {
        // source_url: URL pointing to an image or video file used as the
        // avatar source. The provider renders this asset with lip-sync
        // driven by the agent's audio output.
        source_url: "https://example.com/avatars/assistant_v2.mp4".to_string(),

        // lip_sync: Optional lip-sync configuration. When enabled, the
        // provider synchronizes the avatar's mouth movements with the
        // generated speech audio.
        lip_sync: Some(LipSyncConfig {
            // enabled: Master toggle for lip-sync processing.
            enabled: true,
            // sync_mode: Algorithm used for synchronization.
            // "viseme" maps phonemes to mouth shapes (visemes).
            sync_mode: Some("viseme".to_string()),
        }),

        // rendering: Optional rendering parameters controlling the output
        // video quality of the avatar stream.
        rendering: Some(RenderingConfig {
            // resolution: Target output resolution for the avatar video.
            // Common values: "480p", "720p", "1080p".
            resolution: Some("720p".to_string()),
            // frame_rate: Target frames per second for the avatar video.
            // 30 fps provides smooth motion for most use cases.
            frame_rate: Some(30),
        }),
    };

    println!("📹 Built AvatarConfig:");
    println!("   Source URL : {}", avatar_config.source_url);
    if let Some(ref lip_sync) = avatar_config.lip_sync {
        println!("   Lip-sync   : enabled={}, mode={:?}", lip_sync.enabled, lip_sync.sync_mode);
    }
    if let Some(ref rendering) = avatar_config.rendering {
        println!(
            "   Rendering  : resolution={:?}, frame_rate={:?}",
            rendering.resolution, rendering.frame_rate
        );
    }
    println!();

    // =========================================================================
    // Step 2: Serialize config to JSON
    // =========================================================================
    // The avatar configuration is serialized to JSON and placed in the
    // session's `extra` field. This is what a realtime provider would
    // receive when establishing the session.

    let json = serde_json::to_string_pretty(&avatar_config)?;
    println!("📄 Avatar config as JSON (sent to realtime provider):");
    println!("{json}\n");

    // Demonstrate round-trip: deserialize back and verify equality
    let deserialized: AvatarConfig = serde_json::from_str(&json)?;
    assert_eq!(avatar_config, deserialized);
    println!("✅ JSON round-trip verified: serialize → deserialize produces identical config\n");

    // =========================================================================
    // Step 3: Attach avatar config to RealtimeAgentBuilder
    // =========================================================================
    // RealtimeAgentBuilder::avatar() attaches the avatar configuration to the
    // agent. When the agent connects to a realtime session, the config is
    // included in the session setup payload.
    //
    // Note: We don't call .build() here because that requires a RealtimeModel
    // (an actual LLM provider connection). This example focuses purely on the
    // configuration API.

    let _builder = RealtimeAgentBuilder::new("avatar_assistant")
        .description("A voice assistant with a video avatar")
        .instruction("You are a friendly assistant with a visual avatar presence.")
        .voice("alloy")
        .avatar(avatar_config.clone());

    println!("🏗️  RealtimeAgentBuilder configured:");
    println!("   Agent name   : avatar_assistant");
    println!("   Description  : A voice assistant with a video avatar");
    println!("   Voice        : alloy");
    println!("   Avatar       : attached (source: {})", avatar_config.source_url);
    println!();

    // =========================================================================
    // Step 4: Demonstrate graceful fallback
    // =========================================================================
    // Currently no realtime provider supports video avatars natively. When an
    // agent with avatar config connects to a provider, adk-realtime logs a
    // warning and proceeds audio-only. The avatar config is still placed in
    // the session's `extra` field so future provider implementations can read it.
    //
    // Here we simulate what the agent would log at connection time:

    println!("⚠️  Graceful fallback demonstration:");
    println!("   When connecting to a realtime provider that doesn't support video avatars,");
    println!("   the agent logs a warning and falls back to audio-only mode:");
    println!();
    println!("   [WARN] video avatar configured but the current realtime provider does not");
    println!("          support video avatars; proceeding audio-only");
    println!("          agent=avatar_assistant source_url={}", avatar_config.source_url);
    println!();
    println!("   The avatar config is preserved in the session's `extra` field as JSON,");
    println!("   ready for providers that add video avatar support in the future.");
    println!();

    // Show the extra field structure that would be set on RealtimeConfig
    let extra = serde_json::json!({
        "avatarConfig": serde_json::to_value(&avatar_config)?
    });
    println!("📦 Session extra field (where avatar config is stored):");
    println!("{}\n", serde_json::to_string_pretty(&extra)?);

    // =========================================================================
    // Step 5: Minimal config (no lip-sync, no rendering)
    // =========================================================================
    // AvatarConfig only requires source_url. Lip-sync and rendering are optional.

    let minimal_config = AvatarConfig {
        source_url: "https://example.com/avatars/simple.png".to_string(),
        lip_sync: None,
        rendering: None,
    };

    let minimal_json = serde_json::to_string_pretty(&minimal_config)?;
    println!("📄 Minimal avatar config (source URL only):");
    println!("{minimal_json}\n");

    // =========================================================================
    // Success Summary
    // =========================================================================
    println!("╔══════════════════════════════════════════╗");
    println!("║  ✅ Video Avatar example complete         ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  Demonstrated:                           ║");
    println!("║  • AvatarConfig construction             ║");
    println!("║  • LipSyncConfig & RenderingConfig       ║");
    println!("║  • JSON serialization round-trip          ║");
    println!("║  • RealtimeAgentBuilder.avatar() API      ║");
    println!("║  • Graceful audio-only fallback           ║");
    println!("║  • Minimal config (source URL only)       ║");
    println!("╚══════════════════════════════════════════╝");

    Ok(())
}
