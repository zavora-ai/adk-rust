# adk-model

LLM provider clients for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)).
Every client implements the `adk_core::Llm` trait, so agents and runners accept any of them.

[![Crates.io](https://img.shields.io/crates/v/adk-model.svg)](https://crates.io/crates/adk-model)
[![Documentation](https://docs.rs/adk-model/badge.svg)](https://docs.rs/adk-model)
[![License](https://img.shields.io/crates/l/adk-model.svg)](../LICENSE)

## Providers

| Provider | Client | Feature | Catalog default model |
|----------|--------|---------|-----------------------|
| Google Gemini (AI Studio, Vertex AI) | `GeminiModel` | `gemini` (default) | `gemini-3.7-flash` |
| OpenAI Chat Completions | `openai::OpenAIClient` | `openai` | `gpt-5.6-terra` |
| OpenAI Responses API | `openai::OpenAIResponsesClient` | `openai` | `gpt-5.6-terra` |
| Azure OpenAI | `openai::AzureOpenAIClient` | `openai` | deployment-specific |
| OpenAI-compatible endpoints | `OpenAICompatible` | `openai` | per preset, see [OpenAI-compatible providers](#openai-compatible-providers) |
| Anthropic Claude | `anthropic::AnthropicClient` | `anthropic` | `claude-sonnet-5` |
| DeepSeek | `deepseek::DeepSeekClient` | `deepseek` | `deepseek-flash` |
| Groq | `groq::GroqClient` | `groq` | `openai/gpt-oss-120b` |
| Ollama (local) | `ollama::OllamaModel` | `ollama` | `qwen3.5` |
| OpenRouter | `openrouter::OpenRouterClient` | `openrouter` | `qwen/qwen3.7-max` |
| OpenCode Go and Zen | `opencode::OpenCodeClient` | `opencode` | none |
| Amazon Bedrock | `bedrock::BedrockClient` | `bedrock` | account- and region-specific |
| Azure AI Inference | `azure_ai::AzureAIClient` | `azure-ai` | endpoint-specific |
| Test doubles | `ScriptedLlm`, `MockLlm` | always available | none |

The catalog default is what `adk_model::catalog::recommended_model(provider)` returns. The
catalog also records lifecycle: `catalog::validate_model_selection` rejects model IDs known to
be retired, such as `gemini-2.0-flash` or `deepseek-chat`, and accepts unknown IDs so new,
private, and fine-tuned models stay usable.

## Installation

```toml
[dependencies]
adk-model = "3.0.0"
```

The default build includes only Gemini. Enable other providers with features:

```toml
[dependencies]
adk-model = { version = "3.0.0", features = ["openai", "anthropic"] }
```

Or use the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["models"] }
```

## Quick Start

The program sends one request to Gemini and prints the answer.

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::GeminiModel;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(api_key, "gemini-3.7-flash")?;

    let request = LlmRequest::new(
        "gemini-3.7-flash",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

Pass `true` as the second argument of `generate_content` to stream partial responses. To use a
model in an agent, hand it to `adk_agent::LlmAgentBuilder`:

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;
    let agent = LlmAgentBuilder::new("assistant")
        .instruction("Answer in one sentence.")
        .model(Arc::new(model))
        .build()?;
    println!("built agent {}", adk_core::Agent::name(&agent));
    Ok(())
}
```

## Provider setup

Each section below builds a client and sends the same request as the Quick Start.

### Gemini

`GeminiModel::new` uses the Gemini API (AI Studio). `GeminiModel::from_env(model)` reads the
environment instead: when `GOOGLE_GENAI_USE_ENTERPRISE` (or, if that is unset, the deprecated
`GOOGLE_GENAI_USE_VERTEXAI`) is `1` or a case-insensitive `true`, it builds a Vertex AI client
from Application Default Credentials with `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`,
which needs the `gemini-vertex` feature; otherwise it uses `GOOGLE_API_KEY` or `GEMINI_API_KEY`.

| Feature | Adds |
|---------|------|
| `gemini-vertex` | Vertex AI constructors: `new_google_cloud`, `new_google_cloud_adc`, `new_google_cloud_service_account`, `new_google_cloud_wif`, `new_google_cloud_endpoint` |
| `gemini-interactions` | `GeminiModel::use_interactions_api(true)`, which drives the same `Llm` calls through the Interactions API (Beta); see [Gemini Interactions](../docs/official_docs/models/gemini-interactions.md) |

A Gemini error object inside a successful response fails the call, a stream that ends without a
`finishReason` fails with `model.gemini.unfinished_stream`, and a blocked prompt ends the turn
with `FinishReason::Safety`, instead of reading as an empty answer.

### OpenAI

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5.6-terra"))?;

    let request = LlmRequest::new(
        "gpt-5.6-terra",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

Chat Completions rejects function tools on GPT-5.6 and later models while reasoning is on, so
`OpenAIClient` sends requests that declare tools for those models through the Responses API,
with `store: false`. Other requests stay on Chat Completions.

`AzureOpenAIClient::new(AzureConfig::new(api_key, api_base, api_version, deployment_id))`
targets an Azure OpenAI deployment through the same Chat Completions path.

### OpenAI Responses API

`OpenAIResponsesClient` calls `/v1/responses`, which carries reasoning summaries, built-in tools,
background mode, and conversation state.

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::openai::{
    OpenAIReasoningEffort, OpenAIResponsesClient, OpenAIResponsesConfig, ReasoningSummary,
};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let config = OpenAIResponsesConfig::new(api_key, "gpt-5.6-terra")
        .with_reasoning_summary(ReasoningSummary::Auto);
    let model = OpenAIResponsesClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Low)?;

    let request = LlmRequest::new(
        "gpt-5.6-terra",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

`OpenAIReasoningEffort` has the values `None`, `Minimal`, `Low`, `Medium`, `High`, `XHigh`, and
`Max`. GPT-5.6 accepts every value except `Minimal` through the Responses API; Chat Completions
accepts up to `XHigh`. `OpenAIResponsesConfig::with_reasoning_effort` takes the original
three-value `ReasoningEffort` (`Low`, `Medium`, `High`).

`OpenAIResponsesClient` always uses HTTP. For WebSocket mode, use `openai::WsTransport` from the
`openai-ws` feature. `ResponsesTransport` and `OpenAIResponsesConfig::with_transport` were removed
in 3.0: the option was never read.

#### Background mode

`extensions["openai"]["background"] = true` submits the response in the background.
`poll_response` and `cancel_response` take the response ID from
`provider_metadata["openai"]["response_id"]`.

```rust
use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let client = OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key, "gpt-5.6-luna"))?;

    let mut config = GenerateContentConfig::default();
    config.extensions.insert("openai".into(), serde_json::json!({ "background": true }));
    let request = LlmRequest::new(
        "gpt-5.6-luna",
        vec![Content::new("user").with_text("Summarize the history of Rust in one paragraph.")],
    )
    .with_config(config);

    let submitted = client.generate_content(request, false).await?.next().await.ok_or("no response")??;
    let response_id = submitted
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata["openai"]["response_id"].as_str())
        .ok_or("no response_id")?
        .to_string();

    let polled = client.poll_response(&response_id).await?;
    let status = polled.provider_metadata.as_ref().and_then(|metadata| metadata["openai"]["status"].as_str());
    println!("{response_id}: {status:?}");

    if status != Some("completed") {
        client.cancel_response(&response_id).await?;
    }
    Ok(())
}
```

#### Responses API examples

| Example | Shows |
|---------|-------|
| [`examples/openai_responses/`](../examples/openai_responses/) | `OpenAIResponsesClient` through `Runner` and `LlmAgent`, with multi-turn sessions |
| [`examples/openai_ws_minimal/`](../examples/openai_ws_minimal/) | WebSocket transport (`openai-ws`) |
| [`examples/gemini_ws_outcomes/`](../examples/gemini_ws_outcomes/) | Gemini and Responses WebSocket stream outcomes: abandoned, incomplete, closed, and failed responses, handshake retry |
| [`examples/openai_background/`](../examples/openai_background/) | Background mode, submit and poll |
| [`examples/openai_conversations/`](../examples/openai_conversations/) | Conversations API lifecycle (`openai-conversations`) |
| [`examples/openai_builtin_tools/`](../examples/openai_builtin_tools/) | Built-in tools such as image generation and web search |
| [`examples/openai_deep_research/`](../examples/openai_deep_research/) | Deep research models |
| [`examples/openai_open_responses/`](../examples/openai_open_responses/) | Open Responses mode for other Responses-compatible servers |

See the [Responses API guide](../docs/official_docs/models/openai-responses.md).

### Anthropic

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5"))?;

    let request = LlmRequest::new(
        "claude-sonnet-5",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

`AnthropicConfig` options:

| Option | Effect |
|--------|--------|
| `with_max_tokens(n)` | Output-token cap. Without it the client requests 32,000 tokens from Claude 4 and later models and 4,096 from older or unrecognized ones (`catalog::anthropic_default_max_tokens`) |
| `with_prompt_caching(bool)` | Automatic cache breakpoints on the system prompt and the conversation tail; on by default |
| `with_prompt_cache_ttl(CacheTtl::one_hour())` | Lifetime of both breakpoints, for prefixes reused more than five minutes apart |
| `with_thinking_mode(ThinkingMode::Adaptive)` | Adaptive thinking, the thinking mode of Claude 4.6 and later models |
| `with_thinking_display(ThinkingDisplay::Summarized)` | Returns summarized thinking text; current models omit it by default |
| `with_effort(Effort)`, `with_fast_mode(bool)`, `with_citations(bool)` | Effort level, fast mode, and citations |
| `with_request_timeout(Duration)` | Bound on a request, 10 minutes by default |
| `with_base_url(url)` | Another Messages-compatible endpoint, such as DeepSeek's `https://api.deepseek.com/anthropic` |

`AnthropicClient` also has `count_tokens(&request)`, `list_models()`, `get_model(id)`, and
`latest_rate_limit_info()`. An unknown content block or stop reason from a newer API version no
longer fails the turn: the block is skipped with a `warn` log and replayed unchanged on the next
request, and an unknown stop reason maps to `FinishReason::Other`.

### DeepSeek

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::deepseek::DeepSeekClient;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    // `deepseek-flash`; `DeepSeekClient::v4_pro` selects `deepseek-v4-pro` with thinking on.
    let model = DeepSeekClient::v4_flash(api_key)?;

    let request = LlmRequest::new(
        "deepseek-flash",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

Thinking output arrives as `Part::Thinking` from DeepSeek's `reasoning_content`.
`DeepSeekConfig` sets the thinking mode, `ReasoningEffort::High` or `Max`, and strict tool mode
(`with_strict_tools`, beta). `DeepSeekClient::chat` and `reasoner` now select `deepseek-flash`
and `deepseek-v4-pro`: DeepSeek retired `deepseek-chat` and `deepseek-reasoner` on 2026-07-24.

### Groq

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::groq::{GroqClient, GroqConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GROQ_API_KEY")?;
    let model = GroqClient::new(GroqConfig::gpt_oss_120b(api_key))?;

    let request = LlmRequest::new(
        "openai/gpt-oss-120b",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

`GroqConfig::new(api_key, model)` selects any other Groq model, such as `openai/gpt-oss-20b`.

### Ollama

```rust
use adk_model::ollama::{OllamaConfig, OllamaModel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Requires `ollama serve` and `ollama pull qwen3.5`.
    let model = OllamaModel::new(OllamaConfig::new("qwen3.5"))?;
    let _ = model;
    Ok(())
}
```

`OllamaConfig::new` targets `http://localhost:11434`; `OllamaConfig::with_host(host, model)`
targets another server. Streaming works with tool calls. Ollama runs locally, so its responses
cost zero.

### OpenRouter

`OpenRouterClient` implements `Llm` for agents and also exposes OpenRouter's native APIs.

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::openrouter::{OpenRouterClient, OpenRouterConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENROUTER_API_KEY")?;
    let client = OpenRouterClient::new(
        OpenRouterConfig::new(api_key, "qwen/qwen3.7-max")
            .with_http_referer("https://github.com/zavora-ai/adk-rust")
            .with_title("ADK-Rust"),
    )?;

    let request = LlmRequest::new(
        "qwen/qwen3.7-max",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = client.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    print!("{text}");
                }
            }
        }
    }
    println!();

    let models = client.list_models().await?;
    let credits = client.get_credits().await?;
    println!("models: {}", models.len());
    println!("credits remaining: {}", credits.total_credits - credits.total_usage);
    Ok(())
}
```

Use the native `OpenRouterClient` methods for model discovery and credits, exact chat or
responses payloads, routing, fallback, provider preferences, plugins, and built-in tools such as
web search. The `Llm` adapter supports streaming, tool calling, reasoning parts, and multimodal
requests, takes OpenRouter-specific tuning through `OpenRouterRequestOptions`, and maps responses
into the generic `LlmResponse` shape. The [`examples/openrouter/`](../examples/openrouter/) crate
and the `adk-model` examples `openrouter_chat`, `openrouter_responses`, `openrouter_adapter`, and
`openrouter_discovery` call the live API.

### OpenCode

The `opencode` feature adds `OpenCodeClient`, which picks Chat Completions, Responses, Anthropic
Messages, or Gemini `generateContent` from the model ID and sends the application's
`User-Agent` and `x-opencode-session` headers with every request.

```rust
use adk_model::opencode::{OpenCodeClient, OpenCodeConfig, OpenCodeService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENCODE_API_KEY")?;
    let model = OpenCodeClient::new(
        OpenCodeConfig::new(OpenCodeService::Go, api_key, "deepseek-v4.1-flash")
            .with_user_agent("my-coding-agent/1.0")
            .with_session_id("conversation-42"),
    )?;
    println!("{:?}", model.api());
    Ok(())
}
```

See the [OpenCode guide](../docs/official_docs/models/opencode.md) for the routing table.

### OpenAI-compatible providers

`OpenAICompatible` serves any OpenAI-compatible Chat Completions endpoint. Presets on
`OpenAICompatibleConfig` set the base URL and provider name; all of them need only the `openai`
feature.

| Provider | Preset | Catalog default model |
|----------|--------|-----------------------|
| xAI | `OpenAICompatibleConfig::xai` | `grok-4.6` |
| Fireworks AI | `OpenAICompatibleConfig::fireworks` | `accounts/fireworks/models/kimi-k2p6` |
| Together AI | `OpenAICompatibleConfig::together` | `MiniMaxAI/MiniMax-M2.7` |
| Mistral AI | `OpenAICompatibleConfig::mistral` | `mistral-medium-latest` |
| Perplexity | `OpenAICompatibleConfig::perplexity` | `sonar-pro` |
| Cerebras | `OpenAICompatibleConfig::cerebras` | `gpt-oss-120b` |
| SambaNova | `OpenAICompatibleConfig::sambanova` | `gpt-oss-120b` |
| OrcaRouter | `OpenAICompatibleConfig::orcarouter` | none; model IDs carry a vendor prefix such as `openai/gpt-5.6-terra` |
| MiniMax | `OpenAICompatibleConfig::minimax` | `MiniMax-M2.7` |
| Zhipu AI | `OpenAICompatibleConfig::zhipu` | `glm-5.2` |
| Baidu ERNIE (Qianfan) | `OpenAICompatibleConfig::baidu` | `ernie-5.1` |
| Cohere | `OpenAICompatibleConfig::cohere` | `command-a-plus-05-2026` |
| ByteDance Doubao (Volcano Engine Ark) | `OpenAICompatibleConfig::bytedance` | none; deployment-specific |
| Gemini (OpenAI-compatible endpoint) | `OpenAICompatibleConfig::gemini` | `gemini-3.7-flash` |

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("XAI_API_KEY")?;
    let model = OpenAICompatible::new(OpenAICompatibleConfig::xai(api_key, "grok-4.6"))?;

    let request = LlmRequest::new(
        "grok-4.6",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
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

`OpenAICompatibleConfig::new(api_key, model).with_base_url(url).with_provider_name(name)` targets
any other endpoint, such as vLLM. Reasoning history is not replayed by default:
`with_reasoning_replay(true)` replays `Part::Thinking` through `reasoning_content`, and
`with_reasoning_replay_field(ReasoningReplayField::Reasoning)` uses the `reasoning` field that
newer vLLM deployments read. Thinking stays separate from the visible message text either way.

### Amazon Bedrock

```rust
use adk_model::bedrock::{BedrockClient, BedrockConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Credentials come from the AWS credential chain. Model IDs are account- and region-specific.
    let model_id = std::env::var("BEDROCK_MODEL")?;
    let model = BedrockClient::new(BedrockConfig::new("us-east-1", model_id)).await?;
    let _ = model;
    Ok(())
}
```

`BedrockClient::new_with_api_key` uses a Bedrock bearer key instead of the credential chain, and
`BedrockClient::from_client` wraps an `aws_sdk_bedrockruntime::Client` the application built.

### Azure AI Inference

```rust
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("AZURE_AI_API_KEY")?;
    let model = AzureAIClient::new(AzureAIConfig::new(
        "https://my-endpoint.eastus.inference.ai.azure.com",
        api_key,
        "meta-llama-3.1-8b-instruct",
    ))?;
    let _ = model;
    Ok(())
}
```

## Usage attribution and cost

Every provider sets `LlmResponse::provider` and `LlmResponse::model`, and fills
`UsageMetadata::cost` (USD) from `pricing::PricingCatalog` when the provider reports no cost.
Cached prompt tokens are billed once, Anthropic cache writes by TTL, and long-context tiers apply
where the vendor publishes them. The catalog carries Gemini, OpenAI, Anthropic, DeepSeek, Groq,
xAI, and Mistral list prices, verified on `pricing::PRICING_EFFECTIVE_DATE`; local Ollama models
cost zero, and unpriced models keep `cost: None`. Built-in tool uses the provider reports (web
search, file search, Google Search grounding) land in `UsageMetadata::builtin_tool_uses`, and their
published per-use fees are added to `cost`.

A token count a provider reports as `null` or leaves out reads as zero (`None` for the
breakdowns), and a missing total is the sum of the reported counts. `UsageMetadata::provider_usage`
holds the provider's usage object as received.

```rust
use adk_core::UsageMetadata;
use adk_model::PricingCatalog;

fn main() {
    let usage = UsageMetadata { prompt_token_count: 1_000_000, ..Default::default() };
    let cost = PricingCatalog::standard().cost_usd(Some("openai"), "gpt-4.1-2025-04-14", &usage);
    assert_eq!(cost, Some(2.0));
}
```

`Llm::max_cost_micro_usd` gives an upper bound on a request's cost from the same catalog; a run
with a spend cap refuses a model call that has no bound.

## Behaviour shared by all clients

| Area | Behaviour |
|------|-----------|
| Streaming | A stream that ends before its terminal event, ends mid tool call, or carries an error frame fails with an `AdkError` instead of returning a truncated answer. See "Stream Termination" in [providers.md](../docs/official_docs/models/providers.md#stream-termination) |
| Tool calls | Text-encoded tool calls (Qwen/Hermes, Llama, Mistral Nemo, DeepSeek, DeepSeek-V3 native tokens, Gemma 4, action tags) are recognized only for tools the request declares. Arguments that are not a JSON object are marked with `adk_core::invalid_tool_arguments` instead of becoming `{}`, so the agent answers the call with an error and the model can retry. Chat Completions clients send one `tool` message per function response, and images a tool returns reach every provider |
| Generation config | Temperature, `top_p`, `top_k`, max tokens, stop sequences, seed, penalties, and log probabilities reach the provider where it supports them. The Responses, Anthropic, Bedrock, Groq, Azure AI Inference, and DeepSeek clients fail with `model.unsupported_config`, naming the field, when a request sets one they cannot honour; the OpenAI-compatible Chat Completions clients send all of them |
| Timeouts | `HttpTimeouts` bounds connecting (30 seconds) and every wait for data (10 minutes) on the OpenAI, OpenAI-compatible, Azure OpenAI, Responses, Azure AI Inference, Groq, DeepSeek, OpenRouter, Ollama, and Bedrock clients; `with_timeouts` changes both. Anthropic requests are bounded at 10 minutes (`AnthropicConfig::with_request_timeout`) |
| Retry | Exponential backoff for 408, 429, 500, 502, 503, 504, and 529 (`RetryConfig`); a `Retry-After` delay is honoured up to `RetryConfig::max_delay` |
| Credentials | Provider configs redact the API key in `Debug` output and omit it when serialized |
| Error privacy | An error message quotes at most 512 bytes of an upstream error body, and clients log a failure's code, category, and status, never its message or a response body. See [Errors in spans and logs](../docs/official_docs/observability/telemetry.md#errors-in-spans-and-logs) |
| Telemetry | Token usage is recorded on `gen_ai.usage.*` span fields through `adk-telemetry` |
| Request adapters | `with_request_adapter` on the OpenAI-compatible, Responses, and Anthropic clients edits the JSON body and headers of each attempt, retries included |
| Response headers | `OpenAIResponsesClient::with_response_observer` receives the raw headers of each generation attempt, failed attempts and retries included, before the body is read; it does not run for background polling |

Provider adapters keep native search and reasoning history, citations, usage, and multimodal tool
results. A streaming snapshot with `provider_metadata.content_complete` set to `true` replaces the
deltas before it.

## Test doubles

`ScriptedLlm` answers each call with the next `ScriptedResponse` (text, function calls, streamed
chunks, a stream error, a failed call, usage) and records every request; a call past the script
fails with `SCRIPT_EXHAUSTED_CODE`. `MockLlm` replays the same responses on every call.

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_model::{ScriptedLlm, ScriptedResponse};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = ScriptedLlm::new("scripted", vec![ScriptedResponse::text("Mars.")]);
    let request = LlmRequest::new("scripted", vec![Content::new("user").with_text("Name a planet.")]);
    let response = model.generate_content(request, false).await?.next().await.ok_or("no response")??;
    assert_eq!(response.content.and_then(|c| c.parts[0].text().map(str::to_string)).as_deref(), Some("Mars."));
    assert_eq!(model.requests().len(), 1);
    Ok(())
}
```

## Feature flags

| Feature | Enables |
|---------|---------|
| `gemini` (default) | `GeminiModel` over `adk-gemini` |
| `gemini-vertex` | Gemini on Vertex AI |
| `gemini-interactions` | Gemini Interactions API transport (Beta) |
| `openai` | `OpenAIClient`, `OpenAIResponsesClient`, `AzureOpenAIClient`, `OpenAICompatible` |
| `openai-ws` | Responses API WebSocket transport (`WsTransport`) |
| `openai-conversations` | Conversations API client (`ConversationsClient`) |
| `openai-responses-full` | `openai-ws` and `openai-conversations` |
| `anthropic` | `AnthropicClient` over `adk-anthropic` |
| `deepseek` | `DeepSeekClient` |
| `groq` | `GroqClient` |
| `ollama` | `OllamaModel` |
| `openrouter` | `OpenRouterClient` |
| `opencode` | `OpenCodeClient` (implies `openai`, `anthropic`, `gemini`) |
| `bedrock` | `BedrockClient` |
| `azure-ai` | `AzureAIClient` |
| `all-providers` | `gemini`, `openai`, `anthropic`, `deepseek`, `ollama`, `groq`, `openrouter`, `opencode`, `bedrock`, `azure-ai` |
| `xai`, `fireworks`, `together`, `mistral`, `perplexity`, `cerebras`, `sambanova` | Aliases for `openai`, kept for compatibility; use the `OpenAICompatibleConfig` presets |

The `openai` feature turns on `async-openai`'s `byot` feature. An application that also calls
`async-openai` directly must set `stream` on its own streaming requests.

## Environment variables

The snippets above read their keys from these variables. Only `GeminiModel::from_env` reads the
environment itself; `BedrockClient::new` uses the AWS credential chain.

| Provider | Variable |
|----------|----------|
| Gemini | `GOOGLE_API_KEY` or `GEMINI_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| Groq | `GROQ_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| OpenCode | `OPENCODE_API_KEY` |
| xAI | `XAI_API_KEY` |
| Azure AI Inference | `AZURE_AI_API_KEY` |
| Amazon Bedrock | AWS credential chain, for example `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` |

## Related crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — the `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) — `LlmAgent` and other agents
- [adk-gemini](https://crates.io/crates/adk-gemini) — the Gemini client behind `GeminiModel`
- [adk-anthropic](https://crates.io/crates/adk-anthropic) — the Anthropic client behind `AnthropicClient`

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
