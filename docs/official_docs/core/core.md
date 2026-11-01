# Core Types

Fundamental types and traits from `adk-core` that every other ADK-Rust crate builds on.

## Content and Part

Every message in ADK flows through `Content` and `Part`.

### Content

`Content` is one message: a `role` and one or more `parts`.

| Role | Sent by |
|------|---------|
| `"user"` | The user |
| `"model"` | The model |
| `"system"` | Agent instructions; `LlmAgent` sends its instruction, global instruction, and output-schema directive with this role, and providers with a system parameter receive them there |
| `"function"` | Tool results that `LlmAgent` records |

```rust
use adk_core::Content;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A text message from the user.
    let user_msg = Content::new("user").with_text("What's the weather like?");

    // Several text parts in one message.
    let detailed = Content::new("user")
        .with_text("Here's my question:")
        .with_text("What is the capital of France?");

    // An image from bytes, and one by reference.
    let image_bytes = std::fs::read("photo.jpg")?;
    let image = Content::new("user")
        .with_text("What's in this image?")
        .with_inline_data("image/jpeg", image_bytes);
    let remote = Content::new("user")
        .with_text("Describe this chart")
        .with_file_uri("image/png", "https://example.com/chart.png");

    let _ = (user_msg, detailed, image, remote);
    Ok(())
}
```

`with_inline_data` panics when the data exceeds `MAX_INLINE_DATA_SIZE` (10 MB).

### Part

`Part` is an enum with one variant per kind of content:

| Variant | Fields | Carries |
|---------|--------|---------|
| `Text` | `text` | Plain text |
| `Thinking` | `thinking`, `signature` | A reasoning trace; the signature is replayed to providers that require it |
| `InlineData` | `mime_type`, `data`, `uri`, `annotations` | Binary data in the message |
| `FileData` | `mime_type`, `file_uri`, `annotations` | A reference to a file by URL or cloud storage path |
| `FunctionCall` | `name`, `args`, `id`, `thought_signature` | A tool call from the model |
| `FunctionResponse` | `function_response` (`FunctionResponseData`), `id`, `annotations` | A tool result |
| `ServerToolCall` | `server_tool_call` | A provider-executed tool call (Gemini code execution, OpenAI hosted tools, ...) |
| `ServerToolResponse` | `server_tool_response` | Its result |
| `EmbeddedResource` | `resource` | A source URI plus inline text or binary contents, as in an MCP or ACP embedded-resource block |

`Part` is not `#[non_exhaustive]`, but a `match` that only reads some variants needs a
`_ => {}` arm:

```rust
use adk_core::{Content, Part};

fn describe(content: &Content) {
    for part in &content.parts {
        match part {
            Part::Text { text } => tracing::info!(%text, "text"),
            Part::InlineData { mime_type, data, .. } => {
                tracing::info!(%mime_type, bytes = data.len(), "binary data")
            }
            Part::FileData { mime_type, file_uri, .. } => tracing::info!(%mime_type, %file_uri, "file"),
            Part::FunctionCall { name, args, .. } => tracing::info!(%name, %args, "tool call"),
            Part::FunctionResponse { function_response, .. } => {
                tracing::info!(name = %function_response.name, "tool result")
            }
            Part::EmbeddedResource { resource } => tracing::info!(uri = resource.uri(), "resource"),
            _ => {}
        }
    }
}
```

| Constructor or accessor | Returns |
|-------------------------|---------|
| `Part::text_part(text)`, `Part::inline_data(mime, bytes)`, `Part::file_data(mime, uri)` | A part of that kind |
| `part.text()` | `Some(&str)` for a text part |
| `part.thinking_text()`, `part.is_thinking()` | The reasoning trace of a thinking part |
| `part.mime_type()`, `part.file_uri()`, `part.uri()` | Media details for inline and file data |
| `part.is_media()` | `true` for inline and file data |
| `part.embedded_resource()` | The resource of an embedded-resource part |

### Provider-native items

