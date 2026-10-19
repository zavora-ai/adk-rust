# Ollama (Local Models)

`OllamaModel` runs agents on models served by a local [Ollama](https://ollama.com) server. It
needs no API key, and local models cost zero in the [pricing catalog](./providers.md#usage-attribution-and-cost).

| Item | Value |
|------|-------|
| Client | `OllamaModel`, configured with `OllamaConfig` |
| Feature | `ollama` on `adk-model` or `adk-rust` |
| Server | `http://localhost:11434` by default |
| Endpoint | `POST /api/chat`, streamed or not |
| Catalog default | `qwen3.5` (`adk_model::catalog::OLLAMA_DEFAULT`) |

---

## Step 1: Install Ollama

| Platform | Install |
|----------|---------|
| macOS | `brew install ollama` |
| Linux | `curl -fsSL https://ollama.com/install.sh \| sh` |
| Windows | Download from [ollama.com](https://ollama.com) |

## Step 2: Start the Server

```bash
ollama serve
```

The server listens on `127.0.0.1:11434`.

## Step 3: Pull a Model

```bash
ollama pull qwen3.5
```

| Model tag | Capabilities in the Ollama library |
|-----------|------------------------------------|
| `qwen3.5` | Tools, thinking, vision |
| `qwen3.6:35b-a3b` | Tools, thinking, vision |
| `qwen3-coder:30b` | Tools |
| `gemma4` | Tools, thinking, vision, audio |
| `llama3.2:3b` | Tools |
| `llama3.1:8b` | Tools |
| `llama3.3:70b` | Tools |
| `qwen2.5:7b` | Tools |
| `mistral:7b` | Tools |
| `mistral-nemo:12b` | Tools |
| `devstral:24b` | Tools |
| `deepseek-r1:14b`, `deepseek-r1:32b` | Tools, thinking |
| `gemma3:12b` | Vision |
| `codellama:13b` | — |

An agent with tools needs a model whose library page lists tools. Thinking output arrives as
`Part::Thinking`.

## Step 4: Add to Your Project

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["ollama"] }
anyhow = "1"
tokio = { version = "1", features = ["full"] }
# For the tool-calling sample:
adk-tool = "3.0.0"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

## Step 5: Run an Agent

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No API key needed.
    let model = OllamaModel::new(OllamaConfig::new("qwen3.5"))?;

    let agent = LlmAgentBuilder::new("ollama_assistant")
        .description("Ollama-powered local assistant")
        .instruction("You are a helpful assistant running locally via Ollama. Be concise.")
        .model(Arc::new(model))
        .build()?;

    // Reads each message from stdin and streams the answer; `exit` quits.
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

---

## Configuration

`OllamaConfig` has public fields; `OllamaConfig::with_host(host, model)` sets the server URL:

```rust
use adk_model::ollama::{OllamaConfig, OllamaModel};

fn build() -> adk_core::Result<OllamaModel> {
    let mut config = OllamaConfig::with_host("http://gpu-box:11434", "qwen3.5");
    config.temperature = Some(0.7);
    config.num_ctx = Some(32_768); // context window
    OllamaModel::new(config)
}
```

| Field | Default | Sent as |
|-------|---------|---------|
| `host` | `http://localhost:11434` | — |
| `model` | `qwen3.5` | `model` |
| `num_ctx` | Model default | `options.num_ctx` |
| `temperature`, `top_p`, `top_k` | Model default | `options` |

A request's `GenerateContentConfig` overrides `temperature`, `top_p`, and `top_k`, and sends
`stop_sequences`, `seed`, and both penalties in `options`, `top_logprobs` as `logprobs` and
`top_logprobs` (tokens in `provider_metadata["logprobs"]`), and `response_schema` as
`format`. `max_output_tokens` is not sent.
`OllamaModel::with_timeouts` sets the connect and idle timeouts (30 seconds and 10 minutes by
default); a model that takes longer than the idle timeout to load needs a longer one. A stall
fails with `ErrorCategory::Timeout`: `model.ollama.timeout` before the response headers,
`model.ollama.incomplete_stream` mid-stream.

---

## Tool Calling

```rust
use adk_rust::prelude::*;
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize, JsonSchema)]
struct WeatherArgs {
    location: String,
}

fn build() -> anyhow::Result<LlmAgent> {
    let model = OllamaModel::new(OllamaConfig::new("qwen3.5"))?;

    let weather_tool = Arc::new(FunctionTool::new(
        "get_weather",
        "Get weather for a location",
        |_ctx, args| async move {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(serde_json::json!({
                "location": location,
                "temperature": "72°F",
                "condition": "Sunny"
            }))
        },
    )
    .with_parameters_schema::<WeatherArgs>());

    Ok(LlmAgentBuilder::new("weather_assistant")
        .instruction("Help users check the weather.")
        .model(Arc::new(model))
        .tool(weather_tool)
        .build()?)
}
```

| Behaviour | Rule |
|-----------|------|
| Streaming | Tool calls stream as partial responses, so a streamed tool-call turn is saved as one event |
| Calls without IDs | Ollama sends calls without IDs; the agent assigns each an `adk-call-` ID and leaves it out of the next request |
| Calls written as text | Tool-call markup a model writes into its text (the Qwen/Hermes, Llama, Mistral Nemo, DeepSeek, DeepSeek-V3, Gemma 4, and action-tag formats) becomes a call when it names a declared tool, streamed or not |
| Tool images | Images a tool returns are sent in a user message after the tool messages |

---

## Usage

The final `/api/chat` line's counts become `UsageMetadata`. Ollama leaves out a metric that is
zero, such as `prompt_eval_count` when the whole prompt came from its cache; a missing count
reads as zero, `eval_count` is still reported, and `UsageMetadata::provider_usage` holds the
metric fields the line carried.

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| Connection refused | Start the server with `ollama serve` |
| Model not found | Pull the model first: `ollama pull qwen3.5`; `ollama list` shows the pulled models |
| Slow responses | Use a smaller model, or a machine with a GPU |
| `stream ended before the response finished` | See below |
| `reported an error during generation` | See below |

A streamed response is complete only when Ollama sends a line with `"done": true`. A stream
that ends without one fails with `model.ollama.incomplete_stream` (retryable), and an
`{"error": "..."}` line fails with `model.ollama.stream_error`, quoting Ollama's message.
Both usually mean the model runner stopped: check the `ollama serve` log for an
out-of-memory or crashed runner. See
[Stream Termination](providers.md#stream-termination) for every provider's rules.

---

## Example

`examples/ollama_qwen` checks thinking traces and tool calling on a local Qwen model (`OLLAMA_MODEL`, default `qwen3.5`):

```bash
ollama pull qwen3.5
cargo run --manifest-path examples/ollama_qwen/Cargo.toml
```

---

## Related

- [Model Providers](./providers.md) - Cloud LLM providers
- [Local Models (mistral.rs)](./mistralrs.md) - Native Rust inference

---

**Previous**: [← Model Providers](./providers.md) | **Next**: [Local Models (mistral.rs) →](./mistralrs.md)
