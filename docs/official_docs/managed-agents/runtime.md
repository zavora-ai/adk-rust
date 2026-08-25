# Managed Agent Runtime

> **STABILITY: Experimental** — This feature is additive and feature-gated behind
> `managed-runtime`. It does not affect existing `Runner`/`LlmAgent` APIs when
> the feature is disabled. The API surface may change in future releases.

## Overview

The Managed Agent Runtime (`adk-managed`) is a provider-neutral, durable, resumable
agent execution engine. It takes a declarative `ManagedAgentDef`, builds a runnable
agent, and operates it as a checkpoint-resumable, event-streaming background session.

The runtime is a **library**, not a service. The platform hosts it. This means:

- **Testable in isolation**: Zero HTTP/auth/billing dependencies
- **Embeddable**: Self-hosted deployments use the same runtime trait directly
- **Swappable platform**: Different platforms can host the same runtime
- **Provider-neutral**: Identical event sequences regardless of model provider

## Quick Start

Add the feature to your `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["managed-runtime"] }
```

Or use the `adk-managed` crate directly:

```toml
[dependencies]
adk-managed = "2.1.0"
adk-session = "2.1.0"
```

### Minimal Example (ScriptedLlm — no API key)

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

// A resolver that returns our scripted LLM
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

    // 3. Create an agent
    let def = ManagedAgentDef::new("my-agent", ModelRef::Shorthand("test-model".into()))
        .with_system("You are helpful.");
    let agent = runtime.create(def).await?;

    // 4. Start a session
    let session = runtime.start_session(&agent, None).await?;

    // 5. Subscribe to events and send a message
    let mut stream = runtime.stream_events(&session, None).await?;
    runtime.send_event(&session, UserEvent::Message {
        content: vec![ContentBlock::Text { text: "Hi!".into() }],
    }).await?;

    // 6. Collect events
    while let Some(event) = stream.next().await {
        println!("{event:?}");
    }
    Ok(())
}
```

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                Platform Layer (ep-* crates)                  │
│    HTTP Routes │ Auth │ Billing │ Multi-tenancy              │
└──────────────────────────┬──────────────────────────────────┘
                           │ Rust trait calls (in-process)
                           ▼
┌─────────────────────────────────────────────────────────────┐
│            Runtime Layer (adk-managed)                       │
│                                                             │
│  ManagedAgentRuntime trait + DefaultManagedAgentRuntime      │
│  ───────────────────────────────────────────────────        │
│  • Builds runnable agents from ManagedAgentDef              │
│  • Runs supervised session loop (durable, resumable)        │
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

## Core Types

### ManagedAgentRuntime Trait

The central async trait defining the full agent lifecycle:

| Method | Description |
|--------|-------------|
| `create(def)` | Register an agent definition, returns `AgentHandle` |
| `start_session(agent, env?)` | Start a new session, initial status `Queued` |
| `send_event(session, event)` | Send a `UserEvent` to the session |
| `stream_events(session, from_seq?)` | Subscribe to `SessionEvent` stream |
| `interrupt(session)` | Stop at next boundary, emit `status.idle` |
| `pause(session)` | Checkpoint and pause processing |
| `resume(session)` | Resume from pause or restart |
| `status(session)` | Query current `SessionStatus` |
| `archive(session)` | Terminal state, data retained |
| `delete_session(session)` | Remove session data |

### ManagedAgentDef

Declarative agent definition with builder API:

```rust,ignore
let def = ManagedAgentDef::new("my-agent", ModelRef::Shorthand("gemini-3.7-flash".into()))
    .with_system("You are a helpful assistant.")
    .with_description("Research agent with web search")
    .with_tools(vec![ToolConfig::BuiltIn(ManagedBuiltinTool::WebSearch)]);
