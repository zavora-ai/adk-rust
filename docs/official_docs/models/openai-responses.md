# OpenAI Responses API

ADK-Rust provides a dedicated client for OpenAI's [Responses API](https://platform.openai.com/docs/api-reference/responses) (`/v1/responses` endpoint) — the successor to the Chat Completions API. The Responses API is the recommended way to use current GPT-5.6 models, including their full reasoning-effort range.

> **Note:** Chat Completions rejects function tools on GPT-5.6 and later while reasoning is on, so `OpenAIClient` sends those tool-carrying requests through the Responses API automatically. See [Tool Calls on GPT-5.6 and Later](./providers.md#tool-calls-on-gpt-56-and-later).

## Overview

| Item | Value |
|------|-------|
| Endpoint | `POST /v1/responses` |
| Client | `OpenAIResponsesClient` (implements `Llm`) |
| Config | `OpenAIResponsesConfig` |
| Feature | `openai` on `adk-model` or `adk-rust`; `openai-ws` adds the WebSocket transport |
| Capabilities | Streaming and non-streaming, reasoning summaries, function tools, OpenAI-hosted tools, `previous_response_id` continuation, structured output through `text.format`, background polling and cancellation |

## When to Use Which Client

| Feature | `OpenAIClient` (Chat Completions) | `OpenAIResponsesClient` (Responses) |
|---------|-----------------------------------|--------------------------------------|
| Endpoint | `/v1/chat/completions` | `/v1/responses` |
| Models | Chat-compatible models | Current GPT and reasoning models |
| Reasoning summaries | Not available | Native support |
| Built-in tools | Not available | Web search, file search, code interpreter, and the other hosted tools |
| Server-side state | Session history | Session history, or `previous_response_id` |
| Structured output | `response_format` | `text.format` |
| Reasoning effort `max` | Rejected | Supported |

Use `OpenAIResponsesClient` for reasoning summaries, hosted tools, server-side state, or
reasoning effort `max`. `OpenAIClient` already sends tool-carrying requests for GPT-5.6 and
later through the Responses API.

---

## Installation

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["openai"] }
adk-model = { version = "3.0.0", features = ["openai"] }
adk-tool = "3.0.0"
anyhow = "1"
dotenvy = "0.15"
schemars = "1"   # tool parameter schemas
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

Or with `adk-model` directly:

```toml
[dependencies]
adk-model = { version = "3.0.0", features = ["openai"] }
```

> **Note:** the `openai` feature enables the `byot` feature of `async-openai` 0.41, and
> Cargo enables a feature for every user of a crate in the build. If your application also
> calls `async-openai` directly, set `stream` on each request yourself: with `byot`,
> `create_stream` no longer sets `stream: true` for you.

Set your API key:

```bash
export OPENAI_API_KEY="sk-..."
```

---

## Quick Start

```rust
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::futures::StreamExt;
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")?;

    // 1. Create the Responses API client
    let config = OpenAIResponsesConfig::new(&api_key, "gpt-5.6-terra");
    let model = Arc::new(OpenAIResponsesClient::new(config)?);

    // 2. Build an agent
    let agent = Arc::new(
        LlmAgentBuilder::new("assistant")
            .instruction("You are a helpful assistant. Be concise.")
            .model(model)
            .build()?,
    );

    // 3. Create a session
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions.create(CreateRequest {
        app_name: "my_app".into(),
        user_id: "user".into(),
        session_id: Some("s1".into()),
        state: HashMap::new(),
    }).await?;

    // 4. Run through the Runner
    let runner = Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(sessions)
        .build()?;

    let message = Content::new("user").with_text("What is the capital of France?");
    let mut stream = runner.run(
        adk_rust::UserId::new("user")?,
        adk_rust::SessionId::new("s1")?,
        message,
    ).await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        // Streamed deltas arrive as partial events; the final event carries the whole answer.
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    print!("{text}");
                }
            }
        }
    }
    println!();
    Ok(())
}
```

---

## Configuration

### Basic Configuration

