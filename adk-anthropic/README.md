# adk-anthropic

Dedicated Anthropic API client for [ADK-Rust](https://github.com/zavora-ai/adk-rust). Provides the HTTP client, type system, SSE streaming, error handling, backoff logic, and token pricing for the full Anthropic API surface.

## Legal Disclaimer

This project is an **unofficial** community-maintained library. It is not affiliated with, endorsed by, or sponsored by Anthropic, PBC. Use of the Anthropic API through this library is subject to [Anthropic's Terms of Service](https://www.anthropic.com).


## Features

- **Messages API** — non-streaming and SSE streaming with all content block types
- **Adaptive thinking** — `ThinkingConfig::adaptive()` for Opus 4.6 / Sonnet 4.6
- **Budget-based thinking** — `ThinkingConfig::enabled(budget)` for older models
- **Effort parameter** — `OutputConfig::with_effort()` controlling response thoroughness
- **Structured outputs** — JSON schema via `OutputConfig` / `OutputFormat`
- **Tool calling** — custom function tools, server tools (web search, bash, text editor, code execution, memory)
- **Prompt caching** — automatic (top-level `cache_control`) and explicit (block-level)
- **Context management** — `ContextManagement` with tool result clearing and thinking block clearing (beta)
- **Citations** — document-level citations with char, page, and content block locations
- **Vision** — URL and base64 image analysis
- **PDF processing** — URL, base64, and Files API PDF analysis with citations
- **Token counting** — `/v1/messages/count_tokens` endpoint
- **Fast mode** — `speed: "fast"` for Opus 4.6 (beta, waitlist)
- **Batches API** — async batch processing
- **Files API** — upload, get, delete, list
- **Models API** — list and get model metadata with capabilities
- **Token pricing** — per-model cost calculation from `Usage` data

## Supported Models

| Model | API ID | Generation |
|-------|--------|------------|
| Claude Opus 4.6 | `claude-opus-4-6` | Latest |
| Claude Sonnet 4.6 | `claude-sonnet-4-6` | Latest |
| Claude Haiku 4.5 | `claude-haiku-4-5` | Latest (fastest) |
| Claude Opus 4.5 | `claude-opus-4-5` | Previous |
| Claude Sonnet 4.5 | `claude-sonnet-4-5` | Previous |
| Claude Sonnet 4 | `claude-sonnet-4-0` | Legacy |
| Claude Opus 4 | `claude-opus-4-0` | Legacy |

Any model string not matching a known variant deserializes as `Model::Custom(String)`.

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

Run any example with `cargo run -p adk-anthropic --example <name>`:

| Example | Description |
|---------|-------------|
| `basic` | Non-streaming chat |
| `streaming` | SSE streaming with delta handling |
| `thinking` | Adaptive + budget-based extended thinking |
| `tools` | Tool calling round-trip |
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
| `custom_base_url` | Custom endpoints (Ollama, Vercel, MiniMax, proxies) |

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

let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-6")
    .with_tool_search(ToolSearchConfig::new("^safe_.*"));
```

## License

Apache-2.0
