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

| Model | API ID | `Model` constructor | Catalog role |
|-------|--------|---------------------|--------------|
| Claude Sonnet 5 | `claude-sonnet-5` | `Model::claude_sonnet_5()` | Default, balanced |
| Claude Fable 5.1 | `claude-fable-5-1` | `Model::claude_fable_5_1()` | Flagship |
| Claude Opus 5.5 | `claude-opus-5-5` | `Model::claude_opus_5_5()` | Flagship |
| Claude Sonnet 5.5 | `claude-sonnet-5-5` | `Model::claude_sonnet_5_5()` | Balanced |
| Claude Haiku 5.5 | `claude-haiku-5-5` | `Model::claude_haiku_5_5()` | Economy |
| Claude Opus 5 | `claude-opus-5` | `Model::claude_opus_5()` | Flagship |
| Claude Fable 5 | `claude-fable-5` | `Model::claude_fable_5()` | Flagship |
| Claude Haiku 4.5 | `claude-haiku-4-5` | `KnownModel::ClaudeHaiku45` | Economy |

`KnownModel` also names earlier models, such as `KnownModel::ClaudeOpus47` and
`KnownModel::ClaudeSonnet46`, and `Model::Custom` takes any other ID.

## Setup

Set your API key:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

## Direct Client Usage

```rust
use adk_anthropic::{Anthropic, MessageCreateParams, Model};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Anthropic::new(None)?; // reads ANTHROPIC_API_KEY
    let params = MessageCreateParams::simple("Hello!", Model::claude_sonnet_5());
    let response = client.send(params).await?;
    for block in &response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }
    Ok(())
}
```

## Through adk-model

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

fn build() -> Result<AnthropicClient, Box<dyn std::error::Error>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    Ok(AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5"))?)
}
```

`AnthropicClient` sends an agent's instructions (`system` contents) in the `system` parameter
of every request, and leaves the conversation's first user message a user message. See
[Model Providers](./providers.md#anthropic) for the request defaults and a complete agent.

## Custom base URL (gateways, proxies, compatible endpoints)

Point the client at a different endpoint to route through a corporate proxy or a
Messages-API-compatible gateway. Provide the root URL **without** the `/v1/`
suffix — it is appended automatically. `AnthropicConfig::with_base_url` flows
through `adk-model` to the underlying client:

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

fn build(api_key: String) -> Result<AnthropicClient, adk_core::AdkError> {
    AnthropicClient::new(
        AnthropicConfig::new(api_key, "claude-sonnet-5")
            .with_base_url("https://gateway.internal/anthropic"),
    )
}
```

Or set it directly on the low-level client, and read back the effective endpoint
with `base_url()`:

```rust
use adk_anthropic::Anthropic;

fn build(api_key: String) -> adk_anthropic::Result<Anthropic> {
    let client = Anthropic::new(Some(api_key))?
        .with_base_url("https://api.minimax.io/anthropic".to_string())?;
    assert_eq!(client.base_url(), "https://api.minimax.io/anthropic");
    Ok(client)
}
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

fn build(api_key: String) -> adk_anthropic::Result<Anthropic> {
    Anthropic::new_with_base_url(api_key, "https://gateway.internal/anthropic")
}
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

### Request adapter

`AnthropicClient::with_request_adapter` edits the JSON body and headers of each
Messages request before it is sent, for gateways that need per-request bearer tokens,
routing headers, or metadata:

```rust
use adk_anthropic::HeaderValue;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, RequestAdapter};
use std::sync::{Arc, RwLock};