```rust
use adk_model::openai::OpenAIResponsesConfig;

fn configs() -> [OpenAIResponsesConfig; 3] {
    [
        // API key and model.
        OpenAIResponsesConfig::new("sk-...", "gpt-5.6-luna"),
        // With organization and project.
        OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra")
            .with_organization("org-...")
            .with_project("proj-..."),
        // Custom base URL, for proxies or compatible APIs.
        OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra")
            .with_base_url("https://my-proxy.example.com/v1"),
    ]
}
```

`with_service_tier`, `with_prompt_cache_retention`, and `with_open_responses_mode` (for
Open Responses servers) are also available.

### Reasoning Models

For GPT-5.6 reasoning models, configure reasoning effort and summary:

```rust
use adk_model::openai::{
    OpenAIReasoningEffort, OpenAIResponsesClient, OpenAIResponsesConfig, ReasoningSummary,
};

fn build() -> Result<OpenAIResponsesClient, adk_core::AdkError> {
    let config = OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra")
        .with_reasoning_summary(ReasoningSummary::Detailed);
    OpenAIResponsesClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Max)
}
```

| Reasoning Effort | Description |
|-----------------|-------------|
| `None` | Disable reasoning for the lowest latency |
| `Minimal` | Legacy minimal reasoning on models that support it |
| `Low` | Low reasoning effort |
| `Medium` | Balanced reasoning |
| `High` | High reasoning effort |
| `XHigh` | Extra-high reasoning effort |
| `Max` | Maximum reasoning on supported models |

Availability depends on the model. `OpenAIClient` (Chat Completions) rejects `Max`.

| Reasoning Summary | Description |
|------------------|-------------|
| `Auto` | Model decides whether to include a summary |
| `Concise` | Brief summary of reasoning |
| `Detailed` | Thorough summary of reasoning |

Reasoning summaries appear as `Part::Thinking` in the response stream, letting you show the model's thought process to users.
On the next request the reasoning goes back only as the reasoning item the history keeps
beside the summary, placed ahead of the output it produced; the summary text is never replayed
as an assistant message.

### Retry Configuration

```rust
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::retry::RetryConfig;

fn build(config: OpenAIResponsesConfig) -> Result<OpenAIResponsesClient, adk_core::AdkError> {
    Ok(OpenAIResponsesClient::new(config)?.with_retry_config(RetryConfig::default().with_max_retries(5)))
}
```

Retries cover timeouts (408), rate limits (429), server errors (500, 502, 503, 504, 529), and
network failures, waiting at most `RetryConfig::max_delay` (5 seconds by default) or the
`Retry-After` delay up to that bound.
`RetryConfig` is the only retry policy: the client does not add async-openai's own retry
layer, so `RetryConfig::disabled()` sends each request once.

### Request Adapters

`with_request_adapter` edits the JSON body and headers of every generation request before
it is sent, including each retry attempt. The adapter must not perform I/O or change the
response mode:

```rust
use std::sync::Arc;

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig, RequestAdapter};

fn build(api_key: String) -> Result<OpenAIResponsesClient, adk_core::AdkError> {
    let adapter: RequestAdapter = Arc::new(|body, headers| {
        body["metadata"] = serde_json::json!({"application": "support-bot"});
        headers.insert("x-request-source", "support-bot".parse().expect("valid header"));
        Ok(())
    });
    Ok(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key, "gpt-5.6-terra"))?
        .with_request_adapter(adapter))
}
```

The client does not follow HTTP redirects on generation requests.

### Response Header Observers

`with_response_observer` receives the raw response headers of every generation request,
for example to read rate-limit or request-id headers:

```rust
use std::sync::Arc;

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig, ResponseObserver};

fn build(api_key: String) -> Result<OpenAIResponsesClient, adk_core::AdkError> {
    let observer: ResponseObserver = Arc::new(|headers| {
        if let Some(remaining) = headers.get("x-ratelimit-remaining-requests") {
            tracing::debug!(remaining = ?remaining, "responses rate limit");
        }
    });
    Ok(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key, "gpt-5.6-terra"))?
        .with_response_observer(observer))
}
```

