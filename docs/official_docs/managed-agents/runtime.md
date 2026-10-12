# Managed Agent Runtime

> **STABILITY: Experimental** — This feature is additive and feature-gated behind
> `managed-runtime`. It does not affect existing `Runner`/`LlmAgent` APIs when
> the feature is disabled. The API surface may change in future releases.

## Overview

The Managed Agent Runtime (`adk-managed`) is a provider-neutral, durable, resumable
agent execution engine. It takes a declarative `ManagedAgentDef`, builds a runnable
agent, and operates it as a checkpoint-resumable, event-streaming background session.
Every turn runs under a run in a `DurableRunStore`; a turn that waits on the client
parks its run, and the client's answer resumes it on any runtime instance over the same
stores.

The runtime is a **library**, not a service. The platform hosts it. This means:

- **Testable in isolation**: Zero HTTP/auth/billing dependencies
- **Embeddable**: Self-hosted deployments use the same runtime trait directly
- **Swappable platform**: Different platforms can host the same runtime
- **Provider-neutral**: Identical event sequences regardless of model provider

## Quick Start

Add the feature to your `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["managed-runtime"] }
```

Or use the `adk-managed` crate directly:

```toml
[dependencies]
adk-managed = "3.0.0"
adk-session = "3.0.0"
```

### Minimal Example (ScriptedLlm — no API key)

```rust,ignore
use std::sync::Arc;
use adk_managed::{
    DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner, ModelResolver,
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

    // 4. Start a session for its owner
    let owner = ManagedOwner::new("my-app", "user-1")?;
    let session = runtime.start_session(&agent, &owner, None).await?;

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
- `agent.custom_tool_use` — Client-executed custom tool; the turn ends with `requires_action`
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

Every event is checkpointed and written to the runtime's `ManagedStateStore` before it is
broadcast. A runtime instance that does not host a session rebuilds it from the store —
agent definition, owner, event log, sequence position, and pending tool calls — the first
time any method names it:

```rust,ignore
// A new process over the same stores
let runtime = DefaultManagedAgentRuntime::new(resolver, sessions).with_stores(stores);
runtime.recover().await?; // host every stored session with a run in progress
let stream = runtime.stream_events(&session, Some(5)).await?; // continues from seq 5
```

### Custom Tool Calls

A custom tool is executed by the client. When the model calls one, the turn ends:
`agent.custom_tool_use` is emitted, `status.idle` carries `requires_action` with the
call's ID, and the turn's run parks in the run store until the call expires
(`DurableConfig::pending_expiry`, five minutes by default). The session keeps reading
events while it waits.

```rust,ignore
// Agent emits: agent.custom_tool_use { custom_tool_use_id: "call_1", name: "deploy" }
// status.idle { stop_reason: requires_action { event_ids: ["call_1"] } }
// The client executes the tool, then:
runtime.send_event(&session, UserEvent::CustomToolResult {
    custom_tool_use_id: "call_1".into(),
    content: vec![ContentBlock::Text { text: "Deployed successfully".into() }],
}).await?;
// status.running → agent.message → status.idle
```

The result is recorded in the decision store and wakes the run. The resumed turn shows the
model the result in place of the call's pending response; a text result reaches the model
as `{"result": "<text>"}`. A call that expires is answered with an expiry error and the
turn continues. A message sent while calls are pending, or a result for an ID the session
is not waiting on, is refused with an error.

### Tool Confirmations

A `permission_policy` with `prompt` holds the call before it executes. The turn ends with
`agent.tool_use` and `requires_action`; `user.tool_confirmation` records the decision and
the resumed turn executes the call, or reports the denial and its `deny_message` to the
model. With `ManagedStores::with_approval_service`, held calls become approval requests
(quorum, delegation, escalation, expiry) and the service's decision wakes the run;
`user.tool_confirmation` is then refused.

### Parked Calls on the Durable Runtime

| Store | Holds | In-memory default |
|-------|-------|-------------------|
| `ManagedStateStore` | Event log, sequence, status, pending calls, agent definition | `InMemoryManagedStateStore` |
| `DurableRunStore` | One run per turn: lease, wait until expiry, wake | `InMemoryRunStore` |
| `ApprovalStore` | Custom tool results and confirmation decisions | `InMemoryApprovalStore` |

Each session has its own run queue, and a session's runs execute one at a time across
runtime instances. A session hosted by an instance polls its queue every
`DurableConfig::poll_interval` while its run is parked, so an answer recorded by another
instance, or an expiry, resumes it. Any backend of the three traits works; the run and
approval stores ship for SQLite, PostgreSQL, Redis, MongoDB, Neo4j, and Firestore in
`adk-session`.

```rust,ignore
use std::sync::Arc;
use adk_managed::{DefaultManagedAgentRuntime, Durability, ManagedStores, SessionServiceStateStore};

