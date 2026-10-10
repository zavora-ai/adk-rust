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
| Claude Fable 5.1 | `claude-fable-5-1` | Most capable; demanding reasoning and long-horizon work, 1M context |
| Claude Opus 5.5 | `claude-opus-5-5` | Current Opus; long-running agentic coding, 1M context |
| Claude Sonnet 5.5 | `claude-sonnet-5-5` | Current Sonnet, 1M context |
| Claude Haiku 5.5 | `claude-haiku-5-5` | Current Haiku; high-volume, latency-sensitive work, 1M context |
| Claude Opus 5 | `claude-opus-5` | Previous Opus, 1M context |
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
cleartext, unless plain HTTP is acknowledged as described in
[Internal gateways over HTTP](#internal-gateways-over-http). The same rule applies
to `AnthropicConfig::with_base_url`, which is validated when `AnthropicClient::new`
builds the underlying client.

### Explicit key and endpoint

`Anthropic::new_with_base_url` builds a client from a key and an endpoint without
reading `ANTHROPIC_API_KEY` or `ANTHROPIC_BASE_URL`. A `file://` key is read from
that file, as with `Anthropic::new`. `AnthropicClient::new` uses this constructor
whenever `AnthropicConfig::with_base_url` is set:

```rust
use adk_anthropic::Anthropic;

let client = Anthropic::new_with_base_url(api_key, "https://gateway.internal/anthropic")?;
```

### Internal gateways over HTTP

An internal LLM gateway that is reachable only over plain HTTP needs an explicit
opt-in. The API key then crosses the network unencrypted, so the opt-in belongs
where the base URL is configured and applies to that client only.

| Base URL source | Opt-in | Order |
|-----------------|--------|-------|
| `Anthropic::with_base_url`, `Anthropic::with_base_url_and_timeout` | `Anthropic::allow_insecure_http()` | Before the base URL — both methods validate when called |
| `AnthropicConfig::with_base_url` | `AnthropicConfig::allow_insecure_http()` | Either — validation runs in `AnthropicClient::new` |
| `ANTHROPIC_BASE_URL` (read by `Anthropic::new`, and by `AnthropicClient` when no base URL is configured) | `ANTHROPIC_ALLOW_INSECURE_HTTP` set to `1` or `true` | — |

```rust
use adk_anthropic::Anthropic;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

fn build(api_key: String) -> Result<(Anthropic, AnthropicClient), Box<dyn std::error::Error>> {
    // adk-anthropic: the opt-in precedes the base URL.
    let client = Anthropic::new(Some(api_key.clone()))?
        .allow_insecure_http()
        .with_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic".to_string())?;

    // adk-model: the opt-in and the base URL can come in either order.
    let model = AnthropicClient::new(
        AnthropicConfig::new(api_key, "claude-sonnet-5")
            .allow_insecure_http()
            .with_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic"),
    )?;
    Ok((client, model))
}
```

For a base URL taken from the environment, the acknowledgement comes from the
environment as well:

```bash
export ANTHROPIC_BASE_URL=http://10.60.1.20:8080/api/v1/llm/anthropic
export ANTHROPIC_ALLOW_INSECURE_HTTP=1
```

| Behaviour | Rule |
|-----------|------|
| Loopback `http://` and `https://` | Accepted without the opt-in |
| Non-loopback `http://` with the opt-in | Accepted; a `warn`-level log names the host, never the URL path or the key |
| Non-loopback `http://` without the opt-in | Validation error naming the opt-in for the URL's source |
| Other schemes (`ftp://`, `ws://`) | Rejected, opt-in or not |
| `ANTHROPIC_ALLOW_INSECURE_HTTP` | Applies to `ANTHROPIC_BASE_URL` only; ignored for URLs given in code |
| `FilesClient`, `ManagedAgentsClient` | No opt-in; plain HTTP stays loopback-only |

## Retries and Streaming

| Behaviour | Rule |
|-----------|------|
| Retries | `AnthropicClient` sets the SDK's own retries to zero; `RetryConfig` on the `adk-model` client is the only retry policy |
| Redirects | The client returns a 3xx response as an error instead of following it |
| Stream end | A stream must end with `message_stop` and a stop reason; otherwise it fails |
| Final snapshot | The last streamed response carries the complete message with `provider_metadata.content_complete` set to `true`; replace the earlier text and thinking deltas with it |
| Server tools and citations | Web search calls, results and citations are kept with the assistant turn and replayed natively on the next request; if the turn's text was edited afterwards, the turn is converted from its ADK parts instead |

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
use adk_anthropic::{CacheControlEphemeral, CacheTtl, MessageCreateParams, Model};

let mut params = MessageCreateParams::simple("Question", Model::claude_sonnet_5())
    .with_system("Large system prompt...");
// 5-minute entry (the API default)
params.cache_control = Some(CacheControlEphemeral::new());
// 1-hour entry
params.cache_control = Some(CacheControlEphemeral::new().with_ttl(CacheTtl::one_hour()));
```

Through `adk-model`, `AnthropicClient` caches automatically: it places one breakpoint on the
system prompt and one on the conversation tail. `with_prompt_cache_ttl` sets the lifetime of
both breakpoints, and `with_prompt_caching(false)` turns them off.

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, CacheTtl};

let config = AnthropicConfig::new(api_key, "claude-sonnet-5-5")
    .with_prompt_cache_ttl(CacheTtl::one_hour());
let client = AnthropicClient::new(config)?;
```

| TTL | Write price | Use when |
|-----|-------------|----------|
| `CacheTtl::five_minutes()` (default) | 1.25× base input | Requests sharing a prefix arrive within five minutes |
| `CacheTtl::one_hour()` | 2× base input | Requests sharing a prefix are more than five minutes apart, such as a conversation waiting on a person |

`pricing::estimate_cost` bills the 1-hour share of `usage.cache_creation_input_tokens`,
reported as `usage.cache_creation_input_tokens_1h`, at the 1-hour rate and the remainder at
the 5-minute rate.

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