```

### SessionEvent

Provider-neutral event stream with monotonic sequence numbers:

- `agent.message` — Assistant text content
- `agent.tool_use` — Built-in tool invocation
- `agent.custom_tool_use` — Client-executed custom tool (loop parks)
- `agent.mcp_tool_use` — MCP tool invocation
- `status.running` — Turn started
- `status.idle` — Turn complete (with `stop_reason`)
- `error` — Execution error

### UserEvent

Client-to-agent events:

- `user.message` — Send content to the agent
- `user.interrupt` — Stop current turn
- `user.tool_confirmation` — Allow/deny tool execution
- `user.custom_tool_result` — Return custom tool results
- `user.tool_result` — Built-in tool result (self-hosted only)
- `user.define_outcome` — Set success criteria

### ModelRef

Provider-neutral model reference supporting all providers:

```rust,ignore
// Shorthand (provider inferred from name)
ModelRef::Shorthand("gemini-3.7-flash".into())
ModelRef::Shorthand("gpt-5.6-terra".into())
ModelRef::Shorthand("claude-sonnet-5".into())

// Structured (explicit provider)
ModelRef::Structured {
    provider: Provider::OpenaiCompatible,
    model: ModelConfig::Compatible {
        model: "my-model".into(),
        base_url: "https://my-endpoint.com/v1".into(),
        api_key: "sk-...".into(),
    },
    speed: None,
}
```

## Key Features

### Durable Sessions

Every event is checkpointed atomically. On process crash, `resume()` rehydrates
from the last consistent checkpoint with no event loss:

```rust,ignore
// Before crash: events 0..5 committed
// After restart:
runtime.resume(&session).await?;
// Continues from seq=5, no gap, no duplicate
```

### Custom Tool Parking

When the agent emits `agent.custom_tool_use`, the loop parks until the client
returns results or a configurable timeout elapses:

```rust,ignore
// Agent emits: agent.custom_tool_use { custom_tool_use_id: "ct_1", name: "deploy" }
// Client executes the tool, then:
runtime.send_event(&session, UserEvent::CustomToolResult {
    custom_tool_use_id: "ct_1".into(),
    content: vec![ContentBlock::Text { text: "Deployed successfully".into() }],
}).await?;
```

### Event Replay

Support SSE `Last-Event-ID` reconnection via sequence-based replay:

```rust,ignore
// Reconnect from seq 42 — replays events 43, 44, ... then live tail
let stream = runtime.stream_events(&session, Some(42)).await?;
```

### Provider Parity

Identical `ManagedAgentDef` produces byte-identical event type sequences across
Gemini, OpenAI, Anthropic, Ollama, and OpenAI-compatible providers (fixture F-8).

## Testing with ScriptedLlm

`ScriptedLlm` is a deterministic LLM double that exercises the full runtime
pipeline. Only the provider API call is replaced:

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

## API Reference

Full API documentation is available on docs.rs:

- [`adk-managed` API docs](https://docs.rs/adk-managed)
- [`ManagedAgentRuntime` trait](https://docs.rs/adk-managed/latest/adk_managed/runtime/trait.ManagedAgentRuntime.html)
- [`DefaultManagedAgentRuntime`](https://docs.rs/adk-managed/latest/adk_managed/default_runtime/struct.DefaultManagedAgentRuntime.html)

## Smoke Test Example

A standalone example crate is provided for platform teams:

```bash
cargo run --manifest-path examples/managed_runtime_hello/Cargo.toml
```

This runs fixture F-1 end-to-end with `ScriptedLlm` (no API key required).

## Managed state durability

Managed session state — the event log, sequence position, parked tool calls, and lifecycle
status — lives in a `ManagedStateStore`. The store reports its own guarantee:

| Durability | Meaning |
|------------|---------|
| `ProcessLocal` | Replay and resume work while the process runs. A crash loses the state, and another process cannot resume the session. |
| `CrashDurable` | State is written to a backing store before the write is acknowledged, so another process can reconstruct the session. |

**Only `InMemoryManagedStateStore` ships, and it is `ProcessLocal`.** Check the guarantee rather
than inferring it from the presence of checkpointing:

```rust
use adk_managed::{Durability, InMemoryManagedStateStore, ManagedStateStore};

