# Model Providers (Cloud)

The `adk-model` crate provides a client for each cloud LLM provider. Every client implements
the `adk_core::Llm` trait, so an agent takes any of them without other changes.

## Providers

| Provider | Client | `adk-model` feature | Key the samples read | Catalog default |
|----------|--------|---------------------|----------------------|-----------------|
| Gemini | `GeminiModel` | `gemini` (default) | `GOOGLE_API_KEY` | `gemini-3.7-flash` |
| OpenAI | `OpenAIClient`, [`OpenAIResponsesClient`](./openai-responses.md) | `openai` | `OPENAI_API_KEY` | `gpt-5.6-terra` |
| Anthropic | `AnthropicClient` ([details](./anthropic.md)) | `anthropic` | `ANTHROPIC_API_KEY` | `claude-sonnet-5` |
| DeepSeek | `DeepSeekClient` | `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-flash` |
| Groq | `GroqClient` | `groq` | `GROQ_API_KEY` | `openai/gpt-oss-120b` |
| Meta Muse | `MuseClient` | `muse` | `META_API_KEY` | `muse-spark-1.3` |
| OpenRouter | `OpenRouterClient` | `openrouter` | `OPENROUTER_API_KEY` | `qwen/qwen3.7-max` |
| OpenAI-compatible endpoints | `OpenAICompatible` with a preset | `openai` | Per endpoint | Per preset |
| Azure OpenAI | `AzureOpenAIClient` | `openai` | — | Deployment-scoped |
| Azure AI Inference | `AzureAIClient` | `azure-ai` | — | Deployment-scoped |
| Amazon Bedrock | `BedrockClient` | `bedrock` | — | Deployment-scoped |
| OpenCode Go and Zen | `OpenCodeClient` ([details](./opencode.md)) | `opencode` | `OPENCODE_API_KEY` | `deepseek-v4.1-flash` |

The catalog defaults are the constants in `adk_model::catalog` (`GEMINI_DEFAULT`,
`OPENAI_DEFAULT`, ...); `catalog::recommended_model(provider)` returns them. For local models,
see [Ollama](./ollama.md) and [mistral.rs](./mistralrs.md).

### Provider identifiers

`adk_model::ModelProvider` names each provider with its own client, with the identifier CLIs
and configs use. `adk-rust --provider` takes the same identifiers.

| Identifier | Variant | `feature()` | `env_var()` | `default_model()` |
|------------|---------|-------------|-------------|-------------------|
| `gemini` | `Gemini` | `gemini` | `GOOGLE_API_KEY` (`alt_env_var()`: `GEMINI_API_KEY`) | `Some("gemini-3.7-flash")` |
| `openai` | `Openai` | `openai` | `OPENAI_API_KEY` | `Some("gpt-5.6-terra")` |
| `anthropic` | `Anthropic` | `anthropic` | `ANTHROPIC_API_KEY` | `Some("claude-sonnet-5")` |
| `deepseek` | `Deepseek` | `deepseek` | `DEEPSEEK_API_KEY` | `Some("deepseek-flash")` |
| `groq` | `Groq` | `groq` | `GROQ_API_KEY` | `Some("openai/gpt-oss-120b")` |
| `ollama` | `Ollama` | `ollama` | empty, no key | `Some("qwen3.5")` |
| `muse` | `Muse` | `muse` | `META_API_KEY` (`alt_env_var()`: `MODEL_API_KEY`) | `Some("muse-spark-1.3")` |
| `openrouter` | `Openrouter` | `openrouter` | `OPENROUTER_API_KEY` | `Some("qwen/qwen3.7-max")` |
| `opencode` | `Opencode` (Zen) | `opencode` | `OPENCODE_API_KEY` | `Some("deepseek-v4.1-flash")` |
| `opencode-go` | `OpencodeGo` (Go) | `opencode` | `OPENCODE_API_KEY` | `Some("deepseek-v4.1-flash")` |
| `bedrock` | `Bedrock` | `bedrock` | `AWS_BEARER_TOKEN_BEDROCK`; `requires_key()` is false, since the AWS credential chain applies without it | `None` |
| `azure-ai` | `AzureAi` | `azure-ai` | `AZURE_AI_API_KEY` | `None` |

`ModelProvider::ALL` covers every feature `all-providers` enables. The OpenAI-compatible presets
and Azure OpenAI run on the `openai` feature's clients and are not variants.

```rust
use adk_model::ModelProvider;

fn main() -> Result<(), String> {
    let provider: ModelProvider = "bedrock".parse()?;
    assert_eq!(provider.feature(), "bedrock");
    assert_eq!(provider.default_model(), None);
    assert!(!provider.requires_key());
    Ok(())
}
```

## Installation

The complete examples on this page use the `adk-rust` umbrella crate. Its default `minimal`
tier includes Gemini; add the feature of each other provider you use:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["openai", "anthropic", "deepseek", "groq", "muse", "openrouter"] }
anyhow = "1"
dotenvy = "0.15"
tokio = { version = "1", features = ["full"] }
```

To depend on the clients alone, use `adk-model` with the same feature names:

```toml
[dependencies]
adk-model = { version = "3.0.0", features = ["openai", "anthropic"] }
# Every provider at once: gemini, openai, anthropic, deepseek, ollama, groq,
# muse, openrouter, opencode, bedrock and azure-ai.
# adk-model = { version = "3.0.0", features = ["all-providers"] }
```

Put the keys in `.env` or the environment:

```bash
export GOOGLE_API_KEY="your-key"      # Gemini
export OPENAI_API_KEY="your-key"      # OpenAI
export ANTHROPIC_API_KEY="your-key"   # Anthropic
export DEEPSEEK_API_KEY="your-key"    # DeepSeek
export GROQ_API_KEY="your-key"        # Groq
export META_API_KEY="your-key"        # Meta Muse (or MODEL_API_KEY)
export OPENROUTER_API_KEY="your-key"  # OpenRouter
```

## Schema Normalization

Each client normalizes every tool's parameter schema for its provider before the request is
sent, through the `SchemaAdapter` its `Llm::schema_adapter()` returns:

| Client | Adapter |
|--------|---------|
| `GeminiModel` | `GeminiSchemaAdapter::new()` |
| `OpenAIClient`, `AzureOpenAIClient` | `OpenAiSchemaAdapter` |
| `AnthropicClient` | `AnthropicSchemaAdapter` |
| `OllamaModel`, `DeepSeekClient`, and clients that do not override `schema_adapter()` | `GenericSchemaAdapter` |

```rust
use adk_core::{Llm, SchemaAdapter};
use serde_json::{Value, json};

fn normalized(model: &dyn Llm) -> Value {
    let raw = json!({"type": "object", "properties": {"city": {"type": "string"}}});
    model.schema_adapter().normalize_schema(raw)
}
```

See [Schema Normalization](../tools/schema-normalization.md) for what each adapter changes.

---

## Gemini

`GeminiModel` calls the Gemini API (`generateContent` and `streamGenerateContent`). The
`gemini-vertex` feature adds Vertex AI constructors, and `gemini-interactions` adds the
[Interactions API](./gemini-interactions.md) transport.

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

    let agent = LlmAgentBuilder::new("gemini_assistant")
        .description("Gemini-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    // Reads each message from stdin and streams the answer; `exit` quits.
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Models

| Model | Catalog role |
|-------|--------------|
| `gemini-3.7-flash` | Default, balanced |
| `gemini-3.6-flash` | Balanced |
| `gemini-3.5-flash` | Balanced |
| `gemini-3.5-flash-lite` | Economy |
| `gemini-3.1-pro-preview` | Flagship (preview) |

`GeminiModel::new` logs a warning when the model is deprecated or retired in the catalog, naming
its replacement.

### Thinking

Gemini 3 models take a thinking level. `GeminiModel::with_thinking_config` applies a
`ThinkingConfig` to every request:

```rust
use adk_gemini::{ThinkingConfig, ThinkingLevel};
use adk_model::GeminiModel;

