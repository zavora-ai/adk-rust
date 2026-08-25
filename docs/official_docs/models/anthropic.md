# Anthropic (adk-anthropic)

The `adk-anthropic` crate is a dedicated Anthropic API client for ADK-Rust. It provides direct access to the full Anthropic Messages API surface, including streaming, extended thinking, prompt caching, citations, vision, PDF processing, and token pricing.

## Architecture

`adk-anthropic` is a standalone client crate that `adk-model` wraps via its Anthropic adapter. You can use it directly for low-level API access, or through `adk-model` for the unified `Llm` trait.

```
┌─────────────┐     ┌───────────────┐     ┌──────────────┐
│  Your Code  │────▶│   adk-model   │────▶│adk-anthropic │────▶ Anthropic API
│             │     │ (Llm trait)   │     │ (HTTP client)│
└─────────────┘     └───────────────┘     └──────────────┘
```

## Supported Models

| Model | API ID | Notes |
|-------|--------|-------|
| Claude Sonnet 5 | `claude-sonnet-5` | Default speed/intelligence balance, 1M context |
| Claude Opus 5 | `claude-opus-5` | Flagship capability, 1M context |
| Claude Fable 5 | `claude-fable-5` | Premium creative and long-form work, 1M context |
| Claude Haiku 4.5 | `claude-haiku-4-5` | Cost-efficient previous generation, 200K context |

## Setup

Set your API key:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

## Direct Client Usage

```rust
use adk_anthropic::{Anthropic, MessageCreateParams, Model};

let client = Anthropic::new(None)?; // reads ANTHROPIC_API_KEY
let params = MessageCreateParams::simple("Hello!", Model::claude_sonnet_5());
let response = client.send(params).await?;
```

## Through adk-model

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

let api_key = std::env::var("ANTHROPIC_API_KEY")?;
let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5"))?;
```

## Custom base URL (gateways, proxies, compatible endpoints)

Point the client at a different endpoint to route through a corporate proxy or a
Messages-API-compatible gateway. Provide the root URL **without** the `/v1/`
suffix — it is appended automatically. `AnthropicConfig::with_base_url` flows
through `adk-model` to the underlying client:

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

let model = AnthropicClient::new(
    AnthropicConfig::new(api_key, "claude-sonnet-5")
        .with_base_url("https://gateway.internal/anthropic"),
)?;
```

Or set it directly on the low-level client, and read back the effective endpoint
with `base_url()`:

```rust
use adk_anthropic::Anthropic;

let client = Anthropic::new(Some(api_key))?
    .with_base_url("https://api.minimax.io/anthropic".to_string())?;
assert_eq!(client.base_url(), "https://api.minimax.io/anthropic");
```

When unset, the client uses Anthropic's public API (`https://api.anthropic.com`).

`with_base_url` returns `Result` because the client attaches the Anthropic API key
to every request. Only `https://`, or `http://` with a loopback host
(`localhost`, `127.0.0.1`, `[::1]`) for local development, is accepted — anything
else is rejected as a validation error rather than silently sending the key in
cleartext. The same rule applies to `AnthropicConfig::with_base_url`, which is
validated when `AnthropicClient::new` builds the underlying client.

## Key Features

### Adaptive Thinking

Opus 4.7 **only** supports adaptive thinking — `budget_tokens` is rejected.

```rust
use adk_anthropic::{
    EffortLevel, KnownModel, MessageCreateParams, Model, OutputConfig, ThinkingConfig,
};

// Opus 4.7: use xhigh effort (recommended for coding/agentic)
let mut params = MessageCreateParams::simple("Solve this...", KnownModel::ClaudeOpus47)
    .with_thinking(ThinkingConfig::adaptive());
params.output_config = Some(OutputConfig::with_effort(EffortLevel::XHigh));

// Sonnet 5: balanced default for agentic workloads
let mut params = MessageCreateParams::simple("Solve this...", Model::claude_sonnet_5())
    .with_thinking(ThinkingConfig::adaptive());
params.output_config = Some(OutputConfig::with_effort(EffortLevel::High));
```

### Prompt Caching

```rust
use adk_anthropic::{CacheControlEphemeral, MessageCreateParams, Model};

let mut params = MessageCreateParams::simple("Question", Model::claude_sonnet_5())
    .with_system("Large system prompt...");
params.cache_control = Some(CacheControlEphemeral::new());
```

### Structured Output

```rust
use adk_anthropic::{MessageCreateParams, Model, OutputConfig, OutputFormat};

let mut params = MessageCreateParams::simple("Extract data", Model::claude_sonnet_5());
params.output_config = Some(OutputConfig::new(OutputFormat::json_schema(schema)));
```

### Token Pricing

```rust
use adk_anthropic::pricing::{ModelPricing, estimate_cost};

let cost = estimate_cost(ModelPricing::SONNET_5, &response.usage);
println!("${:.6}", cost.total());
```

## Examples

Run with `cargo run -p adk-anthropic --example <name>`:

- `basic` — non-streaming chat
- `streaming` — SSE streaming
- `thinking` — adaptive + budget thinking
- `tools` — tool calling
- `structured_output` — JSON schema
- `caching` — multi-turn caching with costs
- `context_editing` — tool/thinking clearing (beta)
- `compaction` — server-side compaction
- `token_counting` — pre-send token estimation
- `stop_reasons` — handling all stop reasons
- `fast_mode` — fast inference (beta)
- `citations` — document citations
- `pdf_processing` — PDF analysis
- `vision` — image understanding