| Behaviour | Detail |
|-----------|--------|
| **Frequency** | Once per HTTP attempt — a request retried `N` times calls it `N + 1` times |
| **Timing** | Before status handling or body consumption, so failed attempts are observed too |
| **Scope** | `POST /responses` generation requests only; not background polling, cancellation, or compaction |
| **Contract** | Must not panic or block; headers may include cookies, so do not log the whole map |

---

## Available Models

| Model | Catalog role |
|-------|--------------|
| `gpt-5.6-terra` | Default, balanced |
| `gpt-5.6-sol` | Flagship |
| `gpt-5.6-luna` | Economy |
| `gpt-5.6` | Flagship |

The client sends any other model ID as given, such as `gpt-4.1-nano` or `o4-mini` in
`examples/openai_responses`.

---

## Features

### Tool Calling

Function tools work the same way as with `OpenAIClient` — define tools on the agent and the
runner handles the tool call loop. Give each tool a parameter schema
(`with_parameters_schema`), or the model has no parameters to fill:

```rust
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::prelude::*;
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize, JsonSchema)]
struct WeatherArgs {
    /// City name, such as "Paris".
    city: String,
}

async fn get_weather(
    _ctx: Arc<dyn ToolContext>,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    let city = args["city"].as_str().unwrap_or("unknown");
    Ok(serde_json::json!({
        "city": city,
        "temperature_f": 72,
        "conditions": "Sunny"
    }))
}

fn build(api_key: &str) -> anyhow::Result<LlmAgent> {
    let weather_tool = FunctionTool::new("get_weather", "Get current weather for a city.", get_weather)
        .with_parameters_schema::<WeatherArgs>();
    let model = OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key, "gpt-5.6-terra"))?;

    Ok(LlmAgentBuilder::new("weather_agent")
        .instruction("Use the get_weather tool to answer weather questions.")
        .model(Arc::new(model))
        .tool(Arc::new(weather_tool))
        .build()?)
}
```

Converted function declarations are sent with `strict: false`, so optional parameters stay
optional. An agent's `output_schema` is sent as a strict `text.format` `json_schema`.

### Multi-Turn Conversations

The Runner automatically manages conversation history through sessions. Each turn's context is preserved:

```rust
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::{SessionId, UserId};

async fn two_turns(runner: &Runner, uid: UserId, sid: SessionId) -> anyhow::Result<()> {
    for text in ["My name is Alice.", "What is my name?"] {
        let message = Content::new("user").with_text(text);
        let mut stream = runner.run(uid.clone(), sid.clone(), message).await?;
        while let Some(event) = stream.next().await {
            event?;
        }
    }
    Ok(())
}
```

### Per-Request Reasoning Override

Override reasoning settings per-request using LlmRequest extensions:

```rust
use adk_rust::GenerateContentConfig;
use adk_rust::prelude::*;
use std::sync::Arc;

fn build(model: Arc<dyn Llm>) -> anyhow::Result<LlmAgent> {
    let mut config = GenerateContentConfig::default();
    config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({ "reasoning": { "effort": "high", "summary": "detailed" } }),
    );
    Ok(LlmAgentBuilder::new("flexible_reasoner")
        .model(model)
        .generate_content_config(config)
        .build()?)
}
```

The `openai` extension also takes `previous_response_id`, `include`, `max_tool_calls`,
`background`, `conversation_id`, `prompt_id`, `prompt_cache_retention`, `service_tier`, and
`built_in_tools`.

### Built-In Tools

The Responses API supports OpenAI-hosted tools. Prefer the typed wrappers from `adk-tool`:

```rust
use adk_rust::prelude::*;
use adk_tool::OpenAIWebSearchTool;
use std::sync::Arc;

fn build(model: Arc<dyn Llm>) -> anyhow::Result<LlmAgent> {
    Ok(LlmAgentBuilder::new("researcher")
        .model(model)
        .tool(Arc::new(OpenAIWebSearchTool::new()))
        .build()?)
}
```