fn build(api_key: &str) -> adk_core::Result<GeminiModel> {
    Ok(GeminiModel::new(api_key, "gemini-3.7-flash")?.with_thinking_config(
        ThinkingConfig::new().with_thinking_level(ThinkingLevel::High).with_thoughts_included(true),
    ))
}
```

| Rule | Behaviour |
|------|-----------|
| Thought summaries | With `with_thoughts_included(true)`, they arrive as `Part::Thinking` |
| Thought signatures | `thoughtSignature` values are kept on the parts that carry them and sent back on later turns |
| `thinking_budget` | The token-budget control of earlier models; `ThinkingConfig::validate` rejects a config that sets it together with `thinking_level` |
| `gemini-3.6-flash`, `gemini-3.7-flash` | A request with `temperature`, `top_p`, or `top_k` fails with `model.gemini.sampling_unsupported`, and a `thinking_budget` with `model.gemini.thinking_budget_unsupported` (`InvalidInput`), before anything is sent |

### Failed, Blocked, and Unfinished Responses

`GeminiModel` reports every Gemini response that does not end normally, on the
`generateContent` and `streamGenerateContent` paths alike:

| Response | Result |
|----------|--------|
| An `error` object in a successful body or stream event | An `AdkError` categorized by the object's `code` or `status` (`UNAVAILABLE` → `Unavailable`, `RESOURCE_EXHAUSTED` → `RateLimited`); `details.metadata["provider_error_code"]` holds the code, and `details.upstream_status_code` stays unset because the HTTP status was a success |
| A stream that ends before any chunk carries `finishReason` | `model.gemini.unfinished_stream`, `Unavailable` (retryable) |
| A prompt blocked through `promptFeedback.blockReason` | A final response with `FinishReason::Safety`, `error_code` set to the block reason (for example `PROHIBITED_CONTENT`), and no content; `LlmAgent` ends the run with `model.provider_error` |
| `finishReason` `SAFETY`, `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `IMAGE_SAFETY`, or `MODEL_ARMOR` | `FinishReason::Safety` |
| `finishReason` `RECITATION` | `FinishReason::Recitation` |
| `finishReason` `LANGUAGE`, `MALFORMED_FUNCTION_CALL`, `UNEXPECTED_TOOL_CALL`, `TOO_MANY_TOOL_CALLS`, or `OTHER` | `FinishReason::Other` |

HTTP failures take their category from the status: 500, 502, and the other 5xx statuses are
`Unavailable` and 504 is `Timeout`, all retried; 403 is `Forbidden`.

`examples/gemini_ws_outcomes` runs each case against a scripted local upstream:

```bash
cargo run --manifest-path examples/gemini_ws_outcomes/Cargo.toml
```

---

## OpenAI

