# adk-core

Core traits and types for ADK-Rust agents, tools, sessions, and events.

[![Crates.io](https://img.shields.io/crates/v/adk-core.svg)](https://crates.io/crates/adk-core)
[![Documentation](https://docs.rs/adk-core/badge.svg)](https://docs.rs/adk-core)
[![License](https://img.shields.io/crates/l/adk-core.svg)](../LICENSE)

## Overview

`adk-core` provides the foundational abstractions for [ADK-Rust](https://github.com/zavora-ai/adk-rust). Every other ADK crate builds on the traits and types it defines:

- **`Agent`** — the abstraction every agent implements
- **`Tool` / `Toolset`** — capabilities an agent can call
- **`Llm`** — the interface every model provider implements
- **`SchemaAdapter`** — provider-aware JSON Schema normalization for tool declarations
- **Context hierarchy** — `ReadonlyContext` → `CallbackContext` → `ToolContext` / `InvocationContext`
- **`Content` / `Part`** — message content
- **`Event` / `EventActions`** — the event stream an agent produces
- **`Session` / `State`** — conversation context
- **`AdkError`** — the structured error envelope
- **Governance** — `ToolPolicy`, `authorize_tool_call`, `GovernanceControl`, approvals, run budgets, and the spend ledger
- **Durable runtime contracts** — `LeaseStore`, `DurableRunStore`, `ScheduleStore`, `GovernanceStateStore`, `ActionLedger`, `ApprovalStore`, `SpendLedger`, `AuditLogStore`

The crate contains no model provider client.

## Installation

```toml
[dependencies]
adk-core = "3.0.0"
```

Or use the umbrella crate:

```toml
[dependencies]
adk-rust = "3.0.0"
```

## Feature Flags

No feature is enabled by default.

| Feature | Enables |
|---------|---------|
| `rustls` | `ensure_crypto_provider()` installs the `aws-lc-rs` rustls provider process-wide |
| `conformance` | The `conformance` module: the `*_conformance_tests!` suites for durable store backends |
| `cron` | Cron schedules in IANA time zones for `ScheduleStore` and `schedule::plan_fires` |
| `webhooks` | The `webhook` module: Standard Webhooks signing, verification, and replay protection |
| `webhook-delivery` | The `webhook_delivery` module: signed outbound webhook delivery (implies `webhooks`) |

## Core Traits

### Agent

| Method | Default | Purpose |
|--------|---------|---------|
| `name()` | required | Unique name within the agent tree |
| `description()` | required | Returns `&str`; shown to models that route between agents |
| `sub_agents()` | required | Child agents; return `&[]` for a leaf agent |
| `run(ctx)` | required | Returns `Result<EventStream>` |
| `allows_transfer_to_parent()` / `allows_transfer_to_peers()` | `true` | Transfer restrictions the runner enforces for every transfer |
| `screen_user_input(content)` | no screening | Input guardrails the runner applies before it saves a user message |
| `close()` | closes each sub-agent | Releases what the agent holds; `Runner::close` calls it |

`EventStream` is `Pin<Box<dyn Stream<Item = Result<Event>> + Send>>`.

### Tool

```rust
use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

struct GetWeather;

#[async_trait]
impl Tool for GetWeather {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Returns the weather for a city"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        Ok(json!({ "city": args["city"], "forecast": "sunny" }))
    }
}
```

| Method | Default | Purpose |
|--------|---------|---------|
| `parameters_schema()` / `response_schema()` | `None` | JSON Schemas sent to the model |
| `is_long_running()` | `false` | The call returns before its work completes |
| `is_read_only()` | `false` | The call has no side effects |
| `is_concurrency_safe()` | `false` | The call can run beside other calls |
| `is_agent_delegation()` | `false` | The tool delegates to another agent |
| `effect()` | `ReadOnly` when `is_read_only()`, else `NonIdempotent` | Governs retries, the action ledger, and `DeclarativePolicy` effect rules |
| `timeout_override()` | `None` (the agent's tool timeout) | `Some(None)` removes the timeout; `Some(Some(d))` sets one |
| `origin()` | `STATIC_TOOL_ORIGIN` (`"static"`) | Where the tool came from; MCP tools report `mcp:{toolset name}` |
| `annotations()` | `None` | Behaviour annotations the tool's publisher declared |
| `required_scopes()` | `&[]` | Scopes the caller must hold |

Only `ReadOnly` and `Idempotent` tools are retried, and only after a retryable error or a timeout. With an `ActionLedger` on `RunConfig::action_ledger` (`InMemoryActionLedger` here, `SqliteActionLedger` and the other backends in `adk-session`), a `NonIdempotent` call executes at most once per `ToolContext::idempotency_key()`; a replay of a call that never recorded an outcome is answered with `outcome_unknown_response()` instead of executing again.

`ToolExecutionStrategy::Auto` keeps the LLM-returned order and overlaps a call with its adjacent neighbours only when the selected tool returns `true` from both `is_read_only()` and `is_concurrency_safe()`. Every other call runs alone, in its place: it starts after the calls listed before it finish, and the calls listed after it wait for it. The guarantee is ordering only — a write that is held for confirmation, refused, skipped or failed has changed nothing when the next call runs.

`ensure_unique_tool_names` fails with `agent.duplicate_tool_name` (`DUPLICATE_TOOL_NAME_CODE`) when two tools share a name; `LlmAgentBuilder::build` and the other agent builders run the same check.

### Toolset

```rust
use std::sync::Arc;

use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;

struct WeatherTools {
    tools: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Toolset for WeatherTools {
    fn name(&self) -> &str {
        "weather"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.tools.clone())
    }
}
```

`process_llm_request` adds toolset-specific guidance to each model request, and `close` releases connections or subprocesses; both default to doing nothing.

### Llm

| Method | Default | Purpose |
|--------|---------|---------|
| `name()` | required | Model identifier |
| `generate_content(request, stream)` | required | Returns `Result<LlmResponseStream>` |
| `schema_adapter()` | `GenericSchemaAdapter` | Provider-specific schema normalization |
| `max_cost_micro_usd(request)` | `None` | Upper bound on the request's cost; a spend cap refuses a model that returns `None` |

Wrappers around another `Llm` forward `max_cost_micro_usd`.

### SchemaAdapter

`normalize_schema` (required), `normalize_tool_name`, `empty_schema`, and `parameters_field` adapt tool declarations to a provider's schema dialect. Each `Llm` returns its adapter from `schema_adapter()`; `GenericSchemaAdapter` is the default.

## Key Types

### Content and Part

```rust
use adk_core::Content;

fn message(image_bytes: Vec<u8>) -> Content {
    Content::new("user")
        .with_text("Hello!")
        .with_inline_data("image/png", image_bytes)
        .with_file_uri("image/jpeg", "https://example.com/image.jpg")
}
```

`Part` variants: `Text`, `Thinking`, `InlineData`, `FileData`, `FunctionCall`, `FunctionResponse`, `ServerToolCall`, `ServerToolResponse`, and `EmbeddedResource`. `Content::with_inline_data()` and `Part::inline_data()` panic above 10 MB (`MAX_INLINE_DATA_SIZE`).

### Multimodal Function Responses

A tool returns images, audio, or file references alongside JSON:

```rust
use adk_core::{FileDataPart, FunctionResponseData, InlineDataPart};
use serde_json::{Value, json};

// A tool result with `inline_data` or `file_data` arrays is split into parts
// by `FunctionResponseData::from_tool_result`.
fn chart_result(png_bytes: Vec<u8>) -> Value {
    json!({
        "response": { "title": "Q4 Chart" },
        "inline_data": [{ "mime_type": "image/png", "data": png_bytes }]
    })
}

fn direct(png_bytes: Vec<u8>) -> (FunctionResponseData, FunctionResponseData) {
    let image = FunctionResponseData::with_inline_data(
        "chart_tool",
        json!({ "title": "Q4 Chart" }),
        vec![InlineDataPart {
            mime_type: "image/png".into(),
            data: png_bytes,
            uri: None,
            annotations: None,
        }],
    );
    let file = FunctionResponseData::with_file_data(
        "doc_tool",
        json!({ "status": "ok" }),
        vec![FileDataPart {
            mime_type: "application/pdf".into(),
            file_uri: "gs://bucket/report.pdf".into(),
            annotations: None,
        }],
    );
    (image, file)
}
```

### Event

```rust
use adk_core::Event;

fn inspect(event: &Event) {
    let _content = event.content(); // Option<&Content>
    let _actions = &event.actions; // state changes, transfers, escalation
    let _metadata = &event.provider_metadata; // HashMap<String, String>
}
```

#### Complete Response Snapshots

Terminal events marked with `llm_response.provider_metadata.content_complete` contain the complete response for their event ID, including earlier streamed text. Preserve the original event for persistence and replacement-aware UIs. Append-only output can use one `EventTextDeltas` adapter per live stream:

```rust
use adk_core::{EventStream, EventTextDeltas, Result};
use futures::StreamExt;

async fn render(mut stream: EventStream) -> Result<()> {
    let mut deltas = EventTextDeltas::default();
    while let Some(event) = stream.next().await {
        let event = event?;
        let output = deltas.push(&event);
        // Render output.content(); persist the original event.
        let _ = output.content();
    }
    Ok(())
}
```

The adapter preserves tool parts, thinking signatures, actions, and usage. A non-prefix correction is returned in full because append-only output cannot retract text already rendered.

#### Tool Events

Events expose typed views of tool activity, so UIs render tool calls and results without matching `Part` internals:

```rust
use adk_core::Event;

fn render_tools(event: &Event) {
    for call in event.tool_calls() {
        println!("-> {} {}", call.name, call.args);
    }
    for result in event.tool_results() {
        println!("<- {} {}", result.name, result.response);
    }
    if let Some(stream_name) = event.tool_progress_stream() {
        println!("progress on {stream_name}");
    }
}
```

`call_id` correlates a call with its result and its progress chunks. A tool pushes intermediate output with `ToolContext::emit_progress("stdout", chunk)`; the framework forwards each chunk as a partial event on the same `EventStream`. The default `emit_progress` does nothing.

### EventActions

| Field | Purpose |
|-------|---------|
| `state_delta` | State updates |
| `artifact_delta` | Artifact versions saved by the event |
| `skip_summarization` | End the turn on a tool result without another model call |
| `transfer_to_agent` | Agent transfer |
| `escalate` | Escalate to the parent |
| `tool_confirmation` / `tool_confirmation_decision` | A call held for confirmation, and its decision |
| `compaction` | An `EventCompaction` summary that replaces earlier events |
| `route` | Target node names for dynamic routing in graph workflows |
| `input_screened` | The runner screened this user message with input guardrails |
| `workflow_positions` | Where each workflow agent stopped on a held call |

`EventActions` derives `Default`; struct literals end with `..Default::default()`.

### Tool Context Engineering

| Type | Purpose |
|------|---------|
| `ResolvedContext` | A system instruction together with the `Arc<dyn Tool>` instances it describes |
| `ToolRegistry` | Maps tool names from configuration or skills to tool instances |
| `ValidationMode` | `Strict` (default) rejects a request that names a missing tool; `Permissive` binds the tools that exist and logs a warning |

## Context Hierarchy

```text
ReadonlyContext
    ├── invocation_id(), agent_name(), user_id(), app_name(), session_id(), branch()
    ├── user_content()
    └── acting_context()

CallbackContext (extends ReadonlyContext)
    ├── artifacts()
    └── shared_state()

ToolContext (extends CallbackContext)
    ├── function_call_id(), idempotency_key()
    ├── actions() / set_actions()
    ├── search_memory()
    └── emit_progress()

InvocationContext (extends CallbackContext)
    ├── agent(), memory(), session()
    ├── run_config()
    └── end_invocation() / ended()
```

Context wrappers forward `acting_context()`; otherwise they report the derived identity.

## Error Handling

`AdkError` carries a component, a category, a stable code, a message, a retry hint, and details:

```rust
use adk_core::{AdkError, ErrorCategory, ErrorComponent};

let err = AdkError::new(
    ErrorComponent::Model,
    ErrorCategory::RateLimited,
    "model.openai.rate_limited",
    "OpenAI API rate limit exceeded",
)
.with_provider("openai")
.with_upstream_status(429);

assert!(err.is_retryable());
assert_eq!(err.http_status_code(), 429);
let _problem = err.to_problem_json();
let _log_line = err.diagnostics().to_string(); // everything but the message
```

| Category | HTTP | Retryable by default |
|----------|------|----------------------|
| `InvalidInput` | 400 | no |
| `Unauthorized` | 401 | no |
| `Forbidden` | 403 | no |
| `NotFound` | 404 | no |
| `Timeout` | 408 | yes |
| `Conflict` | 409 | no |
| `RateLimited` | 429 | yes |
| `ResourceExhausted` | 429 | no |
| `Cancelled` | 499 | no |
| `Internal` | 500 | no |
| `Unsupported` | 501 | no |
| `Unavailable` | 503 | yes |

`ErrorCategory` is not `#[non_exhaustive]`, so exhaustive matches list every category. `upstream_excerpt` and `json_error_summary` keep upstream text and JSON values out of messages and logs.

## Governed Tool Execution

`LlmAgent` and `CodeActAgent` run each tool call through `authorize_tool_call` after plugin and callback rewrites and immediately before `Tool::execute`:

| Step | Type | Effect |
|------|------|--------|
| Kill switch | `GovernanceControl` | A frozen run ends with a `governance.frozen` error |
| Resume binding | `GovernedCall::resuming` | A resumed held call runs only for the agent that held it, on a tool with the `ToolOrigin` it was held for |
| Policy | `ToolPolicy`, `DeclarativePolicy`, `StrictestPolicy` | Allow, deny, or require approval; `DeclarativePolicy` denies unlisted tools; `evaluate_call` also receives the tool's `ToolOrigin` and the call ID (`PolicyCall`); `StrictestPolicy` applies several policies and keeps the strictest decision |
| Screen | `ToolCallScreen` | The agent's guardrails; a rewrite is checked against the policy again |
| Confirmation | `ToolApproval`, `ApprovalStore`, `ToolConfirmationHandler::decide_approval` | A live handler may return edited arguments, checked against the policy again, or a denial message. Decisions bind to the call's fingerprint — acting agent, tool origin and declaration digest, tool name, arguments — so they survive a re-issued call ID and never authorize another agent's call or another server's tool |
| Approval service | `ApprovalService`, `StoreApprovalService`, `ApprovalPolicy` | An inbox over the approval store: quorum, delegation, escalation, expiry, and edit / reject / respond actions with compare-and-swap decisions |

```rust
use adk_core::{ArgPredicate, DeclarativePolicy, PolicyDecision, PolicyRule, ToolPolicyRequest};
use serde_json::json;

let policy = DeclarativePolicy::builder()
    .allow_read_only()
    .rule(PolicyRule::allow("transfer").when(ArgPredicate::at_most("/amount", 100.0)))
    .build();

let request = ToolPolicyRequest::new("transfer", json!({ "amount": 500 }));
assert!(matches!(policy.decide(&request), PolicyDecision::Deny { .. }));
```

Build a fingerprint from the request a held call surfaced, or with `ToolConfirmationRequest::for_tool(tool, args).with_acting_context(..)`; `tool_call_fingerprint(name, args)` alone, or a request without an acting agent, matches no call, and `RunConfigBuilder::tool_approval` refuses such a fingerprint (`ToolConfirmationRequest::fingerprint_agent`). `ToolApproval::approve()` and `ToolApproval::deny()` take `with_edited_args`, `with_message`, and an expiry.

Provider-executed built-in tools (Gemini Google Search, OpenAI hosted web search, Anthropic web search, and the others) never reach `authorize_tool_call`. `BuiltinToolGovernor` applies the kill switch, the tool-call budget, and the policy to them before each model request: a denied tool, or one that needs an approval that does not exist yet, is withheld from the request. The uses a provider reports are counted in `UsageMetadata::builtin_tool_uses`, against the run's tool-call budget, and in `UsageMetadata::cost`.

## Agent Identity

Every run has an acting agent. `AgentId`, `AgentPrincipal`, and `ActingContext` (module `principal`) identify it; `ActingConfig` (module `acting`, on `RunConfig::acting`) maps agent names to principals and pins the subject to the session's user. Without configuration an agent acts as `urn:adk:agent:{app}/{agent}`. `IdentitySource`, `StaticIdentitySource`, and `CredentialBroker` (module `credential`) resolve principals and mint audience-bound `ScopedToken`s. `PolicyRule::for_agent`, `for_tenant`, and `for_class` restrict a policy rule to an acting agent.

## Run Budgets and Spend

`RunBudget` limits model calls, total tokens, cost (micro-USD), wall time, and tool calls; `RunConfig::budget` carries it and `RunConfig::budget_tracker` carries the shared `BudgetTracker`. A violation converts to an `AdkError` with category `ResourceExhausted` and a `budget.*` code.

```rust
use adk_core::{BudgetTracker, RunBudget};

let tracker = BudgetTracker::new(RunBudget::new().max_tool_calls(2));
assert!(tracker.begin_tool_calls(2).is_ok());
assert!(tracker.begin_tool_calls(1).is_err());
```

`SpendLedger` holds one budget for model calls and payments: `reserve` holds an amount against every `SpendLimits` cap that covers a `SpendKey` (org, agent, vendor; day, month, or lifetime in UTC), `commit` records the actual amount, and `release` drops the hold. Unsettled holds expire after a TTL. `InMemorySpendLedger` is the process-local implementation; `adk-session` provides SQLite, PostgreSQL, Redis, MongoDB, Neo4j, and Firestore ledgers. Set `RunConfig::spend_ledger` to cap a run; a refused reservation fails with `spend.limit_exceeded`. `with_clock` takes a `Clock`; `ManualClock` lets a test cross a day or month boundary.

```rust
use adk_core::{InMemorySpendLedger, SpendKey, SpendLimits, SpendPeriod};

let ledger = InMemorySpendLedger::new(
    SpendLimits::new().limit(SpendKey::org("my-app").per(SpendPeriod::Day), 50_000_000),
);
```

`ModelCallScope` applies the run budget and the spend ledger to model calls made outside an agent's own loop — compaction summaries, LLM routing, MCP sampling. `generate_metered` and `MeteredLlm` meter against the scope made current with `ModelCallScope::within`. `LlmResponse` carries `model` and `provider` so usage and cost are attributable.

## Audit Records

`AuditRecorder` (module `audit_log`) receives a record for every governed tool decision, approval decision, kill-switch change, and `outcome_unknown` answer; `InMemoryAuditRecorder` keeps them in memory and `FileAuditRecorder` appends them to a JSON Lines file that survives a restart. Each record names the acting agent and the subject it acts for and carries an argument digest, never the arguments. Under `AuditFailureMode::Block` (the default) an action whose record cannot be written does not happen. `AuditLogStore` and `InMemoryAuditLogStore` hold append-only logs with a compare-and-swap on the log size; `adk-auth`'s `signed_audit::AuditLog` signs them.

## Durable Runtime Contracts

Backend-neutral traits for running agents durably across replicas, each with an in-memory implementation that takes a `Clock` through `with_clock`:

| Trait | In-memory | Holds |
|-------|-----------|-------|
| `LeaseStore` | `InMemoryLeaseStore` | Leases with fencing epochs |
| `DurableRunStore` | `InMemoryRunStore` | The run queue: claims, heartbeats, waits, retries, cancellation |
| `ScheduleStore` | `InMemoryScheduleStore` | Cron, one-shot, and interval schedules with a revision compare-and-swap |
| `GovernanceStateStore` | `InMemoryGovernanceStateStore` | Freeze state per organisation, tenant, and agent |
| `ActionLedger` | `InMemoryActionLedger` | Begun and completed non-idempotent tool calls |
| `ApprovalStore` | `InMemoryApprovalStore` | Pending approvals, decisions, and delegations |
| `SpendLedger` | `InMemorySpendLedger` | Spend reservations and committed spend |
| `AuditLogStore` | `InMemoryAuditLogStore` | Append-only audit logs and checkpoints |

`NewRun::with_service_subject` marks a scheduled, webhook, or queue run as acting for a service. `schedule::plan_fires` computes fire times; cron specs need the `cron` feature. `GovernanceControl::mirrored` mirrors a shared `GovernanceStateStore`, so a freeze on one replica reaches every replica.

```rust
use std::time::Duration;

use adk_core::{DurableRunStore, InMemoryRunStore, NewRun, RunOutcome};
use serde_json::json;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let runs = InMemoryRunStore::new();
    runs.enqueue(NewRun::new("default", "app", "user", "agent", json!({}))).await?;
    let claimed = runs.claim("default", "worker-1", Duration::from_secs(30)).await?.unwrap();
    let done = runs
        .finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!("ok")))
        .await?;
    assert!(done.state.is_terminal());
    Ok(())
}
```

### Store Conformance Suites

The `conformance` feature provides one suite per durable store trait. A backend implements a harness that opens it and calls the matching `*_conformance_tests!` macro, which generates one `#[tokio::test]` per contract case: accounting, period rollover, fencing, single-winner concurrency across two handles, and restart survival. Every in-memory and `adk-session` backend runs the same suites.

```toml
[dev-dependencies]
adk-core = { version = "3.0.0", features = ["conformance"] }
```

## Wake-ups and Webhooks

| Module | Feature | Provides |
|--------|---------|----------|
| `webhook` | `webhooks` | Standard Webhooks `WebhookSigner` / `WebhookVerifier` (timestamp tolerance, several secrets for rotation), `WebhookReplayGuard` |
| `webhook_delivery` | `webhook-delivery` | `WebhookDelivery`: signed outbound POSTs with exponential backoff and jitter, a `DeadLetterSink`, and an `EgressPolicy` that refuses internal destinations at resolve time |
| `queue_source` | — | `QueueSource` (pull, ack, nack), `InMemoryQueueSource`, and `QueueIngress`, which enqueues each message as a durable run keyed by its message id and acks only after the enqueue succeeds |

`queue_source_conformance_tests!` (feature `conformance`) checks any `QueueSource`. See [Wake-ups and Webhooks](../docs/official_docs/core/wake-ups.md).

## State Management

State keys use prefixes for scope:

| Prefix | Scope |
|--------|-------|
| `user:` | The user, across sessions |
| `app:` | The application, across users |
| `temp:` | The current invocation; session services do not persist it |

```rust
use adk_core::{Session, ToolContext};
use serde_json::json;

fn read_and_write(session: &dyn Session, ctx: &dyn ToolContext) {
    let _preference = session.state().get("user:preference");

    let mut actions = ctx.actions();
    actions.state_delta.insert("temp:counter".to_string(), json!(42));
    ctx.set_actions(actions);
}
```

`validate_state_key()` rejects keys that are empty, longer than `MAX_STATE_KEY_LEN` (256 bytes), or that contain `/`, `\`, `..`, or a null byte.

```rust
use adk_core::validate_state_key;

assert!(validate_state_key("user_name").is_ok());
assert!(validate_state_key("../etc/passwd").is_err());
```

### Instruction Templates

`inject_session_state` fills `{var}` (required), `{var?}` (optional), prefixed `{user:var}` / `{app:var}` / `{temp:var}`, and `{artifact.name}` placeholders. `${x}`, `\{x}`, and `{{x}}` stay literal, so instructions can quote shell, JavaScript, and template code.

## Callbacks

| Alias | Receives | Returns |
|-------|----------|---------|
| `BeforeAgentCallback` / `AfterAgentCallback` | `Arc<dyn CallbackContext>` | `Result<Option<Content>>` |
| `BeforeModelCallback` | context, `LlmRequest` | `Result<BeforeModelResult>` (`Continue(request)` or `Skip(response)`) |
| `AfterModelCallback` | context, `LlmResponse` | `Result<Option<LlmResponse>>` |
| `BeforeToolCallback` / `AfterToolCallback` | `Arc<dyn CallbackContext>` | `Result<Option<Content>>` |
| `AfterToolCallbackFull` | context, tool, arguments, response | `Result<Option<Value>>` |
| `OnToolErrorCallback` | context, tool, arguments, error message | `Result<Option<Value>>` |
| `InstructionProvider` / `GlobalInstructionProvider` | `Arc<dyn ReadonlyContext>` | `Result<String>` |

Each alias boxes an async closure: `Box<dyn Fn(..) -> Pin<Box<dyn Future<Output = ..> + Send>> + Send + Sync>`. `InvocationHooks` carries run-wide hooks (the runner's plugins) that agents run ahead of their own callbacks.

## Context Compaction

`EventsCompactionConfig` configures compaction: `compaction_interval` (compaction runs when the session's user-message count is a multiple of it), `overlap_size` (the most recent invocations left out of the summary), and a `summarizer` implementing `BaseEventsSummarizer`. Compaction never covers a call still held for confirmation (`held_call_compaction_limit`). `IntraCompactionConfig` triggers compaction within an invocation when the estimated token count passes a threshold.

## Streaming Modes

`StreamingMode` is `None` (complete responses), `SSE` (the default), or `Bidi` (realtime).

## ToolExecutionStrategy

| Strategy | Dispatch |
|----------|----------|
| `Sequential` (default) | One call at a time, in order |
| `Parallel` | Every call concurrently; does not inspect tool metadata |
| `Auto` | In order; adjacent read-only, concurrency-safe calls overlap |

Set per agent with `LlmAgentBuilder::tool_execution_strategy()`.

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `ToolConfirmationRequest::fingerprint()` names the acting agent and the tool's `ToolOrigin` | Take fingerprints from the held call's request or `ToolConfirmationRequest::for_tool` |
| `ErrorCategory` gains `Conflict` (409) and `ResourceExhausted` (429) | Add match arms |
| `RunConfig` gains `acting`, `action_ledger`, `approval_store`, `approval_service`, `audit`, `budget`, `budget_tracker`, `governance`, `invocation_hooks`, `llm_spend_estimate`, `spend_ledger`, `tool_approvals`, `tool_policy`, `tool_confirmation_timeout`, and `deny_on_confirmation_timeout` | End struct literals with `..Default::default()` |
| `ToolApproval` carries `edited_args` and `message` and is no longer `Copy` | Build it with `approve()` / `deny()` |
| `Tool` gains `effect()`, `timeout_override()`, `origin()`, and `annotations()`; `Agent` gains `close()`, `screen_user_input()`, and transfer flags | Wrappers forward them |
| `LlmResponse` gains `model` and `provider`; `UsageMetadata` gains `builtin_tool_uses` | End struct literals with `..Default::default()` |
| `ActingContext` no longer carries an invocation id | Read it from the execution identity |
| `inject_session_state` leaves `${x}`, `\{x}`, and `{{x}}` literal | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate with every component
- [adk-agent](https://crates.io/crates/adk-agent) — agent implementations
- [adk-model](https://crates.io/crates/adk-model) — model providers
- [adk-tool](https://crates.io/crates/adk-tool) — tool implementations
- [adk-runner](https://crates.io/crates/adk-runner) — the execution runtime

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