let sessions = Arc::new(adk_session::PostgresSessionService::new(url).await?);
let state = SessionServiceStateStore::new(sessions.clone(), Durability::CrashDurable);
let runs = Arc::new(adk_session::PostgresRunStore::new(url).await?);
let decisions = Arc::new(adk_session::PostgresApprovalStore::new(url).await?);

let runtime = DefaultManagedAgentRuntime::new(resolver, sessions)
    .with_stores(ManagedStores::new(Arc::new(state), runs, decisions));
assert_eq!(runtime.durability(), Durability::CrashDurable);
```

### Model Resolution

`DefaultModelResolver` constructs the provider a `ModelRef` names. Each provider is built
only under its feature of `adk-managed`; on the umbrella crate, the provider feature of the
same name enables it alongside `managed-runtime`.

| Provider | Shorthand prefix | Feature | Credentials |
|----------|------------------|---------|-------------|
| Gemini | `gemini` | `gemini` | `GOOGLE_API_KEY` or `GEMINI_API_KEY`, or Vertex AI through `GOOGLE_GENAI_USE_VERTEXAI` |
| OpenAI | `gpt` | `openai` | `OPENAI_API_KEY` |
| Anthropic | `claude` | `anthropic` | `ANTHROPIC_API_KEY` |
| Ollama | `llama`, `mistral`, `deepseek` | `ollama` | none; `OLLAMA_HOST` overrides the host |
| OpenAI-compatible | structured only | `openai` | the reference's `api_key` |

`DefaultModelResolver::with_api_key(provider, key)` replaces the environment. A stored
session drops the `api_key` of an OpenAI-compatible reference, so a rebuilt session takes
the key from `with_api_key(Provider::OpenaiCompatible, ..)`.

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["managed-runtime", "openai", "anthropic"] }
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

Managed session state — the event log, sequence position, pending tool calls, lifecycle
status, and the agent definition and owner a session is rebuilt from — lives in a
`ManagedStateStore`. The store reports its own guarantee:

| Durability | Meaning |
|------------|---------|
| `ProcessLocal` | Replay and resume work while the process runs. A crash loses the state, and another process cannot resume the session. |
| `CrashDurable` | State is written to a backing store before the write is acknowledged, so another process can reconstruct the session. |

| Store | Durability | Backed by |
|-------|------------|-----------|
| `InMemoryManagedStateStore` | `ProcessLocal` | Process memory (the default) |
| `FileManagedStateStore` | `CrashDurable` | One fsynced JSON file per session on one host |
| `SessionServiceStateStore` | As declared for the service | Any `SessionService` backend |

`SessionServiceStateStore` keeps each managed session's state in a companion session of
the session service, appending only the events added since the previous write. A
`dyn SessionService` cannot report whether it is durable, so the constructor takes the
guarantee of the service passed to it. `DefaultManagedAgentRuntime::durability` reports the
state store's guarantee; resume after a restart also needs a durable run store, decision
store, and session service.

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

1. Sets the session terminal, cancels its loop, and cancels its run.
2. Removes the runtime handle.
3. Deletes the persisted conversation through the injected `SessionService`, under the same
   identity `start_session` created it with.
4. Deletes the session's state from the `ManagedStateStore`.

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