fn build(token: Arc<RwLock<String>>) -> Result<AnthropicClient, adk_core::AdkError> {
    let adapter: RequestAdapter = Arc::new(move |body, headers| {
        body["metadata"] = serde_json::json!({"user_id": "session-1"});
        let bearer = format!("Bearer {}", token.read().expect("token lock").as_str());
        headers.remove("x-api-key");
        headers.insert(
            "authorization",
            HeaderValue::from_str(&bearer).expect("token is a valid header value"),
        );
        Ok(())
    });
    Ok(AnthropicClient::new(AnthropicConfig::new("unused", "claude-sonnet-5"))?
        .with_request_adapter(adapter))
}
```

| Behaviour | Rule |
|-----------|------|
| Input | The serialized `MessageCreateParams` and the headers the SDK would send, including feature-derived `anthropic-beta` values |
| Frequency | Once per HTTP attempt, retries included |
| Prompt cache | `cache_control` breakpoints are already placed; edits to `system`, `tools`, or cached blocks can miss the cache |
| I/O | None — the closure is synchronous; refresh tokens out of band and read them here |
| Rejected | Changing `stream`, an invalid body, or a top-level field `MessageCreateParams` does not model (`model.anthropic.request_adapter`, nothing sent) |
| Without an adapter | The client sends the same requests through the SDK's normal send and stream methods |

On the `adk-anthropic` client, `Anthropic::message_headers_for_request` returns the headers
`send` or `stream` would send for given parameters, feature-derived `anthropic-beta` values
included, as a starting map for `send_with_headers` and `stream_with_headers`.

## Retries and Streaming

| Behaviour | Rule |
|-----------|------|
| Retries | `AnthropicClient` sets the SDK's own retries to zero; `RetryConfig` on the `adk-model` client is the only retry policy |
| Redirects | The client returns a 3xx response as an error instead of following it |
| Stream end | A stream must end with `message_stop` and a stop reason; otherwise it fails |
| Final snapshot | The last streamed response carries the complete message with `provider_metadata.content_complete` set to `true`; replace the earlier text and thinking deltas with it |
| Server tools and citations | Web search calls, results and citations are kept with the assistant turn and replayed natively on the next request; if the turn's text was edited afterwards, the turn is converted from its ADK parts instead |
| Thinking replay | Signed `thinking` blocks, including the empty blocks returned under `display: "omitted"`, and `redacted_thinking` blocks are replayed unchanged; thinking without a signature is not sent |
| Blank text and empty turns | Text that is empty or whitespace is left out, since the API rejects it; a turn left with no blocks (a whitespace-only answer, or reasoning from another provider that cannot be verified) is left out and the turns around it merge, so it cannot fail every later request |
| Values newer than the client | A content block, delta, stream event, or stop reason the client does not know is kept as `Unknown` instead of failing the response; `adk-model` skips an unknown block with a `warn` log and replays it unchanged from the native copy of the turn, and an unknown stop reason maps to `FinishReason::Other` |
| Tool images | Images a tool returns (`FunctionResponseData::inline_data` and image `file_data`) go inside the `tool_result` block, after the JSON response |
| Generation config | `stop_sequences` is sent; `seed`, `frequency_penalty`, `presence_penalty`, and `top_logprobs` fail with `model.unsupported_config` |
| Claude 5 and Opus 4.7/4.8 sampling | For `claude-opus-5*`, `claude-sonnet-5*`, `claude-haiku-5*`, `claude-fable-5*`, `claude-mythos-5*`, `claude-mythos-preview`, `claude-opus-4-8`, and `claude-opus-4-7`, a request with `temperature`, `top_p`, or `top_k` fails with `model.anthropic.claude5_sampling_unsupported`, and `ThinkingMode::Enabled` with `model.anthropic.claude5_manual_thinking_unsupported`, before anything is sent |

## Key Features

### Adaptive Thinking

Claude 4.6 and later models think adaptively, with the depth set by an effort level; the
Claude 5 models and Opus 4.7 reject budget-based thinking (`budget_tokens`).

```rust
use adk_anthropic::{EffortLevel, MessageCreateParams, Model, OutputConfig, ThinkingConfig};

fn params() -> MessageCreateParams {
    let mut params = MessageCreateParams::simple("Solve this...", Model::claude_sonnet_5())
        .with_thinking(ThinkingConfig::adaptive());
    params.output_config = Some(OutputConfig::with_effort(EffortLevel::High));
    params
}
```

Opus 4.7 and later, Sonnet 5 and later, and Haiku 5.5 omit the thinking text by default and
return only its signature; Opus 4.6 and Sonnet 4.6 return a summary. Through `adk-model`,
`AnthropicConfig::with_thinking_display` sets `thinking.display` for the configured thinking
mode, so `Part::Thinking` carries readable text:

```rust
use adk_model::anthropic::{
    AnthropicClient, AnthropicConfig, Effort, ThinkingDisplay, ThinkingMode,
};

