# adk-enterprise

Native Rust SDK for the ADK-Rust Enterprise Managed Agent Service.

[![Crates.io](https://img.shields.io/crates/v/adk-enterprise.svg)](https://crates.io/crates/adk-enterprise)
[![Documentation](https://docs.rs/adk-enterprise/badge.svg)](https://docs.rs/adk-enterprise)
[![License](https://img.shields.io/crates/l/adk-enterprise.svg)](LICENSE)

## Overview

`adk-enterprise` is the developer-facing client SDK for the ADK-Rust Enterprise Managed Agent Service. It provides a typed, ergonomic interface for creating agents, managing sessions, sending messages, and streaming real-time responses over HTTP/SSE.

**Zero dependency** on `adk-model`, `adk-runner`, `adk-agent`, or any heavy runtime crate — only HTTP + SSE. Fast compile, small binary, deployable anywhere.

## Key Capabilities

- **Any model** — Gemini, OpenAI, Anthropic, DeepSeek, Ollama, or any OpenAI-compatible endpoint via `ModelRef`
- **Auto-reconnect SSE** — tracks `seq` via SSE `id:` field, reconnects transparently with `Last-Event-ID`
- **Automatic retry** — exponential backoff with jitter on 429/5xx, respects `Retry-After` headers
- **Idempotency** — UUID v4 keys on all create operations, reused across retries
- **Self-hosted support** — same SDK works against any deployment URL
- **Forward-compatible** — unknown event types deserialize to `SessionEvent::Unknown`
- **Clone + Send + Sync** — share the client across async tasks without `Arc`
- **Typed errors** — `EnterpriseError` variants for every failure mode with `is_retryable()`
- **Beta features** — Vault/Credential management and Memory stores

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────┐
│  Your Application                                                    │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │ adk-enterprise (this crate)                                     │ │
│  │ EnterpriseClient → HTTP/SSE → typed SessionEvent stream         │ │
│  └──────────────────────────────────┬─────────────────────────────┘ │
└─────────────────────────────────────┼───────────────────────────────┘
                                      │ HTTPS + SSE
                                      ▼
┌─────────────────────────────────────────────────────────────────────┐
│  Platform: https://enterprise.adk-rust.com/managed/v1               │
│  (Auth, routing, billing, multi-tenancy, model routing)              │
└─────────────────────────────────────────────────────────────────────┘
```

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-enterprise = "0.10"
futures = "0.3"
tokio = { version = "1", features = ["full"] }
```

Or via the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "0.10", features = ["enterprise-client"] }
```

## Quick Start

```rust,ignore
use adk_enterprise::{EnterpriseClient, CreateAgentParams, SessionEvent, ContentBlock};
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = EnterpriseClient::from_env()?;

    // Create an agent (any model — Gemini, OpenAI, Anthropic, etc.)
    let agent = client.create_agent(CreateAgentParams {
        name: "Hello Agent".into(),
        model: "gemini-2.5-flash".into(),
        system: Some("You are brief and friendly.".into()),
        ..Default::default()
    }).await?;

    // Start a session and open the event stream
    let session = client.create_session(&agent.id, None).await?;
    let mut stream = client.stream_events(&session.id).await?;

    // Send a message
    client.send_message(&session.id, "What is 2+2?").await?;

    // Process real-time events
    while let Some(event) = stream.next().await {
        match event? {
            SessionEvent::Message { content, .. } => {
                for block in content {
                    if let ContentBlock::Text { text } = block {
                        println!("{text}");
                    }
                }
            }
            SessionEvent::StatusIdle { .. } => break,
            _ => {}
        }
    }

    client.archive_session(&session.id).await?;
    Ok(())
}
```

## Custom Tool Round-Trip

Handle tools that the agent invokes but you execute locally:

```rust,ignore
use adk_enterprise::*;
use futures::StreamExt;
use serde_json::json;

let client = EnterpriseClient::from_env()?;

let agent = client.create_agent(CreateAgentParams {
    name: "Tool Agent".into(),
    model: "gpt-4.1".into(),
    tools: vec![
        ToolConfig::builtin("bash"),
        ToolConfig::custom("get_weather", "Get weather for a city", json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        })),
    ],
    ..Default::default()
}).await?;

let session = client.create_session(&agent.id, None).await?;
let mut stream = client.stream_events(&session.id).await?;
client.send_message(&session.id, "What's the weather in Tokyo?").await?;

while let Some(event) = stream.next().await {
    match event? {
        SessionEvent::CustomToolUse { custom_tool_use_id, name, input, .. } => {
            let result = match name.as_str() {
                "get_weather" => format!("22°C, sunny in {}", input["city"]),
                _ => "unknown tool".into(),
            };
            client.custom_tool_result(&session.id, &custom_tool_use_id, &result).await?;
        }
        SessionEvent::Message { content, .. } => { /* print response */ }
        SessionEvent::StatusIdle { .. } => break,
        _ => {}
    }
}
```

## Self-Hosted Deployment

Point at your own infrastructure — same API, identical behavior:

```rust,ignore
let client = EnterpriseClient::self_hosted(
    "adk_live_your_key",
    "https://your-server.internal/managed/v1",
)?;

// Everything works identically — no code paths conditional on URL
let agents = client.list_agents(None).await?;
```

## Provider Switching

Same agent definition, any model:

```rust,ignore
use adk_enterprise::{ModelRef, Provider};

// Shorthand (provider inferred from name)
let agent = client.create_agent(CreateAgentParams {
    model: "gemini-2.5-flash".into(),
    ..Default::default()
}).await?;

// Explicit provider
let agent = client.create_agent(CreateAgentParams {
    model: ModelRef::structured(Provider::Openai, "gpt-4.1"),
    ..Default::default()
}).await?;

// OpenAI-compatible endpoint (DeepSeek, Together, etc.)
let agent = client.create_agent(CreateAgentParams {
    model: ModelRef::compatible("deepseek-chat", "https://api.deepseek.com"),
    ..Default::default()
}).await?;
```

## API Surface

| Category | Methods |
|----------|---------|
| **Construction** | `new`, `from_env`, `self_hosted`, `with_config` |
| **Agents** | `create_agent`, `get_agent`, `list_agents`, `update_agent`, `archive_agent`, `delete_agent` |
| **Environments** | `create_environment`, `get_environment`, `archive_environment`, `delete_environment`, `download_environment` |
| **Sessions** | `create_session`, `create_session_full`, `get_session`, `list_sessions`, `pause_session`, `resume_session`, `archive_session`, `delete_session` |
| **Events** | `send_event`, `stream_events`, `list_events` |
| **Convenience** | `send_message`, `interrupt`, `allow_tool`, `deny_tool`, `custom_tool_result`, `define_outcome` |
| **Vault** (beta) | `create_vault`, `list_vaults`, `get_vault`, `archive_vault`, `delete_vault`, `create_credential`, `list_credentials`, `update_credential`, `validate_credential`, `delete_credential` |
| **Memory** (beta) | `create_memory_store`, `list_memory_stores`, `get_memory_store`, `delete_memory_store`, `create_memory`, `list_memories`, `get_memory`, `update_memory`, `delete_memory`, `list_memory_versions` |

## Error Handling

All methods return `Result<T, EnterpriseError>`. Error variants map directly to API responses:

```rust,ignore
match client.get_agent("agt_nonexistent").await {
    Ok(agent) => println!("Found: {}", agent.name),
    Err(EnterpriseError::NotFound { message }) => eprintln!("Not found: {message}"),
    Err(EnterpriseError::Authentication { .. }) => eprintln!("Invalid API key"),
    Err(EnterpriseError::RateLimit { retry_after, .. }) => {
        eprintln!("Rate limited — retry after {retry_after:?}");
    }
    Err(e) if e.is_retryable() => eprintln!("Transient (already retried): {e}"),
    Err(e) => eprintln!("Permanent: {e}"),
}
```

## Configuration

```rust,ignore
use adk_enterprise::ClientConfig;
use std::time::Duration;

let config = ClientConfig::new("adk_live_...")
    .with_base_url("https://staging.enterprise.adk-rust.com/managed/v1")
    .with_sse_timeout(Duration::from_secs(600))
    .with_max_retries(5)
    .with_retry_backoff(Duration::from_millis(500));

let client = EnterpriseClient::with_config(config)?;
```

## Module Structure

```text
adk-enterprise/src/
├── lib.rs           # Exports, crate docs
├── client.rs        # EnterpriseClient struct + constructors
├── config.rs        # ClientConfig builder
├── error.rs         # EnterpriseError enum (CANON §5)
├── retry.rs         # Exponential backoff + jitter
├── idempotency.rs   # UUID v4 key generation
├── response.rs      # HTTP response parsing + error mapping
├── stream.rs        # SSE parser + auto-reconnect + EventStream
└── types/
    ├── agent.rs         # Agent, CreateAgentParams, UpdateAgentParams
    ├── environment.rs   # Environment, CreateEnvironmentParams
    ├── session.rs       # Session, SessionStatus, Usage
    ├── events.rs        # UserEvent, SessionEvent, ContentBlock, StopReason
    ├── model_ref.rs     # ModelRef, Provider, ModelConfig
    ├── tools.rs         # ToolConfig, McpServerConfig, SkillRef, PermissionPolicy
    ├── vault.rs         # Vault, Credential (beta)
    ├── memory.rs        # MemoryStore, Memory (beta)
    └── pagination.rs    # ListResponse<T>, ListParams
```

## Requirements

- **Rust 1.85+** (edition 2024)
- **tokio** async runtime
- Network access to the Enterprise platform (or self-hosted deployment)
- API key (`adk_live_...` or `adk_test_...`)

## Stability

> **EXPERIMENTAL** — This crate is additive and feature-gated behind `enterprise-client`
> on the umbrella crate. It does not affect existing APIs when disabled. The API surface
> may change in future releases.

## License

Apache-2.0 — see [LICENSE](../LICENSE) for details.

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
