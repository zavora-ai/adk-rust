# Ollama (Local Models)

Run LLMs locally with complete privacy - no API keys, no internet, no costs.

## Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Ollama Local Setup                          │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   Your Machine                                                      │
│   ┌─────────────────────────────────────────────────────────────┐  │
│   │                                                             │  │
│   │   ┌──────────────┐      ┌──────────────┐                   │  │
│   │   │  ADK-Rust    │ ───▶ │   Ollama     │                   │  │
│   │   │  Agent       │      │   Server     │                   │  │
│   │   └──────────────┘      └──────┬───────┘                   │  │
│   │                                │                            │  │
│   │                         ┌──────▼───────┐                   │  │
│   │                         │  Local LLM   │                   │  │
│   │                         │  (llama3.2)  │                   │  │
│   │                         └──────────────┘                   │  │
│   │                                                             │  │
│   └─────────────────────────────────────────────────────────────┘  │
│                                                                     │
│   🔒 100% Private - Data never leaves your machine                 │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Why Ollama?

| Benefit | Description |
|---------|-------------|
| 🆓 **Free** | No API costs, ever |
| 🔒 **Private** | Data stays on your machine |
| 📴 **Offline** | Works without internet |
| 🎛️ **Control** | Choose any model, customize settings |
| ⚡ **Fast** | No network latency |

---

## Step 1: Install Ollama

### macOS
```bash
brew install ollama
```

### Linux
```bash
curl -fsSL https://ollama.com/install.sh | sh
```

### Windows
Download from [ollama.com](https://ollama.com)

---

## Step 2: Start the Server

```bash
ollama serve
```

You should see:
```
Couldn't find '/Users/you/.ollama/id_ed25519'. Generating new private key.
Your new public key is: ssh-ed25519 AAAA...
time=2024-01-05T12:00:00.000Z level=INFO source=server.go msg="Listening on 127.0.0.1:11434"
```

---

## Step 3: Pull a Model

In a new terminal:

```bash
# Recommended starter model (3B parameters, fast)
ollama pull llama3.2

# Other popular models
ollama pull qwen2.5:7b    # Excellent tool calling
ollama pull mistral       # Good for code
ollama pull codellama     # Code generation
ollama pull gemma2        # Google's efficient model
```

---

## Step 4: Add to Your Project

```toml
[dependencies]
adk-model = { version = "0.2", features = ["ollama"] }
```

---

## Step 5: Use in Code

```rust
use adk_model::ollama::{OllamaModel, OllamaConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No API key needed!
    let model = OllamaModel::new(OllamaConfig::new("llama3.2"))?;

    let agent = LlmAgentBuilder::new("local_assistant")
        .instruction("You are a helpful assistant running locally.")
        .model(Arc::new(model))
        .build()?;

    // Use the agent...
    Ok(())
}
```

---

## Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    // No API key needed!
    let model = OllamaModel::new(OllamaConfig::new("llama3.2"))?;

    let agent = LlmAgentBuilder::new("ollama_assistant")
        .description("Ollama-powered local assistant")
        .instruction("You are a helpful assistant running locally via Ollama. Be concise.")
        .model(Arc::new(model))
        .build()?;

    // Run interactive session
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
```

### Cargo.toml

```toml
[dependencies]
adk-rust = { version = "0.2", features = ["cli", "ollama"] }
tokio = { version = "1", features = ["full"] }
dotenvy = "0.15"
anyhow = "1.0"
```

---

## Configuration Options

```rust
use adk_model::ollama::{OllamaModel, OllamaConfig};

let config = OllamaConfig::new("llama3.2")
    .with_base_url("http://localhost:11434")  // Custom server URL
    .with_temperature(0.7)                     // Creativity (0.0-1.0)
    .with_max_tokens(2048);                    // Max response length

let model = OllamaModel::new(config)?;
```

---

## Recommended Models

| Model | Size | RAM Needed | Best For |
|-------|------|------------|----------|
| `llama3.2` | 3B | 4GB | Fast, general purpose |
| `llama3.2:7b` | 7B | 8GB | Better quality |
| `qwen2.5:7b` | 7B | 8GB | **Best tool calling** |
| `mistral` | 7B | 8GB | Code and reasoning |
| `codellama` | 7B | 8GB | Code generation |
| `gemma2` | 9B | 10GB | Balanced performance |
| `llama3.1:70b` | 70B | 48GB | Highest quality |

### Choosing a Model

- **Limited RAM (8GB)?** → `llama3.2` (3B)
- **Need tool calling?** → `qwen2.5:7b`
- **Writing code?** → `codellama` or `mistral`
- **Best quality?** → `llama3.1:70b` (needs 48GB+ RAM)

---

## Tool Calling with Ollama

Ollama supports function calling with compatible models:

```rust
use adk_model::ollama::{OllamaModel, OllamaConfig};
use adk_agent::LlmAgentBuilder;
use adk_tool::FunctionTool;
use std::sync::Arc;

// qwen2.5 has excellent tool calling support
let model = OllamaModel::new(OllamaConfig::new("qwen2.5:7b"))?;

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
));

let agent = LlmAgentBuilder::new("weather_assistant")
    .instruction("Help users check the weather.")
    .model(Arc::new(model))
    .tool(weather_tool)
    .build()?;
```

> **Note**: Tool calling uses non-streaming mode for reliability with local models.

---

## Example Output

```
👤 User: Hello! What can you do?

🤖 Ollama (llama3.2): Hello! I'm a local AI assistant running on your 
machine. I can help with:
- Answering questions
- Writing and editing text
- Explaining concepts
- Basic coding help

All completely private - nothing leaves your computer!
```

---

## Troubleshooting

### "Connection refused"
```bash
# Make sure Ollama is running
ollama serve
```

### "Model not found"
```bash
# Pull the model first
ollama pull llama3.2
```

### Slow responses
- Use a smaller model (`llama3.2` instead of `llama3.1:70b`)
- Close other applications to free RAM
- Consider GPU acceleration if available

### Check available models
```bash
ollama list
```

---

## Running Examples

```bash
# From the official_docs_examples folder
cd official_docs_examples/models/providers_test
cargo run --bin ollama_example
```

---

## Related

- [Model Providers](./providers.md) - Cloud-based LLM providers
- [Local Models (mistral.rs)](./mistralrs.md) - Native Rust inference

---

**Previous**: [← Model Providers](./providers.md) | **Next**: [Local Models (mistral.rs) →](./mistralrs.md)