fn build() -> Result<AnthropicClient, Box<dyn std::error::Error>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    Ok(AnthropicClient::new(
        AnthropicConfig::new(api_key, "claude-opus-5-5")
            .with_thinking_mode(ThinkingMode::Adaptive)
            .with_thinking_display(ThinkingDisplay::Summarized)
            .with_effort(Effort::High),
    )?)
}
```

### Prompt Caching

```rust
use adk_anthropic::{CacheControlEphemeral, CacheTtl, MessageCreateParams, Model};

fn params() -> MessageCreateParams {
    let mut params = MessageCreateParams::simple("Question", Model::claude_sonnet_5())
        .with_system("Large system prompt...");
    // A 5-minute entry (the API default) is `CacheControlEphemeral::new()`.
    params.cache_control = Some(CacheControlEphemeral::new().with_ttl(CacheTtl::one_hour()));
    params
}
```

Through `adk-model`, `AnthropicClient` caches by default: it places one breakpoint on the
system prompt and one on the conversation tail. `with_prompt_cache_ttl` sets the lifetime of
both breakpoints, and `with_prompt_caching(false)` turns them off.

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, CacheTtl};

fn build(api_key: String) -> Result<AnthropicClient, adk_core::AdkError> {
    let config = AnthropicConfig::new(api_key, "claude-sonnet-5-5")
        .with_prompt_cache_ttl(CacheTtl::one_hour());
    AnthropicClient::new(config)
}
```

| TTL | Write price | Use when |
|-----|-------------|----------|
| `CacheTtl::five_minutes()` (default; also `CacheTtl::standard()`) | 1.25× base input | Requests sharing a prefix arrive within five minutes |
| `CacheTtl::one_hour()` (also `CacheTtl::long()`) | 2× base input | Requests sharing a prefix are more than five minutes apart, such as a conversation waiting on a person |

`pricing::estimate_cost` bills the 1-hour share of `usage.cache_creation_input_tokens`,
reported as `usage.cache_creation_input_tokens_1h`, at the 1-hour rate and the remainder at
the 5-minute rate.

### Structured Output

```rust
use adk_anthropic::{MessageCreateParams, Model, OutputConfig, OutputFormat};
use serde_json::json;

fn params() -> MessageCreateParams {
    let schema = json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
        "additionalProperties": false
    });
    let mut params = MessageCreateParams::simple("Extract data", Model::claude_sonnet_5());
    params.output_config = Some(OutputConfig::new(OutputFormat::json_schema(schema)));
    params
}
```

### Token Pricing

```rust
use adk_anthropic::Message;
use adk_anthropic::pricing::{ModelPricing, estimate_cost};

fn cost_usd(response: &Message) -> f64 {
    estimate_cost(ModelPricing::SONNET_5, &response.usage).total()
}
```

`ModelPricing::for_model_id` resolves a model ID to its rates, including the Claude 5.5 and
Fable 5.1 constants (`OPUS_55`, `SONNET_55`, `HAIKU_55`, `FABLE_51`). Through `adk-model`,
every response's `UsageMetadata::cost` is filled from `PricingCatalog`; see
[Usage Attribution and Cost](./providers.md#usage-attribution-and-cost).

## Examples

Run with `cargo run -p adk-anthropic --example <name>`:

| Example | Shows |
|---------|-------|
| `basic` | Non-streaming chat |
| `anthropic_streaming` | SSE streaming |
| `thinking` | Adaptive and budget thinking |
| `anthropic_tools` | Tool calling |
| `structured_output` | JSON schema output |
| `caching` | Multi-turn caching with costs |
| `context_editing` | Tool and thinking clearing (beta) |
| `compaction` | Server-side compaction |
| `token_counting` | Token counting before sending |
| `stop_reasons` | Every stop reason |
| `fast_mode` | Fast mode (beta) |
| `citations` | Document citations |
| `pdf_processing` | PDF analysis |
| `vision` | Image understanding |
| `anthropic_custom_base_url` | A custom base URL |
| `request_customization` | Per-request beta headers and bearer authentication |
| `server_fallback` | Server-side safety-refusal fallback |

The `managed_agents_hello`, `managed_agents_custom_tools`, `managed_agents_files`,
`managed_agents_memory`, and `managed_agents_multiagent` examples need
`--features managed-agents`.