`Thinking`, `ServerToolCall`, `ServerToolResponse` and a `FunctionCall`'s `thought_signature`
are provider-native items: reasoning, signatures, encrypted reasoning, server-side tool calls and
results, and preserved provider messages. They are meaningful only to the provider and model that
produced them, and private to the agent whose model produced them.

| Who | Receives native items |
|-----|-----------------------|
| The provider and model that produced them | Yes, in the agent's own model turns |
| Another provider, or another model of the same provider | No |
| Another agent's context (`history::other_agent_context`) | No |
| A compaction summary (`intra_compaction::render_part_for_summary`) | No |
| An A2A peer | No |
| `AgentTool`'s caller | Provider tool results only, never a replay-only copy (`history::is_replay_only`) |
| Any model, from a user turn, a tool result or A2A input | No |

Ownership is recorded where a peer cannot write it:

1. Every `adk-model` client records its `history::ProviderOrigin` — its client family and the
   requested model — in each response's `provider_metadata` under `adk_replay_origin`, and
   `LlmAgent` keeps it on the event.
2. The runner and `LlmAgent` mark each of the agent's own model turns with that origin
   (`history::mark_origin`), replacing any marker stored in or sent with the content, and remove
   native items from every other turn (`history::strip_native`).
3. Each client keeps native items only in turns its own origin marks
   (`history::retain_replayable`). An unmarked model turn — history built outside the runner, or
   a session written before origins were recorded — keeps them.

```rust
use adk_core::history::{ProviderOrigin, mark_origin, retain_replayable};
use adk_core::{Content, Part};

let anthropic = ProviderOrigin::new("anthropic", "claude-sonnet-5-5");
let mut turn = Content {
    role: "model".into(),
    parts: vec![Part::Thinking { thinking: "plan".into(), signature: Some("sig".into()) }],
};
mark_origin(&mut turn, Some(&anthropic));

let mut for_gemini = vec![turn];
retain_replayable(&mut for_gemini, &ProviderOrigin::new("gemini", "gemini-3.7-flash"));
assert!(for_gemini.is_empty());
```

### Embedded resources

```rust
use adk_core::{BlobResourceContents, Content, EmbeddedResource, TextResourceContents};

fn main() -> adk_core::Result<()> {
    // A text resource: the UTF-8 payload is kept verbatim, never base64-encoded.
    let text = EmbeddedResource::Text(TextResourceContents::new(
        "file:///src/main.rs",
        Some("text/x-rust".to_string()),
        "fn main() {}",
    ));

    // A binary resource: the checked constructor rejects payloads over MAX_INLINE_DATA_SIZE.
    let blob = EmbeddedResource::Blob(BlobResourceContents::new(
        "file:///logo.png",
        Some("image/png".to_string()),
        vec![0x89, 0x50, 0x4E, 0x47],
    )?);

    let content = Content::new("user")
        .with_text("Here is the entry point and the logo:")
        .with_embedded_resource(text)
        .with_embedded_resource(blob);
    assert_eq!(content.parts.len(), 3);
    Ok(())
}
```

`EmbeddedResource` is an untagged enum with `Text(TextResourceContents)` and
`Blob(BlobResourceContents)` variants, each with a required `uri` and an optional `mime_type`.

---

## Agent Trait

Every agent type — LLM agents, workflow agents, custom agents — implements `Agent`.

| Method | Required | Purpose |
|--------|----------|---------|
| `name()` | Yes | Identifies the agent in logs, transfers, and its default identity; unique within one agent tree |
| `description()` | Yes | Shown to models when the agent is a transfer target or a tool |
| `sub_agents()` | Yes | Child agents; `&[]` for a leaf agent |
| `run(ctx)` | Yes | Runs the agent and returns an `EventStream` |
| `close()` | No | Releases what the agent holds; the default closes each sub-agent. `Runner::close` calls it on the root agent |
| `allows_transfer_to_parent()`, `allows_transfer_to_peers()` | No | Transfer restrictions the runner enforces (default `true`) |
| `screen_user_input(content)` | No | Screens a user message with the input guardrails of the agent and every agent below it; the runner calls it on the root agent and saves only what it returns |

