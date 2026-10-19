# Gemini Interactions API (Beta)

ADK-Rust provides a dedicated client for Google's [Interactions API](https://ai.google.dev/gemini-api/docs/interactions-overview) — Google's new direction for the Gemini API. It replaces the `generateContent` request/response shape with a stateful `Interaction` resource built around a typed step timeline, server-side history, and native agentic workflows.

The Interactions API is **beta**. Google recommends `generateContent` for stable production workloads and may make breaking changes to the Interactions schema. ADK-Rust pins the `Api-Revision: 2026-05-20` (steps schema) contract.

## Overview

| Item | Value |
|------|-------|
| Endpoint | `POST /v1beta/interactions` |
| Builder | `Gemini::create_interaction()` |
| Feature | `interactions` on `adk-gemini`; `gemini-interactions` on `adk-model` and `adk-rust` |
| Capabilities | Single-turn and streaming (`step.delta` events), server-side history through `previous_interaction_id`, a typed step timeline, multimodal input, structured output, client-side function calling and built-in server tools, background interactions, and get / delete / cancel of a stored interaction |

## When to use which API

| Aspect | `generateContent` (`GeminiModel`) | Interactions API (`create_interaction`) |
|--------|-----------------------------------|------------------------------------------|
| Endpoint | `POST /v1beta/models/{model}:generateContent` | `POST /v1beta/interactions` |
| Stability | Stable, recommended for production | Beta, schema may change |
| History | Client resends full transcript | Server-side via `previous_interaction_id` |
| Response shape | `candidates` + `parts` | `steps` timeline |
| Agent runtime (`Llm` trait) | ✅ default transport | ✅ opt-in via `use_interactions_api` |
| New models / tools | — | Launch here first |