The wrappers are `OpenAIWebSearchTool` (`.preview()` selects `web_search_preview`),
`OpenAIFileSearchTool`, `OpenAICodeInterpreterTool`, `OpenAIImageGenerationTool`,
`OpenAIComputerUseTool`, `OpenAIMcpTool`, `OpenAILocalShellTool`, `OpenAIShellTool`, and
`OpenAIApplyPatchTool`. A run with a `ToolPolicy` or a tool-call budget evaluates them before
each request and leaves out the ones it denies; see
[Tool Authorization](../security/tool-authorization.md).

### Previous Response ID

For server-side conversation state (bypassing local session history), pass `previous_response_id`:

```rust
use adk_rust::GenerateContentConfig;
use adk_rust::prelude::*;
use std::sync::Arc;

fn build(model: Arc<dyn Llm>) -> anyhow::Result<LlmAgent> {
    let mut config = GenerateContentConfig::default();
    config
        .extensions
        .insert("openai".to_string(), serde_json::json!({ "previous_response_id": "resp_abc123" }));
    Ok(LlmAgentBuilder::new("stateful").model(model).generate_content_config(config).build()?)
}
```

---

## Streaming Behavior

| Output | How it arrives |
|--------|----------------|
| Text | `Part::Text` deltas with `partial: true`; text that arrives only in a done event or the terminal response is added once |
| Reasoning summaries and raw reasoning (`response.reasoning_text.*`) | `Part::Thinking` deltas |
| Refusals | The response text, streamed or not |
| Function calls | In the final response, from the terminal event or, when the terminal response leaves them out, from the `response.output_item.done` events |
| Final response | `turn_complete: true`, usage, and finish reason |
| Unknown event types | Skipped; a known event with missing fields fails the turn |

When the stream needed correcting, carried hosted tools, or held commentary (phase-tagged
progress messages, sent as `Part::ServerToolCall` messages with `"phase": "commentary"`), the
final response sets `provider_metadata.content_complete` to `true`: it carries the complete
output, so replace the earlier deltas with it. Requests with hosted tools stream like any other.

---

## Provider Metadata

Every response includes provider metadata with the `response_id`:

```rust
use adk_core::LlmResponse;

fn response_id(response: &LlmResponse) -> Option<&str> {
    response.provider_metadata.as_ref()?["openai"]["response_id"].as_str()
}
```

The `openai` object also carries `status`, `service_tier` when the response names one,
`encrypted_content` from reasoning items, `built_in_tool_outputs`, and `logprobs` when the
request set `top_logprobs`.

---

## Error Handling

Errors are mapped to structured `AdkError` with appropriate categories:

| HTTP Status | Error Category | Retryable |
|-------------|---------------|-----------|
| 401 | `Unauthorized` | No |
| 403 | `Forbidden` | No |
| 404 | `NotFound` | No |
| 408 | `Timeout` | Yes |
| 429 | `RateLimited` | Yes |
| 500, 502, 503, 504, 529 | `Unavailable` | Yes |
| Other 4xx | `InvalidInput` | No |
| Other | `Internal` | No |

HTTP errors carry the upstream status in `details.upstream_status_code` and an
identifier-like `x-request-id` in `details.request_id`. A `Retry-After` header, in
seconds or as an HTTP-date, sets the retry delay, capped at `RetryConfig::max_delay`.
An error body that is not a JSON error object is quoted, up to 512 bytes, in the message.
`details.metadata["provider_error_code"]` holds the error object's identifier-like `code`, then
its `type`, then its `status`. A request whose response headers or body stall past the idle
timeout fails with `ErrorCategory::Timeout` (`model.openai_responses.request`), and a stalled
stream with `model.openai_responses.stream`, also `Timeout`.

A body or stream event that does not decode fails with `model.openai_responses.parse` and the
message `OpenAI Responses API parse error`; `details.metadata["openai_responses_parse"]`
describes the failure by layer (`stream_setup`, `stream_event`, `response_body`), serde
category, line, and column, without quoting the response.