let store = InMemoryManagedStateStore::new();
assert_eq!(store.durability(), Durability::ProcessLocal);
assert!(!store.durability().survives_process_loss());
```

### Checkpointing versus flushing

`CheckpointManager::checkpoint` records an event and the new run state together, so replay never
sees one without the other. That is a write to the manager's own fields. `flush` writes the
snapshot to the configured store, and `restore` rebuilds a manager from it:

```rust
use adk_managed::{CheckpointManager, InMemoryManagedStateStore, ManagedStateStore};
use std::sync::Arc;

# async fn example() -> Result<(), adk_managed::types::RuntimeError> {
let store: Arc<dyn ManagedStateStore> = Arc::new(InMemoryManagedStateStore::new());
let manager = CheckpointManager::new("session-1".to_string()).with_store(Arc::clone(&store));
manager.flush().await?;

let restored = CheckpointManager::restore("session-1".to_string(), store).await?;
assert_eq!(restored.session_id(), "session-1");
# Ok(())
# }
```

## Status reporting

`ManagedAgentRuntime::status` reads the same handle the session loop writes to, so normal
transitions are visible, not just control-plane ones:

| Transition | Cause |
|------------|-------|
| `Queued` → `Running` | A turn begins |
| `Running` → `Idle` | The turn completes and usage is recorded |
| any → `Paused` | `pause` |
| any → `Archived` | `archive` or `delete_session` |

> **Note:** before this was one shared handle, `status` reported `Queued` for the entire life
> of a session, including while it was executing turns. Control-plane transitions
> (pause, resume, archive) were visible because they wrote to the handle directly.

## Deletion semantics

`delete_session` removes both planes:

1. Sets the session terminal and cancels its loop.
2. Removes the runtime handle.
3. Deletes the persisted conversation through the injected `SessionService`, under the same
   identity `start_session` created it with.

If step 3 fails, `delete_session` returns an error naming the app, user, and session that
still hold data — the handle is already gone at that point, so the caller has to be told
what needs manual cleanup rather than being allowed to assume success.

```rust,ignore
runtime.delete_session(&session).await?;
// The handle is gone and the conversation is no longer in the session backend.
```

> **Important:** deletion removes the conversation for the session under its owner. See
> [Session ownership](#session-ownership).

## Session ownership

`start_session` requires a `ManagedOwner`. The session is persisted under that identity, and
every Runner call the session loop makes uses it:

```rust
use adk_managed::{ManagedAgentRuntime, ManagedOwner};

# async fn start(runtime: &dyn ManagedAgentRuntime, agent: &adk_managed::AgentHandle)
# -> Result<(), adk_managed::RuntimeError> {
let owner = ManagedOwner::new("support-console", "user-42")?;
let session = runtime.start_session(agent, &owner, None).await?;
# Ok(())
# }
```

> **Important:** `checkpoint` was documented as "atomically persist" with a guarantee that
> "replay will see a consistent view after any crash", and loading was described as returning
> "everything needed to reconstruct a session after a restart". Neither held: both operated on
> in-memory fields with no transaction against any persistent store. With the shipped store,
> `restore` in a new process finds nothing.
Both components are required and must be non-blank. Sessions belonging to different owners are
addressed separately, so lookup and deletion are scoped to one owner and cannot reach another's
data.

> **Note:** every managed session was previously persisted under the constants `managed` /
> `managed_user`, so all of them shared one logical namespace: nothing could be scoped to a
> caller and no session could be attributed to one.

## Environment configuration

`EnvironmentConfig` carries `env_vars` and `working_dir`. This runtime **rejects** a
configuration that requests either:

```text
invalid request: EnvironmentConfig cannot be honoured by this runtime: sessions run
in-process, so per-session environment variables and working directories would have to mutate
process-global state shared with other sessions. Pass `None`, or configure a sandboxed runtime.
```

Sessions run in-process, so applying per-session environment variables or a working directory
would mutate state shared with every other session. Refusing is the honest outcome; a sandboxed
execution boundary is what would make the request satisfiable.

> **Note:** the argument was previously named `_env` and discarded, so a caller supplying
> environment configuration received a session that silently ignored it.