The ADK agent runtime (the `Llm` trait, tool loop, and `Runner`) uses `generateContent` by default. You can also drive the Interactions API through the same runtime by flipping `use_interactions_api(true)` on a `GeminiModel` — see [Interactions as a runtime transport](#interactions-as-a-runtime-transport-agents--runner) below. The direct client (documented first) remains available for callers who want server-side history, observable steps, or beta-only models without involving an agent.

## Enabling

```toml
# Direct client (adk-gemini)
adk-gemini = { version = "3.0.0", features = ["interactions"] }

# Through the model facade / umbrella
adk-model = { version = "3.0.0", features = ["gemini-interactions"] }
adk-rust  = { version = "3.0.0", features = ["gemini-interactions"] }
```

The feature adds no dependencies and leaves the `generateContent` API unchanged.

## Quick start

```rust
use adk_gemini::{Gemini, Model, ThinkingLevel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gemini = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let interaction = gemini
        .create_interaction()
        .model(Model::gemini_3_7_flash())
        .system_instruction("You are concise.")
        .input_text("What is the capital of France?")
        .thinking_level(ThinkingLevel::Low)
        .send()
        .await?;

    println!("{}", interaction.output_text().unwrap_or_default());
    Ok(())
}
```

## Streaming

When streaming, the API emits a step-oriented SSE event model. The most common
path is accumulating text fragments from `step.delta` events:

```rust
use adk_gemini::{Gemini, Model};
use futures::StreamExt;

async fn haiku(gemini: &Gemini) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = gemini
        .create_interaction()
        .model(Model::gemini_3_7_flash())
        .input_text("Write a haiku about Rust.")
        .stream()
        .await?;

    while let Some(event) = stream.next().await {
        if let Some(fragment) = event?.text_delta() {
            print!("{fragment}");
        }
    }
    Ok(())
}
```

Event types: `interaction.created`, `step.start`, `step.delta`, `step.stop`,
`interaction.status_update`, `interaction.completed`, and `error`. Unknown
future events deserialize into `InteractionSseEvent::Other` rather than failing
the stream.

## Server-side multi-turn

Pass a prior interaction's `id` to continue the conversation without resending
history. Note that `tools`, `system_instruction`, and `generation_config` are
interaction-scoped and must be re-specified each turn:

```rust
use adk_gemini::{Gemini, Model};

async fn two_turns(gemini: &Gemini) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let first = gemini
        .create_interaction()
        .model(Model::gemini_3_7_flash())
        .input_text("My favorite color is teal.")
        .send()
        .await?;

    let second = gemini
        .create_interaction()
        .model(Model::gemini_3_7_flash())
        .previous_interaction_id(&first.id)
        .input_text("What is my favorite color?")
        .send()
        .await?;
    Ok(second.output_text())
}
```

## Function calling

The Interactions API surfaces client-side tool calls as `function_call` steps
with a `requires_action` status. Supply results in a follow-up turn:

```rust
use adk_gemini::{Gemini, Model};
use serde_json::json;

async fn weather(gemini: &Gemini) -> Result<(), Box<dyn std::error::Error>> {
    let interaction = gemini
        .create_interaction()
        .model(Model::gemini_3_7_flash())
        .function(
            "get_weather",
            "Get the weather",
            json!({"type": "object", "properties": {"location": {"type": "string"}}}),
        )
        .input_text("Weather in Boston?")
        .send()
        .await?;

    if interaction.status.requires_action() {
        let mut follow_up = gemini
            .create_interaction()
            .model(Model::gemini_3_7_flash())
            .previous_interaction_id(&interaction.id);
        for (call_id, name, _args) in interaction.pending_function_calls() {
            follow_up = follow_up.function_result(call_id, name, json!({"temperature": "72F"}));
        }
        let final_interaction = follow_up.send().await?;
        println!("{}", final_interaction.output_text().unwrap_or_default());
    }
    Ok(())
}
```

## Structured output

```rust
use adk_gemini::{Gemini, Model};
use serde_json::json;

async fn summary(gemini: &Gemini) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let interaction = gemini
        .create_interaction()
        .model(Model::gemini_3_7_flash())
        .input_text("Summarize this article: ...")
        .json_schema(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"]
        }))
        .send()
        .await?;
    Ok(interaction.output_text())
}
```

## Lifecycle

Stored interactions (the server default) can be retrieved, deleted, or cancelled:

```rust
use adk_gemini::Gemini;

async fn lifecycle(gemini: &Gemini, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    // `true` also returns the `user_input` steps.
    let fetched = gemini.get_interaction(id, true).await?;
    if !fetched.status.is_terminal() {
        gemini.cancel_interaction(id).await?; // background interactions only
    }
    gemini.delete_interaction(id).await?;
    Ok(())
}
```

## Status values

`InteractionStatus` mirrors the API lifecycle: `InProgress`, `RequiresAction`,
`Completed`, `Failed`, `Cancelled`, `Incomplete`, `BudgetExceeded`. Use
`is_terminal()` and `requires_action()` for control flow.

## Limitations

The Interactions API does not yet support the Batch API or explicit caching
(server-side implicit caching is available via `previous_interaction_id`). The
ADK agent runtime uses `generateContent` by default; the Interactions API is
available both as the standalone client documented above and as an opt-in
runtime transport (see [below](#interactions-as-a-runtime-transport-agents--runner)).

---

## Interactions as a runtime transport (agents + runner)

Everything above documents the **direct wire client** (`adk_gemini::interactions`)
— a standalone capability you call by hand. This section documents the
**runtime transport** built on top of it: a toggle on `GeminiModel` that lets a
normal `LlmAgent`, `Runner`, tool loop, and sessions drive the Interactions API
with **zero changes to your agent code**.

This mirrors ADK-Python, where `Gemini(model=..., use_interactions_api=True)`
keeps the same `Agent`, runner, and tools.

### generateContent is still the default

`generateContent` remains the default transport. The Interactions API is beta and its
schema may change. The transport is chosen per model; a `GeminiModel` without
`use_interactions_api(true)` uses `generateContent`.

### Enabling the transport

The transport is gated behind the `gemini-interactions` feature (forwarded from
`adk-rust` → `adk-model` → `adk-gemini/interactions`):

```toml
adk-model = { version = "3.0.0", features = ["gemini-interactions"] }
adk-rust  = { version = "3.0.0", features = ["gemini-interactions"] }
```

Flip the switch on the model and wrap it in a normal `LlmAgent` and `Runner` —
nothing else about the agent setup changes:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Build a Gemini model and toggle the Interactions transport.
    //    `use_interactions_api` validates the model id against the allowlist and
    //    returns `Result<Self>`, so it is fallible (`?`).
    let model = GeminiModel::new(std::env::var("GEMINI_API_KEY")?, "gemini-3.7-flash")?
        .use_interactions_api(true)?;

    // 2. Wrap it in a normal LlmAgent — unchanged agent API.
    let agent = Arc::new(
        LlmAgentBuilder::new("assistant")
            .instruction("You are concise.")
            .model(Arc::new(model))
            .build()?,
    );

    // 3. Drive it through the standard Runner.
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "assistant".into(),
            user_id: "user".into(),
            session_id: Some("session-1".into()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("assistant")
        .agent(agent)
        .session_service(sessions)
        .build()?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new("session-1")?,
            Content::new("user").with_text("What is the capital of France?"),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        // Streamed deltas arrive as partial events; the final event carries the whole answer.
        if event.llm_response.partial {
            continue;
        }
        // The server-assigned interaction id is a first-class field on the event.
        if let Some(id) = event.interaction_id() {
            println!("interaction_id = {id}");
        }
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    Ok(())
}
```

### Faithful defaults

The transport defaults to the Interactions API's intended posture, configured
via `InteractionOptions` (re-exported from `adk_model::gemini`):

| Option | Default | Meaning |
|--------|---------|---------|
| `store` | `true` | Interactions are stored server-side so stateful continuation and observability work out of the box. |
| `stateful` | `true` | Multi-turn conversations continue via `previous_interaction_id`; only the current turn's contents are sent when chaining. |
| `background` | `BackgroundMode::AgentTargetsOnly` | `background=true` for agent targets (Deep Research, long-running); `false` for model targets so chat turns stay low-latency. |
| `poll_interval` | `1s` | How often a background interaction is polled until terminal. |

Override any of these with `interaction_options`:

```rust
use adk_model::GeminiModel;
use adk_model::gemini::{BackgroundMode, InteractionOptions};
use std::time::Duration;

fn build(api_key: &str) -> adk_core::Result<GeminiModel> {
    Ok(GeminiModel::new(api_key, "gemini-3.7-flash")?.use_interactions_api(true)?.interaction_options(
        InteractionOptions {
            store: true,
            stateful: true,
            background: BackgroundMode::AgentTargetsOnly,
            poll_interval: Duration::from_millis(500),
        },
    ))
}
```

`BackgroundMode` has three variants: `AgentTargetsOnly` (default), `Always`, and
`Never`.

> When `store` is `false`, the API's incompatibility rules disable stateful
> continuation and background execution; the transport then sends transcript
> input, exactly like generateContent.

### Generation config

The transport forwards `max_output_tokens`, `temperature`, `top_p`, `seed`, and
`stop_sequences`, and the agent's `output_schema` as the response format. The Interactions
`generation_config` has no penalty or log-probability field, so a request that sets
`frequency_penalty`, `presence_penalty`, or `top_logprobs` fails with
`model.unsupported_config` before it is sent.

### Terminal outcomes

A turn ends only on a terminal outcome, in streaming and non-streaming mode:

| Outcome | Result |
|---------|--------|
| `completed` or `requires_action` | The final chunk, with the accumulated function calls on a tool-call turn |
| `failed`, on `interaction.completed` or an `interaction.status_update` | `model.gemini.interactions.failed` |
| `budget_exceeded` | `model.gemini.interactions.budget_exceeded` |
| An `error` event | The mapped stream error |
| A stream that ends before `interaction.completed` | `model.gemini.interactions.unfinished_stream`, `Unavailable` (retryable) |

### Supported targets (allowlist)

The Interactions API supports a fixed set of targets. `use_interactions_api(true)`
validates the model id at configuration time and returns an `AdkError` with
category `InvalidInput` (naming the supported targets) when the id is not on the
allowlist — instead of deferring to an opaque server rejection.

A **model** target sets the request `model` field; an **agent** target sets the
`agent` field.

**Model targets:**

- `gemini-3.7-flash`
- `gemini-3.6-flash`
- `gemini-3.5-flash`
- `gemini-3.5-flash-lite`
- `gemini-3.1-flash-lite`
- `gemini-3.1-pro-preview`
- `gemini-3-flash-preview`
- `gemini-2.5-pro`
- `gemini-2.5-flash`
- `gemini-2.5-flash-lite`
- `lyria-3-clip-preview`
- `lyria-3-pro-preview`

This is the transport's compatibility allowlist, not a recommendation list.
New applications should start with `gemini-3.7-flash`; older and preview IDs
remain listed because the Interactions endpoint still accepts them.

**Agent targets:**

- `deep-research-pro-preview-12-2025`
- `deep-research-preview-04-2026`
- `deep-research-max-preview-04-2026`

```rust
use adk_core::ErrorCategory;
use adk_model::GeminiModel;

fn main() -> adk_core::Result<()> {
    // Unsupported targets fail at configuration time.
    let result = GeminiModel::new("api-key", "gpt-4")?.use_interactions_api(true);
    assert!(matches!(result, Err(error) if error.category == ErrorCategory::InvalidInput));
    Ok(())
}
```

The `InteractionTarget` enum (also re-exported from `adk_model::gemini`)
represents a validated destination if you need to inspect the classification
directly.

### Mixing built-in and custom tools (`bypass_multi_tools_limit`)

The Interactions API **forbids mixing built-in (server-side) tools with custom
function tools in a single request**. To use, say, Google Search alongside your
own function tool, convert the built-in tool into a function-calling tool so the
whole tool set is uniform. This mirrors ADK-Python's
`bypass_multi_tools_limit=True`.

The conversion lives on the `BypassMultiToolsLimit` trait, implemented by the
built-in tool wrappers (`GoogleSearchTool`, `UrlContextTool`,
`GeminiFileSearchTool`). `with_bypass_multi_tools_limit(agent)` takes an internal
single-turn grounded-search agent — an ordinary `LlmAgent` configured with the
built-in tool and a Gemini model — and returns an `Arc<dyn Tool>` that reports
`is_builtin() == false` and runs the built-in behavior internally, returning a
normal function response.

```rust
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_model::GeminiModel;
use adk_tool::{BypassMultiToolsLimit, FunctionTool, GoogleSearchTool};
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize, JsonSchema)]
struct WeatherArgs {
    city: String,
}

fn build(api_key: &str) -> anyhow::Result<LlmAgent> {
    // The grounded-search agent the bypass tool delegates to: a normal LlmAgent
    // with the built-in GoogleSearchTool and a Gemini model.
    let search_agent = Arc::new(
        LlmAgentBuilder::new("grounded-search")
            .instruction("Answer the query using Google Search. Be factual and concise.")
            .model(Arc::new(GeminiModel::new(api_key, "gemini-3.7-flash")?))
            .tool(Arc::new(GoogleSearchTool::new()))
            .build()?,
    );

    // Convert the built-in search tool into a function tool (is_builtin() == false).
    let search_tool = GoogleSearchTool::new().with_bypass_multi_tools_limit(search_agent);

    // A custom function tool to mix alongside it.
    let weather_tool = Arc::new(
        FunctionTool::new("get_weather", "Get the weather for a city.", |_ctx, args| async move {
            Ok(serde_json::json!({"city": args["city"], "forecast": "sunny"}))
        })
        .with_parameters_schema::<WeatherArgs>(),
    );

    // The tool set is now uniform (all function tools), which the Interactions
    // transport accepts.
    let model = GeminiModel::new(api_key, "gemini-3.7-flash")?.use_interactions_api(true)?;
    Ok(LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .tool(search_tool)
        .tool(weather_tool)
        .build()?)
}
```

If you leave a built-in tool **un-bypassed** while mixing it with function tools
under the Interactions transport, request building returns an `AdkError` with
category `InvalidInput` pointing you at `with_bypass_multi_tools_limit`. The
function call id round-trips unchanged through the tool loop, exactly as with
generateContent.

### Stateful continuity and the retention fallback

`interaction_id` is a **first-class field**, not a side channel. Every
`LlmResponse` carries `interaction_id: Option<String>` (populated by the
Interactions transport, `None` otherwise), and `Event` surfaces it via the
`event.interaction_id()` accessor — mirroring ADK-Python's
`event.interaction_id`.

Continuity is provider-neutral. `LlmRequest` carries an additive
`previous_response_id: Option<String>` field that the `LlmAgent` populates from
the most recent event's `interaction_id`. The Interactions transport maps it to
the request's `previous_interaction_id` and sends **only the current turn's
contents** (rather than the full transcript). No Gemini-specific glue lives in
`adk-agent`; the field is unused (a no-op) for generateContent and other
providers.

```text
Turn 1:  request (transcript)        → interaction v1_abc   → event.interaction_id() == "v1_abc"
Turn 2:  request previous_response_id = "v1_abc"
         → previous_interaction_id = "v1_abc", sends only the new turn
         → interaction v1_def        → event.interaction_id() == "v1_def"
```

**Retention-window fallback.** Stored interactions expire, and the server answers an
expired `previous_interaction_id` with `NotFound`.

| Request | Expired `previous_interaction_id` |
|---------|-----------------------------------|
| Non-streaming (`StreamingMode::None` in `RunConfig`) | The request is rebuilt with the full transcript and no `previous_interaction_id`, and sent once more; the `NotFound` is not surfaced |
| Streaming (`StreamingMode::SSE`, the `RunConfig` default) | The `NotFound` error fails the turn |

### Re-exported types

Behind the `gemini-interactions` feature, the following are available from
`adk_model::gemini`:

- `GeminiTransport` — `GenerateContent` (default) or `Interactions`.
- `InteractionOptions` — `store`, `stateful`, `background`, `poll_interval`.
- `BackgroundMode` — `AgentTargetsOnly` (default), `Always`, `Never`.
- `InteractionTarget` — a validated model/agent destination.

The bypass surface lives in `adk-tool` (reachable via `adk_tool` or the umbrella):

- `BypassMultiToolsLimit` trait with `with_bypass_multi_tools_limit(agent)`.
- Implemented by `GoogleSearchTool`, `UrlContextTool`, `GeminiFileSearchTool`.

The additive `LlmResponse.interaction_id` and `LlmRequest.previous_response_id`
core fields are always present (not feature-gated), so the `event.interaction_id()`
accessor compiles regardless of which providers are enabled.
