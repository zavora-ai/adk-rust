# Video Avatar Example

Demonstrates the **video avatar configuration API** from ADK-Rust v0.7.0.

Video avatars attach a visual avatar to a realtime voice agent. The avatar's lip movements synchronize with the agent's speech output, providing a visual presence for voice interactions.

## What This Shows

- Building an `AvatarConfig` with source URL, lip-sync settings, and rendering parameters
- Serializing the configuration to JSON (the payload sent to a realtime provider)
- Attaching avatar config to a `RealtimeAgentBuilder` via the `.avatar()` method
- Graceful fallback behavior when the provider doesn't support video avatars
- Minimal configuration with only a source URL

## Configuration Types

| Type | Description |
|------|-------------|
| `AvatarConfig` | Top-level config: source URL, optional lip-sync, optional rendering |
| `LipSyncConfig` | Lip-sync toggle and sync mode (e.g., `"viseme"`) |
| `RenderingConfig` | Output resolution (e.g., `"720p"`) and frame rate |

### Builder Pattern

```rust
use adk_realtime::avatar::{AvatarConfig, LipSyncConfig, RenderingConfig};
use adk_realtime::RealtimeAgentBuilder;

let config = AvatarConfig {
    source_url: "https://example.com/avatar.mp4".to_string(),
    lip_sync: Some(LipSyncConfig {
        enabled: true,
        sync_mode: Some("viseme".to_string()),
    }),
    rendering: Some(RenderingConfig {
        resolution: Some("720p".to_string()),
        frame_rate: Some(30),
    }),
};

let builder = RealtimeAgentBuilder::new("my_agent")
    .avatar(config)
    .instruction("You are a helpful assistant.")
    .voice("alloy");
```

## Prerequisites

- Rust 1.85.0+
- No API keys or LLM provider required

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `RUST_LOG` | No | Log level filter (default: `info`) |

## Run

```bash
cargo run --manifest-path examples/video_avatar/Cargo.toml
```

## Feature Flag

The video avatar API requires the `video-avatar` feature on `adk-realtime`:

```toml
[dependencies]
adk-realtime = { version = "...", features = ["video-avatar"] }
```

## Current Status

No realtime provider currently supports video avatars natively. When an agent with avatar config connects, `adk-realtime` logs a warning and proceeds in audio-only mode. The avatar configuration is preserved in the session's `extra` field, ready for future provider implementations.
