# adk-anthropic

Dedicated Anthropic API client for [ADK-Rust](https://github.com/zavora-ai/adk-rust). Provides the HTTP client, type system, SSE streaming, error handling, backoff logic, and token pricing for the full Anthropic API surface.

## Legal Disclaimer

This project is an **unofficial** community-maintained library. It is not affiliated with, endorsed by, or sponsored by Anthropic, PBC. Use of the Anthropic API through this library is subject to [Anthropic's Terms of Service](https://www.anthropic.com).


## Features

- **Messages API** — non-streaming and SSE streaming with all content block types
- **Adaptive thinking** — `ThinkingConfig::adaptive()` for Opus 4.8 / Opus 4.7 / Opus 4.6 / Sonnet 4.6
- **Budget-based thinking** — `ThinkingConfig::enabled(budget)` for older models (rejected on Opus 4.8 / Opus 4.7)
- **Effort parameter** — `OutputConfig::with_effort()` with `low`, `medium`, `high`, `xhigh`, `max` levels
- **Structured outputs** — JSON schema via `OutputConfig` / `OutputFormat`
- **Tool calling** — custom function tools, server tools (web search, bash, text editor, code execution, memory)
- **Prompt caching** — automatic (top-level `cache_control`) and explicit (block-level)
- **Context management** — `ContextManagement` with tool result clearing and thinking block clearing (beta)
- **Citations** — document-level citations with char, page, and content block locations
- **Vision** — URL and base64 image analysis
- **PDF processing** — URL, base64, and Files API PDF analysis with citations
- **Token counting** — `/v1/messages/count_tokens` endpoint
- **Fast mode** — `speed: "fast"` for Opus 4.6 (beta, waitlist)
- **Request customization** — caller-selected beta headers, bearer auth, API-version overrides, and exact per-request headers
- **Server-side fallback** — typed default or explicit safety-refusal routing with fallback-aware responses and SSE events
- **Batches API** — async batch processing
- **Files API** — upload, get, delete, list
- **Models API** — list and get model metadata with capabilities
- **Token pricing** — per-model cost calculation from `Usage` data
- **Managed Agents** — full client for Claude Managed Agents (agents, environments, sessions, SSE streaming, custom tools, vaults, memory stores, dreams, webhooks, multiagent orchestration) — feature-gated behind `managed-agents`

## Supported Models

| Model | API ID | Generation |
|-------|--------|------------|
| Claude Opus 5 | `claude-opus-5` | Current flagship |
| Claude Sonnet 5 | `claude-sonnet-5` | Recommended default |
| Claude Fable 5 | `claude-fable-5` | Creative and long-form |
| Claude Opus 4.8 | `claude-opus-4-8` | Previous flagship; fast mode supported |
| Claude Opus 4.7 | `claude-opus-4-7` | Previous generation |
| Claude Sonnet 4.6 | `claude-sonnet-4-6` | Previous generation |
| Claude Haiku 4.5 | `claude-haiku-4-5` | Economy |

Any model string not matching a known variant deserializes as `Model::Custom(String)`.

### Current Model Request Contracts

Claude 5, Opus 4.8, and Opus 4.7 use stricter request contracts than older models:

- **Adaptive thinking only** — `thinking: {type: "enabled", budget_tokens: N}` returns 400. Use `ThinkingConfig::adaptive()`.
- **No custom sampling** — `temperature` and `top_p` parameters are rejected.
- **New `xhigh` effort level** — sits between `high` and `max`. Recommended for coding and agentic workflows.
- **Updated tokenizer** — same text may produce 1.0–1.35× more tokens (especially code).

## Quick Start

```rust
use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new(None)?; // reads ANTHROPIC_API_KEY
    let params = MessageCreateParams::simple("Hello!", KnownModel::ClaudeSonnet46);
    let response = client.send(params).await?;
    for block in &response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }
    Ok(())
}
```

## Examples

### Request headers and server-side fallback

`MessageCreateParams` remains source-compatible with 2.0. Use client methods
for request-scoped beta selection or full header replacement:

```rust
use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Anthropic::new(None)?;
let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46);
let response = client
    .send_with_betas(params, &["fine-grained-tool-streaming-2025-05-14"])
    .await?;
# let _ = response;
# Ok(())
# }
```

For OAuth or an Anthropic-compatible gateway, use
`Anthropic::new_with_auth_token`; it sends `Authorization: Bearer ...` and
omits `x-api-key`. `default_headers_for_request` plus `send_with_headers` or
`stream_with_headers` provides exact replacement semantics when every header
must be controlled by the caller.

Server fallback is limited to safety-classifier refusals. It does not retry
rate limits, overloaded responses, or server errors. It is currently intended
for classifier-enabled models such as Claude Fable 5 and Claude Opus 5:

```rust
use adk_anthropic::{Anthropic, MessageCreateParams, Model, ServerFallbackRequest};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Anthropic::new(None)?;
let params = MessageCreateParams::simple(
    "Hello",
    Model::Custom("claude-fable-5".to_string()),
);
let response = client
    .send_with_server_fallbacks(ServerFallbackRequest::default_routing(params)?)
    .await?;
println!("fallback ran: {}", response.served_by_fallback());
# Ok(())
# }
```

Run any example with `cargo run -p adk-anthropic --example <name>`:

| Example | Description |
|---------|-------------|
| `basic` | Non-streaming chat |
| `anthropic_streaming` | SSE streaming with delta handling |
| `thinking` | Adaptive + budget-based extended thinking |
| `anthropic_tools` | Tool calling round-trip |
| `structured_output` | JSON schema structured outputs |
| `caching` | Multi-turn prompt caching with cost breakdown |
| `context_editing` | Tool result and thinking block clearing (beta) |
| `compaction` | Server-side compaction events |
| `token_counting` | Token count estimation before sending |
| `stop_reasons` | Handling all stop reason values |
| `fast_mode` | Fast inference mode for Opus 4.6 (beta) |
| `citations` | Document citations (plain text, custom content, multi-doc) |
| `pdf_processing` | PDF analysis via URL, base64, and with citations |
| `vision` | Image understanding via URL |
| `anthropic_custom_base_url` | Custom endpoints (Ollama, Vercel, MiniMax, proxies) |
| `request_customization` | Caller-selected beta headers and optional bearer authentication |
| `server_fallback` | Typed server-side safety-refusal fallback |

### Managed Agents Examples

Run with `cargo run -p adk-anthropic --example <name> --features managed-agents`:

| Example | Description |
|---------|-------------|
| `managed_agents_hello` | Minimal session: create agent → send message → stream response |
| `managed_agents_custom_tools` | Custom tool flow: agent calls your tools, you return results |
| `managed_agents_files` | Upload CSV, mount in session, agent analyzes data |
| `managed_agents_memory` | Persistent memory across sessions (agent remembers preferences) |
| `managed_agents_multiagent` | Coordinator delegates to researcher + writer agents in parallel |

**Hello World output:**
```
=== Managed Agents: Hello World ===

✓ Client created
✓ Agent created: agent_01SnYF9JNJR4c1FB1L8YPovY
✓ Environment created: env_01Gdwp4bFCZgGbbnQJLCCVmo
✓ Session created: sesn_01Da6YoDqrgLzkCRVg1DAGjK (status: Idle)
✓ Stream opened

→ Sent: "Hello! What is 2 + 2?"

← Agent response:
4! 😊 Is there anything else I can help you with?

✓ Session idle (stop_reason: end_turn)
```

**Custom Tools output:**
```
→ Sent: "What's the weather like in San Francisco and Tokyo?"

← Processing events:
I'll check the weather in both cities at the same time!
  🔧 Custom tool call: get_weather({"city":"San Francisco"})
  📤 Sending result: 🌤️ Weather in San Francisco: 22°C, partly cloudy
  🔧 Custom tool call: get_weather({"city":"Tokyo"})
  📤 Sending result: 🌤️ Weather in Tokyo: 22°C, partly cloudy
```

**Multiagent output:**
```
✓ Researcher agent created
✓ Writer agent created
✓ Coordinator agent created

→ Sent coordination task

← Processing events:
  ▶ Session running...
  I'll dispatch the Researcher to gather key facts first...
  [Researcher produces structured research]
  [Writer crafts final article from research]

── Session Threads ──
  🧵 Content Coordinator — idle (root)
  🧵 Researcher — idle
  🧵 Writer — idle
```

## Pricing Module

```rust
use adk_anthropic::pricing::{ModelPricing, estimate_cost};

let cost = estimate_cost(ModelPricing::SONNET_46, &response.usage);
println!("Cost: ${:.6}", cost.total());
```

## Trademarks

Anthropic, Claude, and the Anthropic logo are trademarks of Anthropic, PBC. All other trademarks are the property of their respective owners.


## Acknowledgments

This crate was forked from [claudius](https://github.com/crisogray/claudius) v0.19 by [@crisogray](https://github.com/crisogray), a comprehensive Rust SDK for the Anthropic API licensed under Apache-2.0.

The following components originate from claudius and form the foundation of `adk-anthropic`:

- **HTTP client** (`client.rs`) — the `Anthropic` struct, request execution, retry logic, custom base URL support
- **SSE streaming** (`sse.rs`) — Server-Sent Events parser for streaming responses
- **Accumulating stream** (`accumulating_stream.rs`) — stream accumulator for assembling complete messages from SSE deltas
- **Backoff** (`backoff.rs`) — exponential backoff with jitter for retryable errors
- **Error types** (`error.rs`) — comprehensive error enum with typed variants for all API error classes
- **Core type system** (`types/`) — `Message`, `MessageCreateParams`, `MessageParam`, `ContentBlock`, `TextBlock`, `ToolUseBlock`, `ToolResultBlock`, `ImageBlock`, `DocumentBlock`, `ThinkingBlock`, `SystemPrompt`, `Usage`, `StopReason`, `ToolChoice`, `ToolParam`, `CacheControlEphemeral`, and all serde serialization/deserialization logic
- **Client logger** (`client_logger.rs`) — `ClientLogger` trait for capturing API interactions
- **Cache control** (`cache_control.rs`) — cache breakpoint management utilities
- **JSON schema** (`json_schema.rs`) — schema utilities

We stripped claudius's agent framework, CLI tools, chat session management, and observability modules (all handled by other ADK crates), then extended the retained code with full March 2026 API parity: adaptive thinking, effort parameter, structured outputs, context management, fast mode, citations, Files API, Skills API, Models API with capabilities, token pricing, and updated model definitions.

## Tool Search

`ToolSearchConfig` enables regex-based tool filtering at the provider level:

```rust
use adk_anthropic::ToolSearchConfig;

let config = ToolSearchConfig::new("^(search|fetch)_.*");
assert!(config.matches("search_web").unwrap());
assert!(!config.matches("delete_all").unwrap());
```

When integrated with `AnthropicConfig` in `adk-model`, only tools matching the pattern are sent to the API:

```rust
use adk_model::anthropic::AnthropicConfig;
use adk_anthropic::ToolSearchConfig;

let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-5")
    .with_tool_search(ToolSearchConfig::new("^safe_.*"));
```

## License

Apache-2.0
