# adk-managed

Managed agent runtime for ADK-Rust — a provider-neutral agent execution engine with
checkpointing, event replay, and a supervised session loop.

> **Experimental.** Checkpoints, the agent registry, and active sessions are held **in
> memory**. They survive pause, resume, and replay within a process; they do **not** survive
> process loss, and a new process cannot resume a session started by another. Durable managed
> state is not implemented.

[![Crates.io](https://img.shields.io/crates/v/adk-managed.svg)](https://crates.io/crates/adk-managed)
[![Documentation](https://docs.rs/adk-managed/badge.svg)](https://docs.rs/adk-managed)
[![License](https://img.shields.io/crates/l/adk-managed.svg)](LICENSE)

## Overview

`adk-managed` provides the `ManagedAgentRuntime` trait and its default implementation. It takes a declarative `ManagedAgentDef`, builds a runnable agent, and operates it as a resumable, event-streaming background session, checkpointed in memory. The runtime composes existing shipping components behind a unified lifecycle trait.

This is the execution engine inside a managed agent service. It is a **library**, not a service — the platform hosts it.

## Key Capabilities

- **Provider-neutral**: Identical event sequences regardless of LLM provider (Gemini, OpenAI, Anthropic, Ollama, OpenAI-compatible)
- **Checkpointed sessions**: Checkpoint after every event, enabling replay and resume **within the process**. Checkpoints are in-memory and do not survive process loss.
- **Resumable**: Rehydrate from checkpoint and continue from last consistent state
- **Event streaming**: Uniform `SessionEvent` stream with monotonic sequence numbers and SSE replay support
- **Custom tool parking**: Client-executed tools park the loop until results arrive (or timeout)
- **Composable**: Injected services (sessions, sandbox, memory) — no platform dependencies
- **Additive**: Feature-gated; existing `Runner`/`LlmAgent` unchanged when feature is off

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                Platform Layer (ep-* crates)                  │
│    HTTP Routes │ Auth │ Billing │ Multi-tenancy              │
└──────────────────────────┬──────────────────────────────────┘
                           │ Rust trait calls (in-process)
                           ▼
┌─────────────────────────────────────────────────────────────┐
│            Runtime Layer (adk-managed — this crate)          │
│                                                             │
│  ManagedAgentRuntime trait + DefaultManagedAgentRuntime      │
│  ───────────────────────────────────────────────────        │
│  • Builds runnable agents from ManagedAgentDef              │
│  • Runs supervised session loop (in-process checkpoints)    │
│  • Emits provider-neutral SessionEvent stream               │
│  • Manages custom tool parking, checkpoints, interrupts     │
│  • Resolves ModelRef → Arc<dyn Llm>                         │
│                                                             │
│  Composes existing crates:                                  │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐    │
│  │adk-runner│ │adk-session│ │adk-model │ │adk-tool    │    │
│  └──────────┘ └──────────┘ └──────────┘ └────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-managed = "2.1.0"
adk-session = "2.1.0"
adk-core = "2.1.0"
tokio = { version = "1", features = ["full"] }
futures = "0.3"
async-trait = "0.1"
```

Or via the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["managed-runtime"] }
```

### Minimal Example

```rust,ignore
use std::sync::Arc;
use adk_managed::{
    DefaultManagedAgentRuntime, ManagedAgentRuntime, ModelResolver,
    ScriptedLlm, ScriptedTurn,
    resolver::ResolverResult,
    types::{ContentBlock, ManagedAgentDef, ModelRef, UserEvent},
};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use futures::StreamExt;

// A resolver that returns a scripted LLM (no API key needed)
struct MockResolver { llm: Arc<dyn adk_core::Llm> }

#[async_trait]
impl ModelResolver for MockResolver {
    async fn resolve(&self, _: &ModelRef) -> ResolverResult<Arc<dyn adk_core::Llm>> {
        Ok(self.llm.clone())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create a scripted LLM (deterministic, offline, $0)
    let llm = Arc::new(ScriptedLlm::new("test-model", vec![
        ScriptedTurn { text: Some("Hello!".into()), tool_calls: vec![] },
    ]));

    // 2. Build the runtime
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(MockResolver { llm }),
        Arc::new(InMemorySessionService::new()),
    );

    // 3. Create an agent from a declarative definition
    let def = ManagedAgentDef::new("my-agent", ModelRef::Shorthand("test-model".into()))
        .with_system("You are helpful.");
    let agent = runtime.create(def).await?;

    // 4. Start a session (initial status: Queued)
    let session = runtime.start_session(&agent, None).await?;

    // 5. Subscribe to events and send a message
    let mut stream = runtime.stream_events(&session, None).await?;
    runtime.send_event(&session, UserEvent::Message {
        content: vec![ContentBlock::Text { text: "Hi!".into() }],
    }).await?;

    // 6. Collect events: status.running → agent.message → status.idle
    while let Some(event) = stream.next().await {
        println!("{event:?}");
    }
    Ok(())
}
```

## Core Types

### ManagedAgentRuntime Trait

The central async trait defining the full agent lifecycle:

| Method | Description |
|--------|-------------|
| `create(def)` | Register an agent definition → `AgentHandle` |
| `start_session(agent, env?)` | Start a new session (initial status: `Queued`) |
| `send_event(session, event)` | Send a `UserEvent` to the session loop |
| `stream_events(session, from_seq?)` | Subscribe to `SessionEvent` stream |
| `interrupt(session)` | Stop at next boundary, emit `status.idle` |
| `pause(session)` | Checkpoint and pause processing |
| `resume(session)` | Resume from pause or process restart |
| `status(session)` | Query current `SessionStatus` |
| `archive(session)` | Terminal state (data retained for read) |
| `delete_session(session)` | Remove session and its data |

### ManagedAgentDef

Declarative agent definition with a builder API:

```rust,ignore
let def = ManagedAgentDef::new("my-agent", ModelRef::Shorthand("gemini-3.7-flash".into()))
    .with_system("You are a helpful assistant.")
    .with_description("Research agent with web search")
    .with_tools(vec![ToolConfig::WebSearch {}]);
```

### SessionEvent (Agent → Client)

Provider-neutral event stream with monotonic `seq`:

| Type | Description |
|------|-------------|
| `agent.message` | Assistant text content |
| `agent.tool_use` | Built-in tool invocation (server-side) |
| `agent.custom_tool_use` | Client-executed custom tool (loop parks) |
| `agent.mcp_tool_use` | MCP tool invocation |
| `status.running` | Turn started |
| `status.idle` | Turn complete (includes `stop_reason`) |
| `error` | Execution error |

### UserEvent (Client → Agent)

| Type | Description |
|------|-------------|
| `user.message` | Send content to the agent |
| `user.interrupt` | Stop the current turn |
| `user.tool_confirmation` | Allow/deny tool execution |
| `user.custom_tool_result` | Return custom tool results |
| `user.tool_result` | Built-in tool result (self-hosted only) |
| `user.define_outcome` | Set success criteria |

### ModelRef

Provider-neutral model reference:

```rust,ignore
// Shorthand (provider inferred from name prefix)
ModelRef::Shorthand("gemini-3.7-flash".into()) // → Gemini
ModelRef::Shorthand("gpt-5.6-terra".into())    // → OpenAI
ModelRef::Shorthand("claude-sonnet-5".into())  // → Anthropic

// Structured (explicit provider)
ModelRef::Structured {
    provider: Provider::OpenaiCompatible,
    model: ModelConfig::Compatible {
        model: "deepseek-v4-flash".into(),
        base_url: "https://api.deepseek.com/v1".into(),
        api_key: "sk-...".into(),
    },
    speed: None,
}
```

### SessionStatus

Lifecycle state machine:

```text
Queued → Running → Idle (per turn) → Running (next turn)
                 → Rescheduling → Running (retry success) | Failed (exhaust)
                 → Paused → Running (on resume)
                 → Completed / Failed / Archived
```

## Features

### Durable Sessions

Every event is checkpointed atomically. On process crash, `resume()` rehydrates from the last consistent checkpoint with no event loss.

### Custom Tool Parking

When the agent emits `agent.custom_tool_use`, the session loop parks until the client sends `user.custom_tool_result` or a configurable timeout elapses (default: 5 minutes).

### Event Replay (SSE Reconnection)

```rust,ignore
// Reconnect from seq 42 — replays events 43, 44, ... then live tail
let stream = runtime.stream_events(&session, Some(42)).await?;
```

### Provider Parity

An identical `ManagedAgentDef` run against all five providers produces byte-identical event type sequences (verified by golden fixture F-8).

## Testing with ScriptedLlm

`ScriptedLlm` is a deterministic LLM test double that exercises the full runtime pipeline. Only the provider API call is replaced:

```rust,ignore
use adk_managed::testing::{ScriptedLlm, ScriptedTurn, ScriptedToolCall};
use serde_json::json;

let llm = ScriptedLlm::new("test", vec![
    ScriptedTurn {
        text: Some("I'll search for that.".into()),
        tool_calls: vec![ScriptedToolCall {
            name: "web_search".into(),
            input: json!({"query": "rust agents"}),
            id: Some("tc_1".into()),
        }],
    },
    ScriptedTurn {
        text: Some("Here are the results...".into()),
        tool_calls: vec![],
    },
]);
```

This is NOT a mock — it implements the real `Llm` trait and exercises the full runtime (parking, checkpoints, replay, event mapping). Per-commit gate, $0 cost.

## Golden Fixture Tests

Eight fixture JSON files (F-1 through F-8) define conformance scenarios:

| Fixture | Tests |
|---------|-------|
| F-1 Hello | Basic message → response → idle |
| F-2 MCP Tool | MCP tool call flow |
| F-3 Custom Tool | Park → deliver → resume |
| F-4 Confirmation | Tool confirmation request → approve |
| F-5 Resume | Crash → resume from checkpoint |
| F-6 Replay | Historical event replay |
| F-7 Interrupt | Interrupt stops at boundary |
| F-8 Provider Parity | Identical sequences across providers |

Run conformance tests:

```bash
cargo test -p adk-managed --test fixture_conformance_tests
```

## Module Structure

```
adk-managed/src/
├── lib.rs                  # Feature gate, exports
├── runtime.rs              # ManagedAgentRuntime trait
├── default_runtime.rs      # DefaultManagedAgentRuntime implementation
├── types/
│   ├── mod.rs              # Re-exports
│   ├── agent_def.rs        # ManagedAgentDef, builder
│   ├── content.rs          # ContentBlock
│   ├── model_ref.rs        # ModelRef, Provider, ModelConfig
│   ├── tools.rs            # ToolConfig, McpServerConfig, SkillRef, PermissionPolicy
│   ├── events.rs           # UserEvent, SessionEvent, StopReason
│   ├── session.rs          # SessionStatus
│   └── error.rs            # RuntimeError
├── resolver.rs             # ModelRef → Arc<dyn Llm>
├── agent_builder.rs        # ManagedAgentDef → runnable agent
├── session_loop.rs         # Supervised loop: run turns, park, checkpoint
├── parking.rs              # Custom tool parking (channel-based wait)
├── checkpoint.rs           # Atomic event+state persistence
├── sequence.rs             # Monotonic seq counter per session
├── replay.rs               # Event replay with from_seq
├── event_mapping.rs        # Provider-neutral Runner → SessionEvent mapping
├── schema_normalization.rs # Cross-provider MCP schema normalization
├── usage.rs                # Uniform usage reporting
└── testing.rs              # ScriptedLlm, ScriptedTurn, ScriptedToolCall
```

## Feature Flags

| Flag | Description |
|------|-------------|
| `sandbox` | Enable `adk-sandbox` integration for isolated tool execution |
| `memory` | Enable `adk-memory` integration for cross-session memory |
| `full` | Enable all optional features |

## Smoke Test Example

A standalone example crate is provided:

```bash
cargo run --manifest-path examples/managed_runtime_hello/Cargo.toml
```

Runs fixture F-1 end-to-end with `ScriptedLlm` (no API key required). Platform teams can clone this to smoke-test integration.

## Stability

> **EXPERIMENTAL** — This crate is additive and feature-gated behind `managed-runtime` on the umbrella crate. It does not affect existing `Runner`/`LlmAgent` APIs when disabled. The API surface may change in future releases.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
