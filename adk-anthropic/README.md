# adk-anthropic

Dedicated Anthropic API client for [ADK-Rust](https://github.com/zavora-ai/adk-rust): the HTTP client, type system, SSE streaming, error handling, backoff logic, and token pricing behind `adk_model::anthropic::AnthropicClient`.

[![Crates.io](https://img.shields.io/crates/v/adk-anthropic.svg)](https://crates.io/crates/adk-anthropic)
[![Documentation](https://docs.rs/adk-anthropic/badge.svg)](https://docs.rs/adk-anthropic)
[![License](https://img.shields.io/crates/l/adk-anthropic.svg)](../LICENSE)

## Legal Disclaimer

This project is an **unofficial** community-maintained library. It is not affiliated with, endorsed by, or sponsored by Anthropic, PBC. Use of the Anthropic API through this library is subject to [Anthropic's Terms of Service](https://www.anthropic.com).


## Features

- **Messages API** — non-streaming and SSE streaming with all content block types
- **Adaptive thinking** — `ThinkingConfig::adaptive()` for Opus 4.8 / Opus 4.7 / Opus 4.6 / Sonnet 4.6
- **Budget-based thinking** — `ThinkingConfig::enabled(budget)` for older models (rejected on Opus 4.8 / Opus 4.7)
- **Effort parameter** — `OutputConfig::with_effort()` with `low`, `medium`, `high`, `xhigh`, `max` levels
- **Structured outputs** — JSON schema via `OutputConfig` / `OutputFormat`
- **Tool calling** — custom function tools, server tools (web search, web fetch, bash, text editor, code execution, memory); `WebSearchTool20260209` and `WebFetchTool20260209` declare the dynamic-filtering tool versions
- **Prompt caching** — automatic (top-level `cache_control`) and explicit (block-level)
- **Context management** — `ContextManagement` with tool result clearing and thinking block clearing (beta)
- **Citations** — document-level citations with char, page, and content block locations
- **Vision** — URL and base64 image analysis
- **PDF processing** — URL, base64, and Files API PDF analysis with citations
- **Token counting** — `/v1/messages/count_tokens` endpoint
- **Fast mode** — `speed: "fast"` (`SpeedMode::Fast`, research preview)
- **Request customization** — caller-selected beta headers, bearer auth, API-version overrides, and exact per-request headers
- **Server-side fallback** — typed default or explicit safety-refusal routing with fallback-aware responses and SSE events
- **Batches API** — create, get, results, cancel, delete, list
- **Files API** — upload, get, delete, list on `Anthropic`; `FilesClient` (`files` feature) adds `upload_file_from_path` and `download_file`, which returns files created by skills or the code execution tool
- **Skills API** — create, get, update, delete, list
- **Models API** — list and get model metadata with capabilities
- **Forward compatibility** — a content block, delta, stream event, or stop reason newer than the client parses as an `Unknown` variant holding the raw value instead of failing the response
- **Token pricing** — per-model cost calculation from `Usage` data
- **Managed Agents** — client for Claude Managed Agents (agents, environments, sessions, SSE streaming, custom tools, vaults, memory stores, dreams, webhooks, multiagent orchestration), behind the `managed-agents` feature

## Installation

```toml
[dependencies]
adk-anthropic = "3.0.0"
```

| Feature | Enables |
|---------|---------|
| `managed-agents` | The `managed_agents` module (`ManagedAgentsClient`, webhooks) |
| `files` | The `files` module (`FilesClient`) |

## Supported Models

| Model | API ID | Generation |
|-------|--------|------------|
| Claude Fable 5.1 | `claude-fable-5-1` | Most capable; demanding reasoning and long-horizon work |
| Claude Opus 5.5 | `claude-opus-5-5` | Current Opus; fast mode supported |
| Claude Sonnet 5.5 | `claude-sonnet-5-5` | Current Sonnet |
| Claude Haiku 5.5 | `claude-haiku-5-5` | Current Haiku; economy |
| Claude Opus 5 | `claude-opus-5` | Previous Opus; fast mode supported |
| Claude Sonnet 5 | `claude-sonnet-5` | ADK default |
| Claude Fable 5 | `claude-fable-5` | Creative and long-form |
| Claude Opus 4.8 | `claude-opus-4-8` | Previous flagship; fast mode supported |
| Claude Opus 4.7 | `claude-opus-4-7` | Previous generation |
| Claude Sonnet 4.6 | `claude-sonnet-4-6` | Previous generation |
| Claude Haiku 4.5 | `claude-haiku-4-5` | Economy |

`KnownModel` covers the Claude 4 family. The Claude 5 identifiers come from factories that
return `Model::Custom`: `Model::claude_fable_5_1()`, `claude_opus_5_5()`, `claude_sonnet_5_5()`,
`claude_haiku_5_5()`, `claude_opus_5()`, `claude_sonnet_5()`, and `claude_fable_5()`. Any model
string that matches no known variant deserializes as `Model::Custom(String)`.

### Current Model Request Contracts

Claude 5, Opus 4.8, and Opus 4.7 use stricter request contracts than older models:

- **Adaptive thinking only** — `thinking: {type: "enabled", budget_tokens: N}` is rejected. Use `ThinkingConfig::adaptive()`.
- **No custom sampling** — `temperature` and `top_p` are rejected.
- **`xhigh` effort** — between `high` and `max`, available on Opus 4.7 and later.

`adk_model::anthropic::AnthropicClient` checks the first two before sending a request to a Claude
5 model (`model.anthropic.claude5_sampling_unsupported`,
`model.anthropic.claude5_manual_thinking_unsupported`).

## Quick Start

```rust
use adk_anthropic::pricing::{ModelPricing, estimate_cost};
use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new(None)?; // reads ANTHROPIC_API_KEY
    let params = MessageCreateParams::simple("Name one planet in a short sentence.", KnownModel::ClaudeSonnet46);
    let response = client.send(params).await?;
    for block in &response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }

    let cost = estimate_cost(ModelPricing::SONNET_46, &response.usage);
    println!("cost: ${:.6}", cost.total());
    Ok(())
}
```

## Examples

### Request headers and server-side fallback

Client methods select betas for one request or replace every header:

```rust
use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new(None)?;
    let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46);

    // The headers `send` would use, including feature-derived `anthropic-beta` values.
    let headers = client.message_headers_for_request(&params, false)?;
    let response = client.send_with_headers(params, headers).await?;
    println!("{:?}", response.stop_reason);
    Ok(())
}
```

`send_with_betas` and `stream_with_betas` add beta names to one request. For OAuth or an
Anthropic-compatible gateway, `Anthropic::new_with_auth_token` sends
`Authorization: Bearer ...` and omits `x-api-key`. `default_headers_for_request` returns the
client's default headers.

Server fallback is limited to safety-classifier refusals. It does not retry
rate limits, overloaded responses, or server errors. It is currently intended
for classifier-enabled models such as Claude Fable 5 and Claude Opus 5:

```rust
use adk_anthropic::{Anthropic, MessageCreateParams, Model, ServerFallbackRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new(None)?;
    let params = MessageCreateParams::simple("Hello", Model::claude_fable_5());
    let response = client
        .send_with_server_fallbacks(ServerFallbackRequest::default_routing(params)?)
        .await?;
    println!("fallback ran: {}", response.served_by_fallback());
    Ok(())
}
```

### Base URLs and internal gateways

Every request attaches the API key, so a base URL must use `https://`, or `http://`
with a loopback host (`localhost`, `127.0.0.1`, `[::1]`). A trusted internal
gateway that is reachable only over plain HTTP needs an explicit opt-in, which
logs a warning naming the host when the URL is accepted:

| Base URL source | Opt-in |
|-----------------|--------|
| `with_base_url`, `with_base_url_and_timeout` | Call `allow_insecure_http()` **before** them — they validate the URL when called |
| `ANTHROPIC_BASE_URL` (read by `Anthropic::new`) | Set `ANTHROPIC_ALLOW_INSECURE_HTTP` to `1` or `true` |

```rust
use adk_anthropic::Anthropic;

fn main() -> Result<(), adk_anthropic::Error> {
    let client = Anthropic::new(None)?
        .allow_insecure_http()
        .with_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic".to_string())?;
    println!("{}", client.base_url());
    Ok(())
}
```

### Timeouts, stream errors, and request limits

| Behaviour | Rule |
|-----------|------|
| Non-streaming request | `with_timeout` bounds the whole request (default 10 minutes) |
| Connecting | `with_timeout`, capped at 60 seconds |
| Streaming request | `with_timeout` bounds the wait for response headers; the body has no total bound |
| Stalled stream | The stream yields `Error::Timeout` and ends after 30 seconds without data |
| Total stream bound | Opt in with `with_stream_timeout(Some(duration))` |
| Byte-inactivity mode | `with_http_timeouts(connect, idle)` replaces the total request bound with connection and HTTP-byte inactivity limits; SSE comments count as activity |
| Mid-stream `error` event | `AccumulatingStream` yields it as `Err` (`overloaded_error` → `Error::ServiceUnavailable`, `rate_limit_error` → `Error::RateLimit`) and sends the error, not the partial message, through its channel |
| Unknown SSE event type | Logged at `debug` and surfaced as `MessageStreamEvent::Unknown` with its JSON payload |
| Unknown content block, delta, or stop reason | Deserialized to `ContentBlock::Unknown`, `ContentBlockDelta::Unknown`, or `StopReason::Unknown` with the raw value; the message still parses |
| Request validation | Text is capped at 1 MB per message and 100 KB for the system prompt; base64 images at 5 MB and PDFs at 32 MB each; base64 media plus text at 32 MB per request |

```rust
use std::time::Duration;

use adk_anthropic::Anthropic;

fn main() -> Result<(), adk_anthropic::Error> {
    let client = Anthropic::new(None)?
        .with_timeout(Duration::from_secs(30))?
        .with_stream_timeout(Some(Duration::from_secs(900)));
    println!("{:?}", client.timeout());
    Ok(())
}
```

`Debug` output of `Anthropic`, `ManagedAgentsClient`, and `FilesClient` redacts
the API key, and the `x-api-key` header is marked sensitive.

`cargo run -p adk-anthropic --example transport_timeouts` configures a 10-second
connection limit and a 300-second inactivity limit without sending a request.
`Error::retry_after()` reads server delays, including HTTP-date headers. HTTP 529
remains `Error::RateLimit`; `status_code()` preserves its original value.

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

### Managed Agents Defaults and Webhooks

| Helper | Sends | Widen with |
|--------|-------|------------|
| `CreateEnvironmentParams::cloud(name)` | `limited` networking: no hosts, package registries, or MCP servers | `cloud_limited(name, hosts)`, `cloud_unrestricted(name)` |
| `ToolConfig::agent_toolset()` | The agent toolset with `web_fetch` and `web_search` disabled | `ToolConfig::agent_toolset_with_web()` |
| `ToolConfig::agent_toolset_with_policy(policy)` | The same, with a permission policy | Add web entries to `configs` |

The web tools run on Anthropic's servers, so an environment's networking policy does
not restrict them.

Webhook deliveries are signed with [Standard Webhooks](https://www.standardwebhooks.com/).
Pass the raw body and the `webhook-id`, `webhook-timestamp`, and `webhook-signature`
headers to `WebhookVerifier`:

```rust
use adk_anthropic::HeaderMap;
use adk_anthropic::managed_agents::{WebhookEvent, WebhookHeaders, WebhookVerifier};

fn handle(
    body: &str,
    headers: &HeaderMap,
    secret: &str,
) -> Result<WebhookEvent, Box<dyn std::error::Error>> {
    let verifier = WebhookVerifier::new(secret)?; // the whsec_ secret from the Console
    Ok(verifier.verify(body, &WebhookHeaders::from_header_map(headers)?)?)
}
```

Verification accepts any of the space-separated `v1,<base64>` signatures and rejects
timestamps more than five minutes from the local clock in either direction
(`with_tolerance` changes the window).

## Pricing module

`pricing::estimate_cost(pricing, &usage)` prices a response's `Usage`, as in the Quick Start.
`ModelPricing::for_model_id` resolves a model ID, including the Claude 5 and 5.5 identifiers, to
its rates. Cache writes are split by TTL: the `cache_creation_input_tokens_1h` share is billed at
the 1-hour rate and the rest at the 5-minute rate.

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

We stripped claudius's agent framework, CLI tools, chat session management, and observability modules (all handled by other ADK crates), then extended the retained code with adaptive thinking, effort parameter, structured outputs, context management, fast mode, citations, Files API, Skills API, Models API with capabilities, token pricing, and updated model definitions.

## Tool Search

`ToolSearchConfig` enables regex-based tool filtering at the provider level:

```rust
use adk_anthropic::ToolSearchConfig;

fn main() {
    let config = ToolSearchConfig::new("^(search|fetch)_.*");
    assert!(config.matches("search_web").unwrap());
    assert!(!config.matches("delete_all").unwrap());
}
```

When integrated with `AnthropicConfig` in `adk-model`, only tools matching the pattern are sent to the API:

```rust
use adk_anthropic::ToolSearchConfig;
use adk_model::anthropic::AnthropicConfig;

fn main() {
    let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-5")
        .with_tool_search(ToolSearchConfig::new("^safe_.*"));
    assert!(config.tool_search.is_some());
}
```

## License

Apache-2.0
