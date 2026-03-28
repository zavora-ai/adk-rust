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
| Claude Opus 4.6 | `claude-opus-4-6` | Most intelligent, 1M context, 128K output |
| Claude Sonnet 4.6 | `claude-sonnet-4-6` | Best speed/intelligence balance, 1M context |
| Claude Haiku 4.5 | `claude-haiku-4-5` | Fastest, 200K context |
| Claude Opus 4.5 | `claude-opus-4-5` | Previous generation |
| Claude Sonnet 4.5 | `claude-sonnet-4-5` | Previous generation |
| Claude Sonnet 4 | `claude-sonnet-4-0` | Legacy |
| Claude Opus 4 | `claude-opus-4-0` | Legacy |

## Setup

Set your API key:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

## Direct Client Usage

```rust
use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

let client = Anthropic::new(None)?; // reads ANTHROPIC_API_KEY
let params = MessageCreateParams::simple("Hello!", KnownModel::ClaudeSonnet46);
let response = client.send(params).await?;
```

## Through adk-model

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

let api_key = std::env::var("ANTHROPIC_API_KEY")?;
let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-6"))?;
```

## Key Features

### Adaptive Thinking (4.6 models)

```rust
use adk_anthropic::{ThinkingConfig, OutputConfig, EffortLevel};

let mut params = MessageCreateParams::simple("Solve this...", KnownModel::ClaudeSonnet46)
    .with_thinking(ThinkingConfig::adaptive());
params.output_config = Some(OutputConfig::with_effort(EffortLevel::High));
```

### Prompt Caching

```rust
use adk_anthropic::CacheControlEphemeral;

let mut params = MessageCreateParams::simple("Question", KnownModel::ClaudeSonnet46)
    .with_system("Large system prompt...");
params.cache_control = Some(CacheControlEphemeral::new());
```

### Structured Output

```rust
use adk_anthropic::{OutputConfig, OutputFormat};

let mut params = MessageCreateParams::simple("Extract data", KnownModel::ClaudeSonnet46);
params.output_config = Some(OutputConfig::new(OutputFormat::json_schema(schema)));
```

### Token Pricing

```rust
use adk_anthropic::pricing::{ModelPricing, estimate_cost};

let cost = estimate_cost(ModelPricing::SONNET_46, &response.usage);
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