The generation config fields the API has no place for (`stop_sequences`, `seed`,
`frequency_penalty`, `presence_penalty`) fail with `model.unsupported_config` before the
request is sent; see [Generation Config Fields](./providers.md#generation-config-fields).

### Failed and unfinished responses

A response that does not complete successfully ends the turn with an error, in streaming
and non-streaming mode alike:

| Outcome | Error code | Category |
|---------|------------|----------|
| `failed` status, `response.failed`, or an `error` event | `model.openai_responses.response_failed` | From the provider code, below |
| `cancelled` status or `response.cancelled` | `model.openai_responses.cancelled` | `Cancelled` |
| `incomplete` for a reason other than `max_output_tokens` or `content_filter` | `model.openai_responses.incomplete` | `Internal` |
| Stream ends without a terminal event | `model.openai_responses.unfinished_stream` | `Unavailable` (retryable) |
| Terminal response that lacks a tool call the stream announced | `model.openai_responses.unfinished_stream` | `Internal` |
| Unfinished, duplicate, or unnamed tool call | `model.openai_responses.invalid_terminal` or `invalid_identity` | `Internal` |

A failure whose code is one of OpenAI's documented `response.error.code` values keeps it
in `details.metadata["provider_error_code"]` and in the message, and sets the category:

| Provider code | Category | Retryable |
|---------------|----------|-----------|
| `rate_limit_exceeded` | `RateLimited` | Yes |
| `server_error` | `Unavailable` | Yes |
| `vector_store_timeout` | `Timeout` | Yes |
| `invalid_prompt`, `invalid_image*`, `image_*`, and the other image input codes | `InvalidInput` | No |

Any other code, and every provider message, is dropped: a compatible provider's error
text can echo request content.

```rust
use adk_core::AdkError;

fn describe(error: &AdkError) -> &'static str {
    if error.is_retryable() {
        "transient: send again"
    } else if error.is_unauthorized() {
        "check the API key"
    } else {
        "fix the request"
    }
}
```

---

## WebSocket Transport

With the `openai-ws` feature, `WsTransport` sends Responses requests over one persistent
WebSocket connection ([WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode))
instead of one HTTP request each. It is used directly, not as an `Llm`, and yields the same
`LlmResponse` chunks as the HTTP stream: partial deltas, then one final chunk with the rest
of the response.

```toml
[dependencies]
adk-model = { version = "3.0.0", features = ["openai-ws"] }
anyhow = "1"
async-openai = { version = "0.41", default-features = false, features = ["rustls", "responses"] }
futures = "0.3"
tokio = { version = "1", features = ["full"] }
```

```rust
use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
use async_openai::types::responses::{CreateResponseArgs, InputParam};
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = OpenAIResponsesConfig::new(std::env::var("OPENAI_API_KEY")?, "gpt-5.6-terra");
    let transport = WsTransport::connect(&config).await?;
    let request = CreateResponseArgs::default()
        .model("gpt-5.6-terra")
        .input(InputParam::Text("What is the capital of France?".into()))
        .build()?;
    let mut stream = transport.send_request(request).await?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        for part in chunk.content.iter().flat_map(|content| &content.parts) {
            if let Some(text) = part.text() {
                print!("{text}");
            }
        }
    }
    Ok(())
}
```

| Situation | Behaviour |
|-----------|-----------|
| Request frame | Sent as a `response.create` event; `stream` and `background` are HTTP fields and are not sent |
| Requests on one transport | One response at a time; a request waits until the previous stream ends or is dropped |
| Stream dropped before its terminal event | The connection is discarded and the next request opens a new one, so no frame of the abandoned response reaches it |
| Connection the server closed while idle | The next request reconnects |
| Close or end of the connection before a terminal event | `model.openai_responses.unfinished_stream`, `Unavailable` |
| `response.incomplete` at `max_output_tokens` or `content_filter` | A final chunk with `FinishReason::MaxTokens` or `FinishReason::Safety` |
| Other `incomplete`, `response.cancelled`, or `response.failed` | The errors in [Failed and unfinished responses](#failed-and-unfinished-responses) |
| An event, including a completed response, that does not match the Responses schema | `model.openai_responses.parse` |
| `error` event | `model.openai_responses.ws_error`, categorized by the event's documented code or its `status` |
| Event type this version does not know, carrying a terminal `response.status` | Ends the response by that status |
| Handshake | Retried under `RetryConfig` for network errors, 408, 429, and 5xx; any other 4xx (for example 401) and an invalid URL fail at once with the status's category |

`WsTransport::connect` retries handshakes under `RetryConfig::default()`;
`WsTransport::connect_with_retry_config` takes the policy, which also governs reconnects.
A response continued with `previous_response_id` after a reconnect needs `store = true`:
the connection-local cache of an unstored response is lost with the old connection.

`OpenAIResponsesClient` always uses HTTP. The former `OpenAIResponsesConfig::with_transport`
option and the `ResponsesTransport` enum were never read and are removed; use `WsTransport`
for WebSocket mode.

`examples/gemini_ws_outcomes` runs each WebSocket case against a scripted local server and,
when `OPENAI_API_KEY` is set, against the live endpoint:

```bash
cargo run --manifest-path examples/gemini_ws_outcomes/Cargo.toml
```

---

## Background Mode & Cancellation

A request with `"background": true` in its `openai` extension is submitted with
`background: true`; a model whose ID contains `deep-research` (`o3-deep-research`,
`o4-mini-deep-research`) always is. The client polls and cancels a background response by ID:

```rust
use adk_model::openai::OpenAIResponsesClient;

async fn wait(client: &OpenAIResponsesClient, response_id: &str) -> anyhow::Result<()> {
    loop {
        let response = client.poll_response(response_id).await?;
        let status = response
            .provider_metadata
            .as_ref()
            .and_then(|metadata| metadata["openai"]["status"].as_str())
            .unwrap_or_default()
            .to_string();
        if status != "queued" && status != "in_progress" {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn stop(client: &OpenAIResponsesClient, response_id: &str) -> anyhow::Result<()> {
    client.cancel_response(response_id).await?;
    Ok(())
}
```

| Method | Request | Returns |
|--------|---------|---------|
| `poll_response` | `GET /responses/{id}` | A completed or incomplete response converted like any other; for `queued` and `in_progress`, a response with `response_id` and `status` in `provider_metadata["openai"]` and no content; for `failed`, `error_code` and `error_message` set |
| `cancel_response` | `POST /responses/{id}/cancel` | A response with `status: "cancelled"` in `provider_metadata`; an error when the response is not a background response or already finished |

---

## Example

`examples/openai_responses` runs seven scenarios:

```bash
export OPENAI_API_KEY=sk-...
cargo run --manifest-path examples/openai_responses/Cargo.toml
```

| Scenario | Model |
|----------|-------|
| 1. Non-streaming chat | `gpt-4.1-nano` |
| 2. Streaming chat | `gpt-4.1-nano` |
| 3. Reasoning with a summary | `o4-mini` |
| 4. Function tool calling | `gpt-4.1-nano` |
| 5. Multi-turn conversation | `gpt-4.1-nano` |
| 6. System instruction | `gpt-4.1-nano` |
| 7. Temperature and `top_p` | `gpt-4.1-nano` |

### Additional Examples

| Example | Run Command | Feature |
|---------|-------------|---------|
| WebSocket transport | `cargo run --manifest-path examples/openai_ws_minimal/Cargo.toml` | Persistent connection |
| Background mode | `cargo run --manifest-path examples/openai_background/Cargo.toml` | Submit and poll |
| Conversations API | `cargo run --manifest-path examples/openai_conversations/Cargo.toml` | Server-managed multi-turn |
| Built-in tools | `cargo run --manifest-path examples/openai_builtin_tools/Cargo.toml` | Image generation, web search |
| Deep research | `cargo run --manifest-path examples/openai_deep_research/Cargo.toml` | Background research |
| Open Responses | `cargo run --manifest-path examples/openai_open_responses/Cargo.toml` | Provider-agnostic endpoints |

---

## Related

- [Cloud Model Providers](./providers.md) — All supported LLM providers
- [Ollama (Local)](./ollama.md) — Run models locally
- [LlmAgent](../agents/llm-agent.md) — Using models with agents
- [Function Tools](../tools/function-tools.md) — Adding tools to agents

---

**Previous**: [← Cloud Providers](./providers.md) | **Next**: [Ollama (Local) →](./ollama.md)