```rust
use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Result};
use async_trait::async_trait;
use std::sync::Arc;

struct EchoAgent;

#[async_trait]
impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Repeats the user's message"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name().to_string();
        let text = ctx.user_content().parts.iter().filter_map(|part| part.text()).collect::<String>();
        event.set_content(Content::new("model").with_text(text));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}
```

Agents return an `EventStream` — `Pin<Box<dyn Stream<Item = Result<Event>> + Send>>` — rather
than one response, because a turn streams text, runs tool calls, changes state, and signals
transfers as it goes.

---

## Tool Trait

| Method | Default | Purpose |
|--------|---------|---------|
| `name()`, `description()` | Required | The function name and description the model sees |
| `parameters_schema()` | `None` | JSON Schema of the arguments; without one the model has no parameters to fill |
| `response_schema()` | `None` | JSON Schema of the result |
| `execute(ctx, args)` | Required | Runs the tool |
| `effect()` | `ReadOnly` when `is_read_only()`, otherwise `NonIdempotent` | The side effect class (`ToolEffect`); decides retries, the action ledger, and `DeclarativePolicy::allow_read_only` |
| `is_read_only()`, `is_concurrency_safe()` | `false` | Parallel dispatch under `ToolExecutionStrategy::Auto` |
| `is_long_running()` | `false` | The tool starts work and returns before it finishes |
| `origin()` | `"static"` | Where the tool came from (an MCP server's tools report `mcp:{name}`); part of an approval's binding |
| `required_scopes()` | `&[]` | Scopes the caller must hold |
| `timeout_override()` | `None` | A per-tool timeout instead of the agent's |

```rust
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Get current weather for a city. Use this when the user asks about weather conditions."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "City name (e.g., 'London')" },
                "units": { "type": "string", "enum": ["celsius", "fahrenheit"] }
            },
            "required": ["city"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let city = args["city"].as_str().unwrap_or("Unknown");
        let units = args["units"].as_str().unwrap_or("celsius");
        Ok(json!({ "city": city, "temperature": 22, "units": units, "condition": "sunny" }))
    }
}
```

A tool that fails with `Err(AdkError)` is answered to the model with the error's component,
category, and code, not its message; see [Function Tools](../tools/function-tools.md) for what
the model receives and for `FunctionTool` and `#[tool]`.

---

## Toolset Trait

A `Toolset` provides tools per context — by user, from an MCP server, or changing during a run.

| Method | Default | Purpose |
|--------|---------|---------|
| `name()` | Required | Identifies the toolset |
| `tools(ctx)` | Required | The tools for this context |
| `process_llm_request(...)` | Nothing | Adjusts the model request |
| `close()` | Nothing | Releases what the toolset holds; `Agent::close` calls it |

```rust
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use std::sync::Arc;

struct AdminToolset {
    admin_tools: Vec<Arc<dyn Tool>>,
    user_tools: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Toolset for AdminToolset {
    fn name(&self) -> &str {
        "admin_toolset"
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        if ctx.user_id().starts_with("admin_") {
            Ok(self.admin_tools.clone())
        } else {
            Ok(self.user_tools.clone())
        }
    }
}
```

---

## Context Traits

Contexts give agents, callbacks, and tools what they need while they run. Each trait extends
the one before it.

| Trait | Adds |
|-------|------|
| `ReadonlyContext` | `invocation_id`, `agent_name`, `user_id`, `app_name`, `session_id`, `branch`, `user_content`, `acting_context` (the acting agent; see [Identity](#identity)), `state`, and the typed `try_*` identity accessors |
| `CallbackContext` | `artifacts`, `shared_state`, and, in tool callbacks, `tool_name`, `tool_input`, `tool_outcome` |
| `ToolContext` | `function_call_id`, `idempotency_key`, `actions` / `set_actions`, `search_memory` |
| `InvocationContext` | `agent`, `memory`, `session`, `run_config`, `end_invocation` / `ended`, and capability methods with defaults: `is_cancelled`, `user_scopes`, `request_metadata`, `get_secret`, `get_secret_for`, delegation depth, and transfer details |

A context that wraps another — as workflow agents do to change the branch, the user content,
or the session — must forward every capability method and `acting_context()`; a method it
does not forward falls back to its default (`false`, empty, `None`, or the derived identity)
instead of failing to compile. Every wrapper in the workspace forwards them, and a conformance
suite pushes a context with non-default values through each wrapper.

---

## Session and State

| `Session` method | Returns |
|------------------|---------|
| `id()`, `app_name()`, `user_id()` | The session's address |
| `state()` | The session's `State` |
| `conversation_history()` | The session's contents, as the model sees them |

`State` reads with `get(key)` and `all()`. Agents and tools change state through event
actions, which the runner applies and persists:

```rust
use adk_core::{Result, ToolContext};
use serde_json::{Value, json};
use std::sync::Arc;

async fn remember_topic(ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
    let mut actions = ctx.actions();
    actions.state_delta.insert("current_topic".to_string(), json!("weather"));
    actions.state_delta.insert("user:theme".to_string(), json!("dark"));
    actions.state_delta.insert("temp:step_count".to_string(), json!(1));
    ctx.set_actions(actions);
    Ok(json!({ "saved": true }))
}
```

| Prefix | Constant | Scope |
|--------|----------|-------|
| `user:` | `KEY_PREFIX_USER` | Every session of the user |
| `app:` | `KEY_PREFIX_APP` | Every session of the application |
| `temp:` | `KEY_PREFIX_TEMP` | The current invocation; never persisted |
| (none) | — | This session |

See [Sessions](../sessions/sessions.md) and [State](../sessions/state.md).

---

## Error Handling

`AdkError` is the structured error every ADK operation returns:

```rust
use adk_core::{AdkError, ErrorCategory, ErrorComponent};

fn main() {
    let err = AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::RateLimited,
        "model.openai.rate_limited",
        "Too many requests",
    );

    assert!(err.is_retryable()); // RateLimited, Unavailable, and Timeout are retryable
    assert_eq!(err.http_status_code(), 429);
    let _problem = err.to_problem_json(); // a JSON Problem Details body for HTTP responses

    // Shorthand constructors produce `*.legacy` codes.
    let _legacy = AdkError::model("something went wrong");
}
```

| Category | HTTP | Retryable by default |
|----------|------|----------------------|
| `InvalidInput` | 400 | No |
| `Unauthorized`, `Forbidden` | 401, 403 | No |
| `NotFound` | 404 | No |
| `Timeout` | 408 | Yes |
| `Conflict` | 409 | No |
| `RateLimited` | 429 | Yes |
| `ResourceExhausted` | 429 | No |
| `Cancelled` | 499 | No |
| `Internal` | 500 | No |
| `Unsupported` | 501 | No |
| `Unavailable` | 503 | Yes |

`ResourceExhausted` marks a used-up run, team, or spend budget, and `Conflict` a write that
lost a race or used a stale version (`lease.stale_epoch`, `run_store.stale_epoch`,
`governance.conflict`, `approval.version_conflict`). `AdkError::diagnostics()` displays an
error by everything but its message, for log lines that must not carry request content.

See [Development Guidelines](../development/development-guidelines.md#error-handling) for the
full guide.

---

## Identity

ADK-Rust keeps four identities apart:

| Identity | Type | Answers |
|----------|------|---------|
| Auth | `RequestContext` | Who is authenticated and with which scopes (`user_id`, `scopes`, `metadata`); set at HTTP ingress |
| Session | `AdkIdentity` | Which conversation: `app_name`, `user_id`, `session_id` |
| Execution | `ExecutionIdentity` | Which invocation of the session, on which branch, by which agent |
| Agent | `ActingContext` | Which agent is acting, for which subject, through which delegation chain |

### Session and execution identity

```rust
use adk_core::{AdkIdentity, AppName, ExecutionIdentity, InvocationId, SessionId, UserId};

fn main() -> adk_core::Result<()> {
    let identity = AdkIdentity::new(
        AppName::try_from("weather-app")?,
        UserId::try_from("tenant:alice@example.com")?,
        SessionId::generate(),
    );
    assert_eq!(identity.app_name.as_ref(), "weather-app");

    let exec = ExecutionIdentity {
        adk: identity.clone(),
        invocation_id: InvocationId::generate(),
        branch: String::new(),
        agent_name: "planner".to_string(),
    };
    let _ = exec;
    Ok(())
}
```

`AppName`, `UserId`, `SessionId`, and `InvocationId` share one validation rule: not empty, no
null bytes, at most 512 bytes (`MAX_ID_LEN`). Characters such as `:`, `@`, and `/` are allowed.
All four serialize as plain strings (`#[serde(transparent)]`). `ReadonlyContext` and `Session`
parse them on demand with `try_identity()`, `try_execution_identity()`, `try_app_name()`,
`try_user_id()`, and the other `try_*` accessors, which return an error instead of panicking.

### Agent identity

Every run has an acting agent. `ReadonlyContext::acting_context()` returns an
`ActingContext` (`adk_core::principal`): the agent's `AgentPrincipal` (an `AgentId` with
tenant, class, instance, sponsor, and attributes), the `SubjectRef` it acts for, and the
delegation chain.

| Source of the agent's principal | Applies to |
|---------------------------------|------------|
| `RunConfig::acting` (`ActingConfig::principals` by agent name, `RunConfigBuilder::agent_principal`) | Any agent in the run |
| The runner's `identity_source(...)` (`IdentitySource::current` for the root agent, `principal_for` for any agent) | Any agent |
| `Runner::builder().agent_principal(...)` | The root agent |
| None of these | `urn:adk:agent:{app}/{agent}` (`AgentPrincipal::derived`) |

The subject is always the session's user, as `ActingConfig::subject_kind` (`Human` by
default, `Service` for runs a scheduler, webhook, or queue creates). A run configured to act
for anyone else fails at its start with `identity.subject_mismatch` (`Forbidden`) unless the
runner's `subject_delegation(...)` policy allows it. `AgentTool` delegation appends the
caller to the delegate's chain.

```rust
use adk_core::principal::{ActingContext, AgentId, AgentPrincipal, SubjectRef};
use adk_core::{RunConfig, SubjectKind};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let billing = AgentPrincipal::new(AgentId::new("urn:adk:agent:billing")?).with_tenant("acme");
    let config = RunConfig::builder()
        .agent_principal("billing_agent", billing.clone())
        .subject_kind(SubjectKind::Human)
        .build();
    let _ = config;

    let acting = ActingContext::new(billing).on_behalf_of(SubjectRef::human("bob@example.com"));
    assert_eq!(serde_json::to_value(&acting)?["onBehalfOf"]["kind"], "human");
    Ok(())
}
```

The acting context reaches `ToolPolicyRequest::acting`, `ToolConfirmationRequest::acting`,
`SpendKey::agent`, and audit records. See [Agent Identity](../security/agent-identity.md).

---

## Governance

The governed path and its stores live in `adk-core`, so every runtime — `LlmAgent`,
`CodeActAgent`, the realtime runtimes, the durable worker — enforces them the same way:

| Type | Role | Guide |
|------|------|-------|
| `GovernanceControl` | Kill switch: `freeze(reason)` fails new runs at their start and stops running ones before their next model or tool call; `GovernanceControl::mirrored` replicates it through a `GovernanceStateStore` | [Durable Runtime](durable-runtime.md#replicated-kill-switch) |
| `ToolPolicy`, `DeclarativePolicy` | Allow, deny, or require approval per tool call | [Tool Authorization](../security/tool-authorization.md) |
| `authorize_tool_call` | The governed path: kill switch, policy, guardrails, confirmation, on a call's final arguments | [Tool Authorization](../security/tool-authorization.md) |
| `ApprovalStore`, `ApprovalService` | Recorded decisions for held calls, bound to the call's fingerprint | [Approval Service](../security/approval-service.md) |
| `RunBudget`, `BudgetTracker` | Per-run limits on model calls, tokens, cost, tool calls, and wall time | [Run Budgets](runner.md#run-budgets) |
| `SpendLedger`, `ModelCallScope` | Hard spend caps across runs, reserved before each model call | [Spend Ledger](../security/spend-ledger.md) |
| `ActionLedger` | Records non-idempotent calls so a resumed run never repeats one | [Durable Stores](../security/durable-stores.md) |

```rust
use adk_core::{GOVERNANCE_FROZEN_CODE, GovernanceControl};

fn main() {
    let control = GovernanceControl::new();
    control.freeze("incident 4521");
    let refused = control.check().expect_err("a frozen control refuses work");
    assert_eq!(refused.code, GOVERNANCE_FROZEN_CODE);
    control.unfreeze();
    assert!(control.check().is_ok());
}
```

A runner creates its own control when none is set; `Runner::governance()` returns it.

---

## Events

```rust
use adk_core::{Event, EventStream};
use futures::StreamExt;

async fn print_events(mut stream: EventStream) {
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => print_event(&event),
            Err(error) => {
                tracing::warn!(error = %error.diagnostics(), "run failed");
                break;
            }
        }
    }
}

fn print_event(event: &Event) {
    // Streamed deltas arrive as partial events; the final event carries the whole answer.
    if event.llm_response.partial {
        return;
    }
    if let Some(content) = event.content() {
        for part in &content.parts {
            if let Some(text) = part.text() {
                print!("{text}");
            }
        }
    }
    for (key, value) in &event.actions.state_delta {
        tracing::info!(%key, %value, "state changed");
    }
    if event.is_final_response() {
        println!();
    }
}
```

See [Events](../events/events.md).

---

## Llm Trait

Every model provider implements `Llm`:

| Method | Default | Purpose |
|--------|---------|---------|
| `name()` | Required | The model identifier |
| `generate_content(request, stream)` | Required | Returns an `LlmResponseStream` |
| `schema_adapter()` | `GenericSchemaAdapter` | Normalizes tool schemas for the provider |
| `max_cost_micro_usd(request)` | `None` | An upper bound on the call's cost, for spend reservations |

| `LlmRequest` field | Holds |
|--------------------|-------|
| `model` | The model identifier |
| `contents` | The conversation, instructions included as `system` contents |
| `config` | `Option<GenerateContentConfig>`: temperature, `top_p`, `top_k`, `max_output_tokens`, `stop_sequences`, `seed`, penalties, `top_logprobs`, `response_schema`, `cached_content`, and provider `extensions` |
| `tools` | Tool declarations by name |
| `previous_response_id` | A provider-side continuation id, used by the Gemini Interactions transport |

```rust
use adk_core::{Content, GenerateContentConfig, LlmRequest};
use serde_json::json;

fn main() {
    let request = LlmRequest::new("gemini-3.7-flash", vec![Content::new("user").with_text("Hello!")])
        .with_config(GenerateContentConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(1024),
            ..Default::default()
        })
        .with_response_schema(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        }));
    assert!(request.config.and_then(|config| config.response_schema).is_some());
}
```

| `LlmResponse` field | Holds |
|---------------------|-------|
| `content` | The generated content |
| `usage_metadata` | Token counts, cost, built-in tool uses, and the provider's own usage object |
| `finish_reason` | Why generation stopped (`FinishReason`) |
| `partial`, `turn_complete` | Whether this is a streamed delta, and whether the turn is finished |
| `error_code`, `error_message` | A provider-reported failure, such as a blocked prompt |
| `provider_metadata` | Provider-specific details (grounding, log probabilities, response ids) |
| `model`, `provider` | Which model and provider produced the response |
| `interaction_id`, `citation_metadata`, `interrupted` | Interactions continuity, citations, and realtime interruption |

Clients for each provider are in `adk-model`; see [Model Providers](../models/providers.md).

---

**Previous**: [← Introduction](../introduction.md) | **Next**: [Runner →](runner.md)