`OpenAIClient` calls Chat Completions. [`OpenAIResponsesClient`](./openai-responses.md) calls
the Responses API directly, for reasoning summaries, built-in tools, and server-side state.

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5.6-terra"))?;

    let agent = LlmAgentBuilder::new("openai_assistant")
        .description("OpenAI-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Tool Calls on GPT-5.6 and Later

OpenAI's Chat Completions endpoint rejects function tools on GPT-5.6 and later models
(`gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.6-sol`, `gpt-6-*`) unless reasoning is disabled,
with HTTP 400 `Function tools with reasoning_effort are not supported`. `OpenAIClient`
handles this per request:

| Request | Endpoint |
|---------|----------|
| Declares tools, model GPT-5.6 or later, reasoning not `none` | Responses API (`/responses`) with the same key, organization, project, base URL, reasoning effort, retry policy, and timeouts; `store: false` |
| No tools | Chat Completions |
| Model before GPT-5.6, reasoning effort `None` on the client and no `effort` on the request, or `"reasoning_effort": "none"` in the request's `openai` extension | Chat Completions |
| Client built with `OpenAIClient::compatible` | Chat Completions |

### Structured Output (JSON Schema)

`output_schema` on an agent becomes a strict JSON schema: `response_format` on Chat
Completions, and `text.format` on the Responses API, which also carries every tool-carrying
GPT-5.6 request from `OpenAIClient`:

```rust
use adk_rust::prelude::*;
use serde_json::json;
use std::sync::Arc;

fn build(api_key: &str) -> anyhow::Result<LlmAgent> {
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5.6-terra"))?;

    Ok(LlmAgentBuilder::new("data_extractor")
        .model(Arc::new(model))
        .instruction("Extract person information from the text.")
        .output_schema(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "number" },
                "metadata": {
                    "type": "object",
                    "properties": { "source": { "type": "string" } },
                    "required": ["source"],
                    "additionalProperties": false
                }
            },
            "required": ["name", "age", "metadata"],
            "additionalProperties": false
        }))
        .build()?)
}
```

Strict mode requires `additionalProperties: false` on every object, nested objects included.

When the model declines a request, Chat Completions sends the refusal in `message.refusal`
(streamed as `delta.refusal`) with `content: null`. `OpenAIClient`, `OpenAICompatible` and
its presets, `AzureOpenAIClient`, and OpenCode Chat return the refusal as the response's
text and finish it with `FinishReason::Safety`. `error_code` stays unset, since a refusal is
an answer, not a failure; with `output_schema` set, `LlmAgent` validates the refusal text
like any other answer.

### Reasoning Effort

```rust
use adk_model::openai::{OpenAIClient, OpenAIConfig, OpenAIReasoningEffort};

fn build(api_key: &str) -> Result<OpenAIClient, adk_core::AdkError> {
    OpenAIClient::new_with_reasoning_effort(
        OpenAIConfig::new(api_key, "gpt-5.6-terra"),
        OpenAIReasoningEffort::XHigh,
    )
}
```

`OpenAIReasoningEffort` has `None`, `Minimal`, `Low`, `Medium`, `High`, `XHigh`, and `Max`;
availability depends on the model. Chat Completions has no `max`:
`OpenAIClient::new_with_reasoning_effort` rejects `Max` with
`model.openai.reasoning_effort_unsupported`, so use `OpenAIResponsesClient` for it. The
three-value `ReasoningEffort` (`Low`, `Medium`, `High`), set with
`OpenAIConfig::with_reasoning_effort`, remains for compatibility.

### OpenAI-Compatible Endpoints

`OpenAIConfig::compatible` points `OpenAIClient` at another Chat Completions server, such as
Ollama's `/v1`, vLLM, or LM Studio:

```rust
use adk_model::openai::{OpenAIClient, OpenAIConfig};

fn build() -> Result<OpenAIClient, adk_core::AdkError> {
    // Ollama ignores the API key.
    OpenAIClient::new(OpenAIConfig::compatible("not-needed", "http://localhost:11434/v1", "qwen3.5"))
}
```

`OpenAICompatible` with an `OpenAICompatibleConfig` preset (`fireworks`, `together`,
`mistral`, `perplexity`, `cerebras`, `sambanova`, `xai`, `minimax`, `gemini`,
`orcarouter`, and others) sets the base URL and provider name. Structured output needs
support on the server.

#### Reasoning History

Reasoning replay is disabled by default. `Part::Thinking` stays separate from visible message
text regardless of the replay setting.

| Client setting | Assistant history field |
|---|---|
| Default or `with_reasoning_replay(false)` | Neither field |
| `with_reasoning_replay(true)` | `reasoning_content` |
| `with_reasoning_replay_field(ReasoningReplayField::Reasoning)` | `reasoning` |

```rust
use adk_model::{OpenAICompatible, OpenAICompatibleConfig, ReasoningReplayField};

fn build() -> Result<OpenAICompatible, adk_core::AdkError> {
    let config = OpenAICompatibleConfig::new("key", "model").with_base_url("http://localhost:8000/v1");
    // For vLLM endpoints that accept `reasoning`.
    Ok(OpenAICompatible::new(config)?
        .with_reasoning_replay(true)
        .with_reasoning_replay_field(ReasoningReplayField::Reasoning))
}
```

The last reasoning-replay builder call determines the setting. Enable replay only for
endpoints that accept the chosen field. This setting controls history serialization, not
thinking generation. Standard OpenAI and Azure Chat Completions omit both fields by default.

#### Gemini Through the OpenAI-Compatible Endpoint

`OpenAICompatibleConfig::gemini` targets
`https://generativelanguage.googleapis.com/v1beta/openai` with a Gemini API key:

```rust
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

fn build() -> anyhow::Result<OpenAICompatible> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    Ok(OpenAICompatible::new(OpenAICompatibleConfig::gemini(api_key, "gemini-3.7-flash"))?)
}
```

Every key of a request's `extensions["openai"]` object is written into the request body as is,
so Gemini-specific options such as `{"extra_body": {"google": {"thinking_config": {...}}}}`
reach the endpoint. Use [`GeminiModel`](#gemini) for server-side tools, the Interactions API,
and native `ThinkingConfig`.

```bash
# Chat, reasoning effort, extra_body thinking, streaming, function calling, structured output.
cargo run -p adk-model --features openai --example gemini_openai_compat

# The same client driving an LlmAgent in a Runner.
cargo run -p adk-agent --example gemini_openai_compat_agent
```

#### OrcaRouter

`OpenAICompatibleConfig::orcarouter` targets `https://api.orcarouter.ai/v1`. Model IDs carry a
vendor prefix (`openai/gpt-5.6-terra`, `anthropic/claude-sonnet-5`), so changing the model
string switches the upstream vendor:

```rust
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

fn build() -> anyhow::Result<OpenAICompatible> {
    let api_key = std::env::var("ORCAROUTER_API_KEY")?;
    Ok(OpenAICompatible::new(OpenAICompatibleConfig::orcarouter(api_key, "openai/gpt-5.6-terra"))?)
}
```

```bash
# Chat, tool calling, and a second vendor on the same client (requires ORCAROUTER_API_KEY).
cargo run --manifest-path examples/orcarouter/Cargo.toml
```

### Models

| Model | Catalog role |
|-------|--------------|
| `gpt-5.6-terra` | Default, balanced |
| `gpt-5.6-sol` | Flagship |
| `gpt-5.6-luna` | Economy |
| `gpt-5.6` | Flagship |

---

## Anthropic

See [Anthropic](./anthropic.md) for the `adk-anthropic` client, thinking, caching, and
gateways.

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-5"))?;

    let agent = LlmAgentBuilder::new("anthropic_assistant")
        .description("Anthropic-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Request Defaults

| Setting | Default | Override |
|---------|---------|----------|
| `max_tokens` | 32,000 (`ANTHROPIC_DEFAULT_MAX_TOKENS`) for Claude 4 and later model IDs (`claude-opus-*`, `claude-sonnet-*`, `claude-haiku-*`, `claude-fable-*`, `claude-mythos-*`), 4,096 for others (`adk_model::catalog::anthropic_default_max_tokens`) | `AnthropicConfig::with_max_tokens`, or `max_output_tokens` per request |
| Request timeout | 10 minutes for a whole non-streaming request, such as an agent called through `AgentTool`, and for the response headers of a stream; connecting is capped at 60 seconds | `AnthropicConfig::with_request_timeout` |
| Streamed body | Fails after 30 seconds without data | — |

Thinking counts toward `max_tokens`, and Claude 5 models think on every turn, so a small cap
ends turns with `max_tokens` before the answer.

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use std::time::Duration;

fn build() -> Result<AnthropicClient, adk_core::AdkError> {
    let config = AnthropicConfig::new("sk-ant-xxx", "claude-opus-5-5")
        .with_max_tokens(64_000)
        .with_request_timeout(Duration::from_secs(1_800));
    AnthropicClient::new(config)
}
```

### Models

| Model | Catalog role |
|-------|--------------|
| `claude-sonnet-5` | Default, balanced |
| `claude-fable-5-1` | Flagship |
| `claude-opus-5-5` | Flagship |
| `claude-sonnet-5-5` | Balanced |
| `claude-haiku-5-5` | Economy |
| `claude-opus-5` | Flagship |
| `claude-fable-5` | Flagship |
| `claude-haiku-4-5` | Economy |

---

## DeepSeek

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    // `deepseek-flash`; `DeepSeekClient::reasoner` selects `deepseek-v4-pro` with thinking.
    let model = DeepSeekClient::chat(&api_key)?;

    let agent = LlmAgentBuilder::new("deepseek_assistant")
        .description("DeepSeek-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Models

| Model | Constructors |
|-------|--------------|
| `deepseek-flash` (default) | `DeepSeekClient::chat`, `DeepSeekClient::v4_flash` |
| `deepseek-v4-pro` | `DeepSeekClient::reasoner` (thinking on), `DeepSeekClient::v4_pro` (thinking on, effort `High`) |

DeepSeek retired `deepseek-chat`, `deepseek-reasoner`, and the `deepseek-v4-flash` alias on
2026-07-24. With thinking on, the reasoning arrives as `Part::Thinking`.

### Web search

`DeepSeekClient` speaks Chat Completions, which accepts function tools only. DeepSeek runs
web search on its other endpoints:

| Endpoint | Client | Tool | Models |
|---|---|---|---|
| Responses, `https://api.deepseek.com` ([guide](https://api-docs.deepseek.com/guides/responses_api)) | `OpenAIResponsesClient` (feature `openai`) | `DeepSeekWebSearchTool`, `{"type": "web_search"}` | `deepseek-v4-pro`; `deepseek-flash` ignores the tool |
| Anthropic-compatible, `https://api.deepseek.com/anthropic` ([guide](https://api-docs.deepseek.com/guides/anthropic_api)) | `AnthropicClient` (feature `anthropic`) | `WebSearchTool`, `web_search_20250305` | Both |
| Search API, `https://api.deepseek.com/v1/tools/search` ([guide](https://api-docs.deepseek.com/guides/search)) | `adk_harness::DeepSeekSearch` | The harness's governed `web_search` | Any model |

Neither endpoint documents a domain allowlist for the tool. Both clients report the search
from `Llm::native_web_search()`, which the harness reads to attach it by default.

```rust
use adk_rust::prelude::*;
use adk_rust::model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_tool::DeepSeekWebSearchTool;
use std::sync::Arc;

fn build() -> anyhow::Result<LlmAgent> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    let model = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new(api_key, "deepseek-v4-pro").with_base_url("https://api.deepseek.com"),
    )?;
    Ok(LlmAgentBuilder::new("researcher")
        .model(Arc::new(model))
        .tool(Arc::new(DeepSeekWebSearchTool::new()))
        .build()?)
}
```

---

## Groq

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GROQ_API_KEY")?;
    let model = GroqClient::new(GroqConfig::gpt_oss_120b(&api_key))?;

    let agent = LlmAgentBuilder::new("groq_assistant")
        .description("Groq-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Models

| Model | Config |
|-------|--------|
| `openai/gpt-oss-120b` (default) | `GroqConfig::gpt_oss_120b(key)` |
| `openai/gpt-oss-20b` (faster, lower cost) | `GroqConfig::gpt_oss_20b(key)` |
| Any model Groq serves | `GroqConfig::new(key, model)` |

---

## Meta Muse

`MuseClient` calls the Meta Model API (`https://api.meta.ai/v1`) Chat Completions
endpoint, which is drop-in OpenAI-compatible. The key is read from `META_API_KEY`
(Meta also documents `MODEL_API_KEY`).

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("META_API_KEY")?;
    let model = MuseClient::new(MuseConfig::spark_1_3(&api_key))?;

    let agent = LlmAgentBuilder::new("muse_assistant")
        .description("Muse-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Models

| Model | Config |
|-------|--------|
| `muse-spark-1.3` (default, recommended for new work) | `MuseConfig::spark_1_3(key)` |
| `muse-spark-1.2` (previous version) | `MuseConfig::spark_1_2(key)` |
| `muse-spark-1.1` (original version) | `MuseConfig::spark_1_1(key)` |
| Any model the API serves | `MuseConfig::new(key, model)` |

The `-contributor` checkpoints (`muse-spark-1.3-contributor`,
`muse-spark-1.2-contributor`) work through `MuseConfig::new`. Contributor pricing
is discounted in exchange for permission to train on request content, so no preset
selects one. Rate limits are 3,000 requests and 4,000,000 tokens per minute per team.
Spark reasoning counts toward `max_tokens`, so the client raises smaller output caps
to 1,024 (`MUSE_MIN_MAX_TOKENS`) instead of returning empty `length` responses, and
maps `GenerateContentConfig::effort` to the API's `reasoning_effort`.

---

## OpenRouter

`OpenRouterClient` calls OpenRouter's native Chat Completions API by default, or its Responses
API with `OpenRouterApiMode::Responses`.

### Complete Example

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENROUTER_API_KEY")?;
    let config = OpenRouterConfig::new(&api_key, "openai/gpt-5.6-luna")
        // Optional app attribution: `HTTP-Referer`, `X-OpenRouter-Title` and `X-Title`.
        .with_title("my-agent");
    let model = OpenRouterClient::new(config)?;

    let agent = LlmAgentBuilder::new("openrouter_assistant")
        .description("OpenRouter-powered assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

OpenRouter-specific request options — API mode, fallback `models`, `route`, provider
preferences, plugins, reasoning — travel in `GenerateContentConfig::extensions["openrouter"]`,
written with `OpenRouterRequestOptions::insert_into_config`:

```rust
use adk_core::GenerateContentConfig;
use adk_model::openrouter::{OpenRouterApiMode, OpenRouterRequestOptions};

fn config() -> adk_core::Result<GenerateContentConfig> {
    let mut config = GenerateContentConfig::default();
    OpenRouterRequestOptions::default()
        .with_api_mode(OpenRouterApiMode::Responses)
        .with_models(vec!["openai/gpt-5.6-luna".to_string(), "qwen/qwen3.7-max".to_string()])
        .insert_into_config(&mut config)?;
    Ok(config)
}
```

An error OpenRouter relays from an upstream provider carries the generic message "Provider
returned error"; the `AdkError` message appends the provider name and its explanation, cut
to 512 bytes. `adk-model/examples/openrouter_chat.rs`, `openrouter_responses.rs`,
`openrouter_discovery.rs`, and `openrouter_adapter.rs` cover each API.

---

## Usage Attribution and Cost

Every `adk-model` provider sets `LlmResponse::provider` and `LlmResponse::model`
— the model version the provider reports when it reports one, otherwise the
configured identifier — and fills `UsageMetadata::cost` in USD when the provider
did not report a cost itself. Rates come from `adk_model::pricing::PricingCatalog`,
which consolidates the Gemini, OpenAI and Anthropic pricing modules and adds
DeepSeek, Groq, xAI and Mistral list prices. Each vendor table cites the
published price page and the date it was read; `PricingCatalog::version()`
returns that verification date (`PRICING_EFFECTIVE_DATE`).

| Rule | Behaviour |
|------|-----------|
| Cached tokens | `prompt_token_count` includes cache reads and writes; each prompt token is billed once, at the uncached, cache-read or cache-write rate |
| Anthropic cache writes | The 1-hour share of `cache_creation_input_tokens` bills at the 1-hour rate, the rest at the 5-minute rate |
| Long context | Gemini Pro models (over 200K), OpenAI GPT-5.4/5.5 (272K and above) and Claude Haiku 5.5 (over 100K) switch the whole request to the long-context rates; GPT-5.6 long-context rates are not applied |
| Dated aliases | `gpt-4.1-2025-04-14`, `claude-sonnet-4-5-20250929`, `claude-sonnet-4-5@20250929` and `us.anthropic.claude-sonnet-4-5-20250929-v1:0` resolve to the listed model |
| Billing provider | A vendor's list price applies only to providers that bill it at that price; an OpenAI model name served by another gateway stays unpriced |
| Gemini | `gemini-3.7-flash` requests are reported as `gemini-3.8-flash`; both are priced at the Gemini 3.8 Flash rates, which change on 2027-01-01 |
| DeepSeek | `deepseek-flash` (and the retired `deepseek-v4-flash` alias) and `deepseek-v4-pro` at peak rates during 01:00–04:00 and 06:00–10:00 UTC on weekdays, half price otherwise; Chinese public holidays bill at peak; `deepseek-chat` and `deepseek-reasoner` are unpriced |
| Groq | GPT-OSS and Qwen models at Groq's published rates; models Groq prices only on request (Llama, MiniMax) stay unpriced |
| xAI | Grok models, with the whole request at the long-context rate once the prompt reaches 200K tokens |
| Mistral | Current models and their `-latest` aliases; retired dated ids and Mistral Large 4 (sale price) stay unpriced |
| Ollama | Local models cost zero; `*cloud*` models stay unpriced |
| OpenCode | Responses are attributed to `opencode` / `opencode-go` and stay unpriced |
| Unknown models | `cost` stays `None`, which means unknown, never free |

```rust
use adk_core::UsageMetadata;
use adk_model::pricing::{PRICING_EFFECTIVE_DATE, PricingCatalog};

fn main() {
    let usage = UsageMetadata {
        prompt_token_count: 1_000_000,
        candidates_token_count: 200_000,
        cache_read_input_token_count: Some(400_000),
        ..Default::default()
    };
    let catalog = PricingCatalog::standard();
    let cost = catalog.cost_usd(Some("gemini"), "gemini-3.5-flash-lite", &usage);
    // 600K × $0.30 + 400K × $0.03 + 200K × $2.50 per million tokens.
    assert!((cost.unwrap() - 0.692).abs() < 1e-9);
    assert_eq!(catalog.version(), PRICING_EFFECTIVE_DATE);
}
```

### Incomplete usage reports

Compatible endpoints report a token count they did not compute as `null` or leave it out.
A count like that is unknown, never a reason to fail the turn: the tool call in the same
response still runs, and the counts that were reported reach `UsageMetadata` and run budgets.

| Field | A `null` or missing count reads as |
|-------|------------------------------------|
| `prompt_token_count`, `candidates_token_count` | `0` |
| `total_token_count` | The sum of the reported prompt and output counts |
| `cache_read_input_token_count`, `thinking_token_count` and the other breakdowns | `None` |

`UsageMetadata::provider_usage` holds the provider's usage object exactly as received,
including fields `UsageMetadata` does not project:

| Provider | `provider_usage` |
|----------|------------------|
| OpenAI Chat and compatible presets, OpenAI Responses, DeepSeek, Azure AI | The response's `usage` object |
| Groq | `usage`, or `x_groq.usage` when only that is present, on the finish chunk |
| Anthropic | `usage`; for a stream, the `message_start` usage with the `message_delta` usage fields applied over it |
| Gemini | `usageMetadata`, including `toolUsePromptTokenCount`, `trafficType` and `serviceTier` |
| Gemini Interactions | The interaction's `usage` object |
| Ollama | The metric fields of the final line (`prompt_eval_count`, `eval_count` and the durations) that Ollama sent |
| OpenRouter | The usage detail objects (`prompt_tokens_details`, `completion_tokens_details`, `cost_details`) and unknown fields |
| Bedrock | The Converse SDK's token counts (`inputTokens`, `outputTokens`, `totalTokens`, cache reads and writes) |

> **Note:** Ollama leaves out a metric that is zero, such as `prompt_eval_count` when the
> whole prompt came from its cache. A final line without it, streamed or not, still reports
> `eval_count`.

### Built-in tool uses and fees

Providers report the built-in tools they ran in `UsageMetadata::builtin_tool_uses`, one
`BuiltinToolUse { kind, count }` per kind. `cost` adds the published per-use fee of every priced
kind to the token cost.

| Kind | Reported from | Fee in `PricingCatalog::standard()` |
|------|---------------|--------------------------------------|
| `web_search` | Anthropic `usage.server_tool_use.web_search_requests` | $10 per 1,000 searches |
| `web_fetch` | Anthropic `usage.server_tool_use.web_fetch_requests` | None — token cost only |
| `code_execution` | Anthropic `usage.server_tool_use.code_execution_requests` | Unpriced (billed per container hour) |
| `web_search` | OpenAI Responses `web_search_call` items | $10 per 1,000 calls |
| `file_search` | OpenAI Responses `file_search_call` items | $2.50 per 1,000 calls |
| `code_execution` | OpenAI Responses `code_interpreter_call` items | Unpriced (billed per container session) |
| `image_generation`, `mcp` | OpenAI Responses `image_generation_call`, `mcp_call` items | Unpriced |
| `google_search` | Gemini `groundingMetadata.webSearchQueries`, one per distinct query | Gemini 3 and newer: $14 per 1,000 queries. Gemini 2.5 Pro and Flash: $35 per 1,000 grounded prompts |
| `google_maps` | Gemini grounding chunks with a Maps source, one per response | Gemini 2.5 Pro and Flash: $25 per 1,000 grounded prompts. Gemini 3: unpriced (billed per Maps query, which responses do not report) |
| `code_execution` | Gemini `executableCode` parts | None — token cost only |

- **Vendor API only**: fees apply when the provider is the vendor's own API (`gemini`, `openai`,
  `anthropic`); Bedrock, Azure and other gateways keep the token cost only.
- **Free allowances are not tracked**: Gemini's monthly search allowance and similar quotas are
  ignored, so a run inside an allowance is over-estimated.
- **Unpriced kinds add nothing**: the uses still count against the tool-call budget.
- **OpenAI `web_search_preview` on non-reasoning models** is billed at $25 per 1,000 calls; the
  response does not name the tool version, so the catalog charges the $10 rate.

Add fees for other kinds or contracts with `PricingCatalog::with_builtin_tool_fee`:

```rust
use adk_core::{BuiltinToolUse, UsageMetadata};
use adk_model::pricing::{BuiltinToolFee, ModelPrice, ModelRates, PriceVendor, PricingCatalog};

fn main() {
    let catalog = PricingCatalog::new("contract-2026")
        .with_price(ModelPrice::new(PriceVendor::Google, "gemini-3.7-flash", ModelRates::new(0.75, 3.75)))
        .with_builtin_tool_fee(
            BuiltinToolFee::per_use(PriceVendor::Google, "google_search", 0.010)
                .for_models(["gemini-3*"]),
        );
    let usage = UsageMetadata {
        builtin_tool_uses: vec![BuiltinToolUse::new("google_search", 3)],
        ..Default::default()
    };
    let cost = catalog.cost_usd(Some("gemini"), "gemini-3.7-flash", &usage).unwrap();
    assert!((cost - 0.03).abs() < 1e-9);
}
```

Run budgets read this cost to enforce a spend cap, and count each built-in tool use as a tool
call; see [Run Budgets](../core/runner.md#run-budgets).

---

## Retries, Tool Calls, and Credentials

These rules apply to every HTTP provider client in `adk-model` (OpenAI and
OpenAI-compatible, Azure OpenAI, Azure AI Inference, Anthropic, DeepSeek, Groq).

| Behaviour | Rule |
|-----------|------|
| Transient upstream errors | HTTP 500, 502, 503, 504, and 529 map to `ErrorCategory::Unavailable` with the upstream status recorded, so `RetryConfig` retries them |
| `Retry-After` | A delay in seconds or an HTTP-date is attached to the error and used for the next retry. A delay above `RetryConfig::max_server_delay` (default 60 seconds) is not waited: the error is returned with the delay on `AdkError::retry` |
| Gemini `RetryInfo` | Without a `Retry-After` header, Gemini (AI Studio and Vertex AI) takes the delay from the `retryDelay` of a `google.rpc.RetryInfo` error detail (`"37s"`, `"1.5s"`), which is how the API reports the wait a 429 quota error needs, under the same bound |
| Mid-stream Anthropic `error` event | Fails the stream with the category of the equivalent HTTP error, so `overloaded_error` and `rate_limit_error` are retryable |
| Provider error codes | `AdkError::details.metadata["provider_error_code"]` holds the provider's own code as a string: the first Google `ErrorInfo.reason`, then the error object's string `code`, `type`, or `status`. Gemini (AI Studio and Vertex AI), OpenAI (Chat Completions and Responses), OpenAI-compatible, Azure OpenAI, Azure AI Inference, DeepSeek, Anthropic, and OpenRouter record it; codes over 128 bytes or outside `[A-Za-z0-9_.-]` are dropped |
| Malformed tool arguments | Arguments that are not a JSON object, streamed or not (truncated JSON, an array, a number), or an Anthropic call cut off by `max_tokens`, become `adk_core::invalid_tool_arguments(reason, raw)`; the turn continues and `LlmAgent` answers the call with an error without executing it, so the model can send it again. An empty payload is a zero-argument call with `{}` |
| Text-encoded tool calls | Recognised only when the request declares the named tool; a ` ```json ` fence also needs an `arguments` or `parameters` object. DeepSeek-V3 native tokens (`<｜tool▁call▁begin｜>function<｜tool▁sep｜>NAME` with a fenced arguments object, and the V3.1 `NAME<｜tool▁sep｜>{...}` form) become calls. The OpenAI-compatible clients and `OllamaModel` recognise them in streamed and complete responses |
| Several function responses in one content | The Chat Completions clients (OpenAI-compatible and its presets, OpenCode Chat, Azure OpenAI, Groq, DeepSeek, OpenRouter) send one `tool` message per response, in order, and the content's other parts as a user message after them; these clients and Azure AI Inference also send function responses in a user-role content as `tool` messages |
| Images a tool returns | Anthropic sends them inside the `tool_result` block and Bedrock inside the `toolResult` (Bedrock also carries supported documents); the OpenAI clients, Groq, DeepSeek, Azure AI Inference, and Ollama, whose tool messages carry only text, send them in a user message after the tool messages |
| Tool order | Anthropic and Bedrock send tool definitions sorted by name, so the prompt-cache prefix is stable |
| API keys | Provider configs redact the key in `Debug` output and omit it when serialized |
| Error messages | Every client, Gemini, Ollama, and OpenRouter included, quotes an HTTP error body or a provider's error message up to `adk_core::UPSTREAM_EXCERPT_BYTES` (512 bytes), since it can echo the prompt or tool arguments. A response that cannot be decoded is described by its decoder error class and position (`adk_core::json_error_summary`), never quoted |
| Logs | Clients, the retry loop, and `adk-gemini` log a failure's diagnostics (`AdkError::diagnostics()`: category, code, upstream status, provider, provider code, request ID) and never its message, a response body, or a response; see [Errors in spans and logs](../observability/telemetry.md#errors-in-spans-and-logs) |
| Redirects | Anthropic, Gemini, OpenAI, OpenAI-compatible, Azure OpenAI, Azure AI Inference and DeepSeek clients return a 3xx response as an error instead of following it |
| Inline PDFs | OpenAI and Azure OpenAI receive a `file` part; other OpenAI-compatible providers receive the text fallback |

```rust
use std::time::Duration;

use adk_model::RetryConfig;
use adk_model::catalog::GROQ_DEFAULT;
use adk_model::groq::{GroqClient, GroqConfig};

fn build() -> Result<GroqClient, adk_core::AdkError> {
    let api_key = std::env::var("GROQ_API_KEY").unwrap_or_default();
    let retry = RetryConfig::default().with_max_delay(Duration::from_secs(30));
    Ok(GroqClient::new(GroqConfig::new(api_key, GROQ_DEFAULT))?.with_retry_config(retry))
}
```

### Generation Config Fields

Each client sends the `GenerateContentConfig` fields its API has a place for. A field the
client cannot honour fails the request before anything is sent, with
`model.unsupported_config` (`ErrorCategory::InvalidInput`) and a message naming the field,
instead of a generation that silently runs without it.

| Client | `stop_sequences` | `seed` | `frequency_penalty`, `presence_penalty` | `top_logprobs` |
|--------|------------------|--------|-----------------------------------------|----------------|
| Chat Completions (OpenAI, OpenAI-compatible, Azure OpenAI, OpenCode Chat) | `stop` | `seed` | sent | `logprobs` and `top_logprobs`; tokens in `provider_metadata["logprobs"]` |
| OpenAI Responses | rejected | rejected | rejected | `top_logprobs`; tokens in `provider_metadata["openai"]["logprobs"]` |
| Gemini (`generateContent`, AI Studio and Vertex AI) | `stopSequences` | `seed` | `frequencyPenalty`, `presencePenalty` | `responseLogprobs` and `logprobs`; Gemini's `logprobsResult` in `provider_metadata["logprobs"]` |
| Gemini Interactions | `stop_sequences` | `seed` | rejected | rejected |
| Anthropic | `stop_sequences` | rejected | rejected | rejected |
| Bedrock | `stopSequences` | rejected | rejected | rejected |
| Groq | `stop` | `seed` | sent | rejected |
| DeepSeek | `stop` | rejected | sent | rejected |
| Azure AI Inference | `stop` | `seed` | sent | rejected |
| Ollama | `options.stop` | `options.seed` | `options.frequency_penalty`, `options.presence_penalty` | `logprobs` and `top_logprobs`; tokens in `provider_metadata["logprobs"]` |
| OpenRouter Chat Completions | `stop` | `seed` | sent | `top_logprobs`; tokens in `provider_metadata["logprobs"]` |
| OpenRouter Responses | `stop` | rejected | sent | `top_logprobs`; tokens in `provider_metadata["logprobs"]` |

Where the table says tokens are in `provider_metadata["logprobs"]` without naming another
shape, the value is `{"content": [...]}` with one entry per token, as Chat Completions
returns it; a streamed answer carries every token's entry on its final response.
`OpenAIClient` requests routed through the Responses API follow the OpenAI Responses row.

A model can still reject a field its API accepts: GPT-5 models answer `stop` with HTTP 400,
so stop sequences on OpenAI need a model such as `gpt-4.1-mini`, and current Gemini models
(`gemini-3.5-flash-lite`, `gemini-3.7-flash`) answer penalties and log probabilities with
`INVALID_ARGUMENT` ("Penalty is not enabled for this model"), which fails the request with
`ErrorCategory::InvalidInput`. `top_k` is not covered by this check, and `OllamaModel` does
not send `max_output_tokens`.

```rust
use adk_core::GenerateContentConfig;

// Stops at the first "7" on every client that sends `stop_sequences`.
fn config() -> GenerateContentConfig {
    GenerateContentConfig { stop_sequences: vec!["7".to_string()], ..Default::default() }
}
```

### Per-Call Reasoning Effort

`GenerateContentConfig::effort` sets the reasoning effort of one call (`Effort::Low`,
`Medium`, or `High`). A set value wins over the effort the client was built with; `None`
keeps the client's configuration. The harness sets it on every request from its
`EffortPolicy` (spec 3.1 §7.23).

| Client | Sent as | Models that take it |
|--------|---------|---------------------|
| `AnthropicClient` | `output_config.effort`, with `thinking: {type: "adaptive"}` when the client sets no thinking mode | Claude 4.6 and later; Claude Opus 4.5 takes the effort without adaptive thinking |
| `OpenAIClient`, `OpenAICompatible` and its presets, `AzureOpenAIClient`, OpenCode Chat | `reasoning_effort` | the o-series, GPT-OSS, GPT-5 and later except `-chat` models, and any model whose client was built with an effort |
| `OpenAIResponsesClient`, and `OpenAIClient` when it routes a tool call through Responses | `reasoning.effort` | the same |
| `GeminiModel`, both transports | `thinking_level`, replacing a configured token budget | Gemini 3 and later, and any model whose client sets a thinking level |
| `OpenRouterClient` | `reasoning.effort`, unless the request's `openrouter` options set an effort, a token budget, or `enabled: false` | every model |
| `DeepSeekClient` | `reasoning_effort: "high"` (the API serves `low` and `medium` as `high`) | every model, unless the client disables thinking |
| `GroqClient` | `reasoning_effort` | GPT-OSS models |
| `OllamaModel`, `BedrockClient`, `AzureAIClient` | not sent | none |

A model that does not take the effort runs with its client's configuration, and the client
logs the ignored value at debug level. A request's `openai` extension still overrides the
Chat Completions body, and its `reasoning` object the Responses one. A request that sets an
effort on an `OpenAIClient` built with reasoning effort `None` turns reasoning on for that
call, so a GPT-5.6 tool call goes through the Responses API.

```rust
use adk_core::{Effort, GenerateContentConfig};

// Every call made with this config runs at high effort, whatever the client's default.
fn config() -> GenerateContentConfig {
    GenerateContentConfig { effort: Some(Effort::High), ..Default::default() }
}
```

### Stream Termination

A streamed response is complete only when the provider sends its terminal event. A stream
that ends without one, ends while a tool call is still arriving, or reports an error fails
with an `AdkError` instead of returning the text received so far as a successful answer.
`LlmAgent` then fails the run: the session stores no answer for the turn, `output_key`
state is not written, and no tool from the turn runs.

| Client | Complete when the stream sends | Ends early | Error inside the stream |
|--------|--------------------------------|------------|-------------------------|
| `OpenAIClient` Chat Completions, `OpenAICompatible` and its presets, OpenCode Chat | a `finish_reason`, or `data: [DONE]` with no tool call still arriving | `model.openai_compat.incomplete_stream` | `model.openai_compat.stream_error` |
| `AzureOpenAIClient` | the same | `model.azure_openai.incomplete_stream` | `model.azure_openai.stream_error` |
| `GroqClient` | the same | `model.groq.incomplete_stream` | `model.groq.stream_error` |
| `OllamaModel` | a line with `"done": true` | `model.ollama.incomplete_stream` | `model.ollama.stream_error` |
| `OpenRouterClient`, Chat Completions | a `finish_reason`, or `data: [DONE]` with no tool call still arriving | `model.openrouter.chat_stream_incomplete` | From the error's HTTP code, else `model.openrouter.stream_error` |
| `OpenRouterClient`, Responses | `response.completed` or `response.incomplete` | `model.openrouter.responses_stream_incomplete` | `response.failed`: from the error's HTTP code, else `model.openrouter.response_failed` |

- **Ends early** errors, and a connection that fails mid-stream, are
  `ErrorCategory::Unavailable`, and a stream that sends nothing for the idle timeout (see
  [Timeouts](#timeouts)) is `ErrorCategory::Timeout`; both are retryable.
- **Error inside the stream** covers a chunk with a top-level `error` object (an Ollama
  `{"error": "..."}` line included) and a chunk that finishes with `finish_reason: "error"`.
  For the OpenAI-compatible, Azure OpenAI, Groq, and Ollama clients, and an OpenRouter
  chunk that only finishes with `error`, the category follows a numeric `code` in the HTTP
  error range, else the provider code (`rate_limit_exceeded` is `RateLimited`,
  `invalid_request_error` is `InvalidInput`), else `Unavailable`. The provider's message is
  quoted up to 512 bytes and its code is kept in `details.metadata["provider_error_code"]`.
  A Chat Completions body with an `error` object and HTTP 200 fails the same way without
  streaming. OpenRouter error objects, in Chat Completions chunks and in `response.failed`,
  take the status-specific code and category of a numeric `code` (502 is
  `model.openrouter.bad_gateway`, `Unavailable`), else `Internal`.
- **Undecodable chunks** fail with `model.openai_compat.parse`, `model.azure_openai.parse`,
  `model.groq.invalid_chunk`, or `model.ollama.stream_chunk` (`Internal`). The message names
  only the decoder's error class and position, never the chunk. A chunk that cannot be decoded
  after the response finished, such as a cut-off usage trailer, is ignored.
- **Unknown finish reasons** map to `FinishReason::Other`, not `Stop`, on the Chat Completions,
  Azure OpenAI, Groq, and OpenRouter clients and on `DeepSeekClient` and `AzureAIClient`,
  streamed or not. `OllamaModel` maps the final line's `done_reason`: `stop` to
  `FinishReason::Stop`, `length` (the `max_output_tokens` cap, sent as `options.num_predict`)
  to `FinishReason::MaxTokens`, and any other value to `FinishReason::Other`.
- **Reasoning in the final chunk**: `reasoning_content` or `reasoning` sent in the chunk that
  carries `finish_reason` is emitted as a `Part::Thinking` delta before the final text or tool
  calls.
- **SSE framing**: `data:` fields are read with or without the space after the colon, a
  last line without a line ending is read, and a UTF-8 character split across network
  chunks is decoded whole.
- **OpenRouter Responses**: `response.incomplete` for `max_output_tokens` finishes with
  `FinishReason::MaxTokens` and for `content_filter` with `FinishReason::Safety`; any other
  reason fails with `model.openrouter.response_incomplete`, and a `cancelled` status with
  `model.openrouter.response_cancelled`. A non-streaming response with status `failed`
  fails with `model.openrouter.response_failed`. Text and reasoning stream as partial
  deltas; tool calls arrive in the terminal response, whose content is complete and
  replaces the deltas. A tool call that is unfinished or unnamed fails the turn instead of
  being dropped, and arguments that are not a JSON object are marked as described under
  "Malformed tool arguments", in Chat Completions and Responses.

`DeepSeekClient` and `AzureAIClient` streams are complete when a chunk carries a
`finish_reason`. Their failures take the categories above (a stream idle past the timeout
is `Timeout`), and an error object's message and code are kept the same way:

| Client | Ends early | Error inside the stream | Undecodable chunk |
|--------|------------|-------------------------|-------------------|
| `DeepSeekClient` | `model.deepseek.incomplete_stream` | `model.deepseek.stream_error` | `model.deepseek.invalid_chunk` |
| `AzureAIClient` | `model.azure_ai.incomplete_stream` | `model.azure_ai.stream_error` | `model.azure_ai.invalid_chunk` |

[OpenAI Responses](./openai-responses.md#failed-and-unfinished-responses),
[Gemini](#failed-blocked-and-unfinished-responses), and
[Anthropic](./anthropic.md#retries-and-streaming) apply the same rule with their own codes.

```rust
use adk_core::{AdkError, Llm, LlmRequest};
use adk_model::groq::GroqClient;
use futures::StreamExt;

/// Returns the complete streamed text, sending the request again once when the stream
/// ends early.
async fn answer(client: &GroqClient, request: LlmRequest) -> Result<String, AdkError> {
    match stream_text(client, request.clone()).await {
        Err(error) if error.is_retryable() => stream_text(client, request).await,
        result => result,
    }
}

async fn stream_text(client: &GroqClient, request: LlmRequest) -> Result<String, AdkError> {
    let mut stream = client.generate_content(request, true).await?;
    let mut text = String::new();
    while let Some(response) = stream.next().await {
        // A truncated stream ends here with an error, never with a partial answer.
        let response = response?;
        // Groq sends each piece of text once, in deltas or in the finishing chunk.
        for part in response.content.iter().flat_map(|content| &content.parts) {
            text.push_str(part.text().unwrap_or_default());
        }
    }
    Ok(text)
}
```

### Timeouts

Every client except `GeminiModel` takes an `HttpTimeouts` that bounds its
HTTP transport, so a stream that stops sending data fails the run instead of holding it.
Anthropic retains its request-timeout defaults unless `with_timeouts` selects byte-inactivity
bounds (see [Request Defaults](#request-defaults)).

| Bound | Default | Covers |
|-------|---------|--------|
| `connect` | 30 seconds | establishing the connection, TLS included |
| `idle` | 10 minutes | each wait for data: the response headers, then every gap between body chunks |

A request whose response headers do not arrive, a stream that stalls, and a complete
response whose body stalls all fail with `ErrorCategory::Timeout`, which is retryable; any
other transport failure is `ErrorCategory::Unavailable`. The error keeps the code of the step
that stalled:

| Stall | Code |
|-------|------|
| No response headers, or a complete response's body | the client's request code, such as `model.groq.request`, `model.openai_compat.request`, `model.openai_responses.request`, `model.openrouter.request_failed` or `model.openrouter.response_read_failed`, `model.ollama.timeout`, `model.bedrock.timeout` |
| A stream | the client's incomplete-stream code (`model.openai_compat.incomplete_stream` for the OpenAI-compatible clients), `model.openai_responses.stream`, `model.openrouter.chat_stream_read_failed`, `model.openrouter.responses_stream_read_failed`, `model.bedrock.stream_idle_timeout` |

A long stream that keeps sending data is never cut off, and a reasoning model that thinks for
minutes before its first chunk stays within the 10-minute default.

`with_timeouts` sets both bounds and returns `Result`, on `OpenAIClient` (Chat Completions and
its Responses route), `OpenAICompatible`, `AzureOpenAIClient`, `OpenAIResponsesClient`,
`AzureAIClient`, `GroqClient`, `DeepSeekClient`, `OpenRouterClient`, `OllamaModel`, `AnthropicClient`, and
`BedrockClient`. `BedrockClient` sets them as the AWS SDK's connect and read timeouts and
bounds each stream event by `idle`; an SDK client passed to `BedrockClient::from_client` keeps
its own connect and read timeouts until `with_timeouts` replaces them.

```rust
use std::time::Duration;

use adk_model::HttpTimeouts;
use adk_model::groq::{GroqClient, GroqConfig};

fn build(api_key: String) -> Result<GroqClient, adk_core::AdkError> {
    GroqClient::new(GroqConfig::new(api_key, "openai/gpt-oss-120b"))?
        .with_timeouts(HttpTimeouts::new(Duration::from_secs(10), Duration::from_secs(120)))
}
```

### Request Adapters and Explicit Endpoints

`OpenAICompatible` (and so every OpenAI-compatible preset) and `OpenAIResponsesClient`
accept a `RequestAdapter` that edits the JSON body and headers of each generation
request, retries included; `AnthropicClient` has its own
([Request adapter](./anthropic.md#request-adapter)). Constructors that take the endpoint
explicitly avoid ambient configuration when the application owns authentication:

| Constructor | Provider | Reads environment |
|-------------|----------|-------------------|
| `Anthropic::new_with_base_url` | Anthropic | No |
| `GeminiModel::new_with_base_url` | Gemini API | No |
| `GeminiModel::new_google_cloud_endpoint` | Vertex AI (`gemini-vertex`) | Only ADC when no key is given |
| `BedrockClient::new_with_api_key`, `BedrockClient::from_client` | Amazon Bedrock | No |

```rust
use std::sync::Arc;

use adk_model::openai::RequestAdapter;
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

fn build(api_key: String) -> Result<OpenAICompatible, adk_core::AdkError> {
    let adapter: RequestAdapter = Arc::new(|body, headers| {
        body["metadata"] = serde_json::json!({"application": "support-bot"});
        headers.insert("x-request-source", "support-bot".parse().expect("valid header"));
        Ok(())
    });
    Ok(OpenAICompatible::new(OpenAICompatibleConfig::new(api_key, "gpt-5.6-luna"))?
        .with_request_adapter(adapter))
}
```

---

## Switching Providers

All providers implement the same `Llm` trait, so an agent takes any of them as
`Arc<dyn Llm>`:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

fn model_for(provider: &str, api_key: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(match provider {
        "openai" => Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5.6-terra"))?),
        "anthropic" => Arc::new(AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5"))?),
        "deepseek" => Arc::new(DeepSeekClient::chat(api_key)?),
        "groq" => Arc::new(GroqClient::new(GroqConfig::gpt_oss_120b(api_key))?),
        _ => Arc::new(GeminiModel::new(api_key, "gemini-3.7-flash")?),
    })
}

fn build(provider: &str, api_key: &str) -> anyhow::Result<LlmAgent> {
    Ok(LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(model_for(provider, api_key)?)
        .build()?)
}
```

`adk_rust::provider_from_env()` returns the first compiled provider the environment
configures: Gemini on Vertex AI when `GOOGLE_GENAI_USE_ENTERPRISE` or
`GOOGLE_GENAI_USE_VERTEXAI` is `1` or `true` (needs `gemini-vertex`), then
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, and `GOOGLE_API_KEY`.

---

## Content Bedrock Cannot Carry

`Content` can express more than Bedrock Converse accepts, so the Bedrock adapter records the
fate of every part:

| Disposition | Meaning |
|-------------|---------|
| `Converted` | Carried to the provider in an equivalent native form |
| `Downgraded` | Carried in a lossier form — a file reference rendered as descriptive text the model can read but not fetch |
| `Omitted` | Not carried at all |

Downgrades and omissions emit a `tracing` warning as they are recorded, naming the part
kind, MIME type, and reason. `report_for_contents` returns the outcome without sending a
request, and `ConversionReport::into_error` turns omissions into an error, so a request that
would reach the model incomplete can be refused:

```rust
use adk_core::{AdkError, Content, Part};
use adk_model::bedrock::convert::report_for_contents;

fn check(content: &Content) -> Result<(), AdkError> {
    let report = report_for_contents(std::slice::from_ref(content));
    for omission in report.omitted_parts() {
        tracing::warn!(part.kind = omission.kind, reason = %omission.detail, "part dropped");
    }
    match report.into_error() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn main() {
    let content = Content {
        role: "user".to_string(),
        parts: vec![Part::inline_data("video/mp4", vec![0u8; 16])],
    };
    assert!(check(&content).is_err());
}
```

`into_error` covers omissions only. A downgrade still reaches the model, and rejecting it
would refuse the documented textual fallback. A part that leaves the adapter without a
recorded fate is recorded as an omission ("dropped by the Bedrock adapter without a recorded
reason"), and `adk-model/tests/part_conversion_matrix_tests.rs` fails on it.

| Part | Disposition |
|------|-------------|
| Text, FunctionCall, FunctionResponse, Thinking | `Converted` |
| `InlineData` with JPEG, PNG, GIF, WebP | `Converted` as an image block |
| `InlineData` with a supported document type (PDF and similar) | `Converted` as a document block |
| `InlineData` with audio, video, or arbitrary binary | `Omitted` |
| `FileData` for an image or supported document | `Downgraded` to text — Converse takes S3 URIs, not arbitrary URLs |
| `FunctionResponse` inline data with an image or supported document type | `Converted` inside the tool result |
| `FunctionResponse` inline data of any other type | `Omitted`, recorded beside the converted response |
| `FileData` for any other type | `Omitted` |
| `ServerToolCall`, `ServerToolResponse` | `Omitted` — Gemini-specific |
| `EmbeddedResource` text, or a blob of a supported type | `Converted` |
| `EmbeddedResource` blob of an unsupported type | `Omitted` |

---

## Project Templates

`cargo adk new` generates a project for a provider:

```bash
cargo adk new gemini_agent --provider gemini
cargo adk new openai_agent --template openai
cargo adk new anthropic_agent --provider anthropic
```

`scripts/check-cargo-adk-templates.sh` compiles the generated projects in CI. More examples
are in the [adk-playground](https://github.com/zavora-ai/adk-playground) repository.

## Related

- [OpenAI Responses API](./openai-responses.md) - The dedicated Responses client
- [Anthropic](./anthropic.md) - The `adk-anthropic` client
- [Gemini Interactions API](./gemini-interactions.md) - Stateful Gemini transport
- [OpenCode Go and Zen](./opencode.md) - Per-model routing for OpenCode services
- [Ollama (Local)](./ollama.md) - Run models locally with Ollama
- [Local Models (mistral.rs)](./mistralrs.md) - Native Rust inference
- [LlmAgent](../agents/llm-agent.md) - Using models with agents
- [Function Tools](../tools/function-tools.md) - Adding tools to agents

---

**Previous**: [← Realtime Agents](../agents/realtime-agents.md) | **Next**: [Ollama (Local) →](./ollama.md)
