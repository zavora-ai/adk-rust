# OpenAI Responses API

ADK-Rust provides a dedicated client for OpenAI's [Responses API](https://platform.openai.com/docs/api-reference/responses) (`/v1/responses` endpoint) — the successor to the Chat Completions API. The Responses API is the recommended way to interact with OpenAI's latest models, including reasoning models (o3, o4-mini) and GPT-4.1 series.

## Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                  OpenAI Responses API Client                        │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   Endpoint:  POST /v1/responses                                     │
│   Client:    OpenAIResponsesClient                                  │
│   Config:    OpenAIResponsesConfig                                  │
│   Feature:   openai                                                 │
│                                                                     │
│   Capabilities:                                                     │
│   • Streaming and non-streaming                                     │
│   • Reasoning summaries (o-series models)                           │
│   • Tool / function calling                                         │
│   • Multi-turn via previous_response_id                             │
│   • Built-in tools (web search, file search, code interpreter)      │
│   • System instructions                                             │
│   • Temperature, top_p, max_output_tokens                           │
│   • Automatic retry with exponential backoff                        │
│                                                                     │
│   vs Chat Completions (OpenAIClient):                               │
│   • Stateful conversations (server-side context)                    │
│   • Native reasoning summaries                                      │
│   • Built-in tool hosting                                           │
│   • Simpler multi-turn (no manual message history)                  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## When to Use Which Client

| Feature | `OpenAIClient` (Chat Completions) | `OpenAIResponsesClient` (Responses) |
|---------|-----------------------------------|--------------------------------------|
| Endpoint | `/v1/chat/completions` | `/v1/responses` |
| Models | All GPT models | All GPT + o-series reasoning models |
| Reasoning summaries | Not available | Native support |
| Built-in tools | Not available | Web search, file search, code interpreter |
| Server-side state | Manual message history | `previous_response_id` |
| Structured output | `response_format` | `text.format` (planned) |
| Maturity | Stable, widely adopted | Newer, recommended by OpenAI |

Use `OpenAIResponsesClient` when you need reasoning models with summaries, built-in tools, or want to use OpenAI's latest API. Use `OpenAIClient` for backward compatibility with existing Chat Completions workflows.

---

## Installation

```toml
[dependencies]
adk-rust = { version = "0.5.0", features = ["openai"] }
```

Or with `adk-model` directly:

```toml
[dependencies]
adk-model = { version = "0.5.0", features = ["openai"] }
```

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
    let config = OpenAIResponsesConfig::new(&api_key, "gpt-4.1-nano");
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
    let runner = Runner::new(RunnerConfig {
        app_name: "my_app".into(),
        agent,
        session_service: sessions,
        ..Default::default()
    })?;

    let message = Content::new("user").with_text("What is the capital of France?");
    let mut stream = runner.run(
        adk_rust::UserId::new("user")?,
        adk_rust::SessionId::new("s1")?,
        message,
    ).await?;

    while let Some(event) = stream.next().await {
        let event = event?;
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
use adk_model::openai::{OpenAIResponsesConfig, ReasoningEffort, ReasoningSummary};

// Minimal — just API key and model
let config = OpenAIResponsesConfig::new("sk-...", "gpt-4.1-nano");

// With organization and project
let config = OpenAIResponsesConfig::new("sk-...", "gpt-4.1-mini")
    .with_organization("org-...")
    .with_project("proj-...");

// Custom base URL (for proxies or compatible APIs)
let config = OpenAIResponsesConfig::new("sk-...", "gpt-4.1-mini")
    .with_base_url("https://my-proxy.example.com/v1");
```

### Reasoning Models

For o-series models (o3, o4-mini), configure reasoning effort and summary:

```rust
use adk_model::openai::{
    OpenAIResponsesClient, OpenAIResponsesConfig,
    ReasoningEffort, ReasoningSummary,
};

let config = OpenAIResponsesConfig::new("sk-...", "o4-mini")
    .with_reasoning_effort(ReasoningEffort::Medium)
    .with_reasoning_summary(ReasoningSummary::Detailed);

let model = OpenAIResponsesClient::new(config)?;
```

| Reasoning Effort | Description |
|-----------------|-------------|
| `Low` | Minimal reasoning — fastest, cheapest |
| `Medium` | Balanced reasoning (default for most tasks) |
| `High` | Maximum reasoning — most thorough |

| Reasoning Summary | Description |
|------------------|-------------|
| `Auto` | Model decides whether to include a summary |
| `Concise` | Brief summary of reasoning |
| `Detailed` | Thorough summary of reasoning |

Reasoning summaries appear as `Part::Thinking` in the response stream, letting you show the model's thought process to users.

### Retry Configuration

```rust
use adk_model::retry::RetryConfig;

let client = OpenAIResponsesClient::new(config)?
    .with_retry_config(RetryConfig {
        max_retries: 3,
        ..Default::default()
    });
```

Retries are automatic for rate limits (429), server errors (500/502/503/504), and network failures.

---

## Available Models

| Model | Type | Description |
|-------|------|-------------|
| `gpt-4.1` | Chat | Latest GPT-4.1 with improved instruction following |
| `gpt-4.1-mini` | Chat | Balanced speed and capability |
| `gpt-4.1-nano` | Chat | Ultra-fast, cheapest option |
| `o3` | Reasoning | Full reasoning model |
| `o3-mini` | Reasoning | Efficient reasoning model |
| `o4-mini` | Reasoning | Latest efficient reasoning model |
| `gpt-5` | Chat | State-of-the-art unified model |
| `gpt-5-mini` | Chat | Efficient version of GPT-5 |

---

## Features

### Tool Calling

Function tools work the same way as with `OpenAIClient` — define tools on the agent and the runner handles the tool call loop:

```rust
use adk_rust::prelude::*;
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use std::sync::Arc;

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

let weather_tool = FunctionTool::new(
    "get_weather",
    "Get current weather for a city. Requires a 'city' string parameter.",
    get_weather,
);

let config = OpenAIResponsesConfig::new(&api_key, "gpt-4.1-nano");
let model = Arc::new(OpenAIResponsesClient::new(config)?);

let agent = LlmAgentBuilder::new("weather_agent")
    .instruction("Use the get_weather tool to answer weather questions.")
    .model(model)
    .tool(Arc::new(weather_tool))
    .build()?;
```

### Multi-Turn Conversations

The Runner automatically manages conversation history through sessions. Each turn's context is preserved:

```rust
// Turn 1
let msg1 = Content::new("user").with_text("My name is Alice.");
let mut stream = runner.run(uid.clone(), sid.clone(), msg1).await?;
// ... consume stream ...

// Turn 2 — the model remembers the previous turn
let msg2 = Content::new("user").with_text("What is my name?");
let mut stream = runner.run(uid.clone(), sid.clone(), msg2).await?;
// Response: "Your name is Alice."
```

### Per-Request Reasoning Override

Override reasoning settings per-request using LlmRequest extensions:

```rust
use adk_rust::prelude::*;

let agent = LlmAgentBuilder::new("flexible_reasoner")
    .model(model)
    .generate_content_config(GenerateContentConfig {
        extensions: {
            let mut ext = std::collections::HashMap::new();
            ext.insert("openai".to_string(), serde_json::json!({
                "reasoning": {
                    "effort": "high",
                    "summary": "detailed"
                }
            }));
            ext
        },
        ..Default::default()
    })
    .build()?;
```

### Built-In Tools

The Responses API supports OpenAI-hosted tools. Pass them via extensions:

```rust
let agent = LlmAgentBuilder::new("researcher")
    .model(model)
    .generate_content_config(GenerateContentConfig {
        extensions: {
            let mut ext = std::collections::HashMap::new();
            ext.insert("openai".to_string(), serde_json::json!({
                "built_in_tools": [
                    { "type": "web_search_preview" }
                ]
            }));
            ext
        },
        ..Default::default()
    })
    .build()?;
```

Available built-in tools: `web_search_preview`, `file_search`, `code_interpreter`.

### Previous Response ID

For server-side conversation state (bypassing local session history), pass `previous_response_id`:

```rust
let agent = LlmAgentBuilder::new("stateful")
    .model(model)
    .generate_content_config(GenerateContentConfig {
        extensions: {
            let mut ext = std::collections::HashMap::new();
            ext.insert("openai".to_string(), serde_json::json!({
                "previous_response_id": "resp_abc123"
            }));
            ext
        },
        ..Default::default()
    })
    .build()?;
```

---

## Streaming Behavior

The Responses API client streams text and reasoning deltas in real-time:

- Text deltas arrive as `Part::Text` with `partial: true`
- Reasoning summary deltas arrive as `Part::Thinking` with `partial: true`
- Function calls are emitted from the final `ResponseCompleted` event with correct names and arguments
- The final event has `turn_complete: true` with usage metadata and finish reason

This means you see text appearing token-by-token while the model generates, and function calls arrive as complete objects ready for execution.

---

## Provider Metadata

Every response includes provider metadata with the `response_id`:

```rust
if let Some(meta) = &response.provider_metadata {
    let response_id = meta["openai"]["response_id"].as_str();
    // Use for previous_response_id, logging, debugging
}
```

Additional metadata may include:
- `encrypted_content` — from reasoning models (for context preservation)
- `built_in_tool_outputs` — results from web search, file search, code interpreter

---

## Error Handling

Errors are mapped to structured `AdkError` with appropriate categories:

| HTTP Status | Error Category | Retryable |
|-------------|---------------|-----------|
| 401 | `Unauthorized` | No |
| 429 | `RateLimited` | Yes |
| 500, 502, 503, 504 | `Unavailable` | Yes |
| Other | `Internal` | No |

```rust
match runner.run(uid, sid, message).await {
    Ok(stream) => { /* process stream */ }
    Err(e) if e.is_retryable() => { /* retry logic */ }
    Err(e) if e.is_unauthorized() => { /* check API key */ }
    Err(e) => { /* handle other errors */ }
}
```

---

## Example

A complete 7-scenario example is available at `examples/openai_responses/`:

```bash
export OPENAI_API_KEY=sk-...
cargo run --manifest-path examples/openai_responses/Cargo.toml
```

Scenarios covered:
1. Basic non-streaming chat
2. Basic streaming chat
3. Reasoning model with summary (o4-mini)
4. Tool calling with function tools
5. Multi-turn conversation
6. System instructions
7. Temperature and generation config

---

## Related

- [Cloud Model Providers](./providers.md) — All supported LLM providers
- [Ollama (Local)](./ollama.md) — Run models locally
- [LlmAgent](../agents/llm-agent.md) — Using models with agents
- [Function Tools](../tools/function-tools.md) — Adding tools to agents

---

**Previous**: [← Cloud Providers](./providers.md) | **Next**: [Ollama (Local) →](./ollama.md)
