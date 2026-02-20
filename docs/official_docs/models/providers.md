# Model Providers (Cloud)

ADK-Rust supports multiple cloud LLM providers through the `adk-model` crate. All providers implement the `Llm` trait, making them interchangeable in your agents.

## Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Cloud Model Providers                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   • Gemini (Google)    ⭐ Default    - Multimodal, large context    │
│   • OpenAI (GPT-5)    🔥 Popular    - Best ecosystem               │
│   • Anthropic (Claude) 🧠 Smart      - Best reasoning               │
│   • DeepSeek           💭 Thinking   - Chain-of-thought, cheap      │
│   • Groq               ⚡ Ultra-Fast  - Fastest inference           │
│                                                                     │
│   For local/offline models, see:                                    │
│   • Ollama     → ollama.md                                          │
│   • mistral.rs → mistralrs.md                                       │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Quick Comparison

| Provider | Best For | Speed | Cost | Key Feature |
|----------|----------|-------|------|-------------|
| **Gemini** | General use | ⚡⚡⚡ | 💰 | Multimodal, large context |
| **OpenAI** | Reliability | ⚡⚡ | 💰💰 | Best ecosystem |
| **Anthropic** | Complex reasoning | ⚡⚡ | 💰💰 | Safest, most thoughtful |
| **DeepSeek** | Chain-of-thought | ⚡⚡ | 💰 | Thinking mode, cheap |
| **Groq** | Speed-critical | ⚡⚡⚡⚡ | 💰 | Fastest inference |

---

## Step 1: Installation

Add the providers you need to your `Cargo.toml`:

```toml
[dependencies]
# Pick one or more providers:
adk-model = { version = "0.3.2", features = ["gemini"] }        # Google Gemini (default)
adk-model = { version = "0.3.2", features = ["openai"] }        # OpenAI GPT-5
adk-model = { version = "0.3.2", features = ["anthropic"] }     # Anthropic Claude
adk-model = { version = "0.3.2", features = ["deepseek"] }      # DeepSeek
adk-model = { version = "0.3.2", features = ["groq"] }          # Groq (ultra-fast)

# Or all cloud providers at once:
adk-model = { version = "0.3.2", features = ["all-providers"] }
```

## Step 2: Set Your API Key

```bash
export GOOGLE_API_KEY="your-key"      # Gemini
export OPENAI_API_KEY="your-key"      # OpenAI
export ANTHROPIC_API_KEY="your-key"   # Anthropic
export DEEPSEEK_API_KEY="your-key"    # DeepSeek
export GROQ_API_KEY="your-key"        # Groq
```

---

## Gemini (Google) ⭐ Default

> **Best for**: General purpose, multimodal tasks, large documents
> 
> **Key highlights**:
> - 🖼️ Native multimodal (images, video, audio, PDF)
> - 📚 Up to 2M token context window
> - 💰 Competitive pricing
> - ⚡ Fast inference

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("gemini_assistant")
        .description("Gemini-powered assistant")
        .instruction("You are a helpful assistant powered by Google Gemini. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Description | Context |
|-------|-------------|---------|
| `gemini-3-pro` | Most intelligent, complex agentic workflows | 2M tokens |
| `gemini-3-flash` | Fast and efficient for most tasks | 1M tokens |
| `gemini-2.5-pro` | Advanced reasoning and multimodal | 1M tokens |
| `gemini-2.5-flash` | Balanced speed and capability (recommended) | 1M tokens |
| `gemini-2.5-flash-lite` | Ultra-fast for high-volume | 1M tokens |
| `gemini-2.0-flash` | Previous generation (retiring March 2026) | 1M tokens |

### Example Output

```
👤 User: What's in this image? [uploads photo of a cat]

🤖 Gemini: I can see a fluffy orange tabby cat sitting on a windowsill. 
The cat appears to be looking outside, with sunlight illuminating its fur. 
It has green eyes and distinctive striped markings typical of tabby cats.
```

---

## OpenAI (GPT-5) 🔥 Popular

> **Best for**: Production apps, reliable performance, broad capabilities
> 
> **Key highlights**:
> - 🏆 Industry standard
> - 🔧 Excellent tool/function calling
> - 📖 Best documentation & ecosystem
> - 🎯 Consistent, predictable outputs
> - 📋 **Structured output** with JSON schema enforcement

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5-mini"))?;

    let agent = LlmAgentBuilder::new("openai_assistant")
        .description("OpenAI-powered assistant")
        .instruction("You are a helpful assistant powered by OpenAI GPT-5. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Structured Output (JSON Schema)

OpenAI supports guaranteed JSON output via `output_schema`. ADK-Rust automatically wires this to OpenAI's `response_format` API:

```rust
use adk_rust::prelude::*;
use serde_json::json;
use std::sync::Arc;

let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5-mini"))?;

let agent = LlmAgentBuilder::new("data_extractor")
    .model(Arc::new(model))
    .instruction("Extract person information from the text.")
    .output_schema(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "number" },
            "email": { "type": "string" }
        },
        "required": ["name", "age"]
    }))
    .build()?;

// Response is guaranteed to be valid JSON matching the schema
```

For strict mode with nested objects, include `additionalProperties: false` at each level:

```rust
.output_schema(json!({
    "type": "object",
    "properties": {
        "title": { "type": "string" },
        "metadata": {
            "type": "object",
            "properties": {
                "author": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["author"],
            "additionalProperties": false  // Required for nested objects
        }
    },
    "required": ["title", "metadata"],
    "additionalProperties": false  // Auto-injected at root level
}))
```

### OpenAI-Compatible Local APIs

Use `OpenAIConfig::compatible()` to connect to local servers (Ollama, vLLM, LM Studio):

```rust
// Ollama exposes OpenAI-compatible API at /v1
let config = OpenAIConfig::compatible(
    "not-needed",                      // API key (ignored by Ollama)
    "http://localhost:11434/v1",       // Base URL
    "llama3.2"                         // Model name
);
let model = OpenAIClient::new(config)?;
```

> **Note**: Structured output (`output_schema`) requires backend support. Native OpenAI fully supports it; local servers may have limited support.

### Available Models

| Model | Description | Context |
|-------|-------------|---------|
| `gpt-5.1` | Latest iteration with improved performance | 256K tokens |
| `gpt-5` | State-of-the-art unified model with adaptive thinking | 256K tokens |
| `gpt-5-mini` | Efficient version for most tasks (recommended) | 128K tokens |
| `gpt-4o` | Multimodal model (deprecated August 2025) | 128K tokens |
| `gpt-4o-mini` | Fast and affordable (deprecated August 2025) | 128K tokens |

### Example Output

```
👤 User: Write a haiku about Rust programming

🤖 GPT-5: Memory so safe,
Ownership guards every byte—
Compiler, my friend.
```

---

## Anthropic (Claude) 🧠 Smart

> **Best for**: Complex reasoning, safety-critical apps, long documents
> 
> **Key highlights**:
> - 🧠 Exceptional reasoning ability
> - 🛡️ Most safety-focused
> - 📚 200K token context
> - ✍️ Excellent writing quality

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-4-5-20250929"))?;

    let agent = LlmAgentBuilder::new("anthropic_assistant")
        .description("Anthropic-powered assistant")
        .instruction("You are a helpful assistant powered by Anthropic Claude. Be concise and thoughtful.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Description | Context |
|-------|-------------|---------|
| `claude-opus-4-5-20251101` | Most capable for complex autonomous tasks | 200K tokens |
| `claude-sonnet-4-5-20250929` | Balanced intelligence and cost (recommended) | 1M tokens |
| `claude-haiku-4-5-20251001` | Ultra-efficient for high-volume workloads | 200K tokens |
| `claude-opus-4-20250514` | Hybrid model with extended thinking | 200K tokens |
| `claude-sonnet-4-20250514` | Balanced model with extended thinking | 1M tokens |

### Example Output

```
👤 User: Explain quantum entanglement to a 10-year-old

🤖 Claude: Imagine you have two magic coins. When you flip them, they always 
land the same way - both heads or both tails - even if one coin is on Earth 
and the other is on the Moon! Scientists call this "entanglement." The coins 
are connected in a special way that we can't see, like invisible best friends 
who always make the same choice at the exact same time.
```

---

## DeepSeek 💭 Thinking

> **Best for**: Complex problem-solving, math, coding, reasoning tasks
> 
> **Key highlights**:
> - 💭 **Thinking mode** - shows chain-of-thought reasoning
> - 💰 Very cost-effective (10x cheaper than GPT-4)
> - 🔄 Context caching for repeated prefixes
> - 🧮 Strong at math and coding

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    
    // Standard chat model
    let model = DeepSeekClient::chat(&api_key)?;
    
    // OR: Reasoning model with thinking mode
    // let model = DeepSeekClient::reasoner(&api_key)?;

    let agent = LlmAgentBuilder::new("deepseek_assistant")
        .description("DeepSeek-powered assistant")
        .instruction("You are a helpful assistant powered by DeepSeek. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Description | Special Feature |
|-------|-------------|-----------------|
| `deepseek-r1-0528` | Latest reasoning model | Enhanced thinking depth |
| `deepseek-r1` | Advanced reasoning | Comparable to o1 |
| `deepseek-v3.1` | Latest 671B MoE model | General tasks |
| `deepseek-chat` | 671B MoE model (V3) | General purpose, cheap |
| `deepseek-vl2` | Vision-language model | Multimodal |

### Example Output (Reasoner with Thinking Mode)

```
👤 User: What's 17 × 23?

🤖 DeepSeek: <thinking>
Let me break this down:
17 × 23 = 17 × (20 + 3)
       = 17 × 20 + 17 × 3
       = 340 + 51
       = 391
</thinking>

The answer is 391.
```

---

## Groq ⚡ Ultra-Fast

> **Best for**: Real-time applications, chatbots, speed-critical tasks
> 
> **Key highlights**:
> - ⚡ **Fastest inference** - 10x faster than competitors
> - 🔧 LPU (Language Processing Unit) technology
> - 💰 Competitive pricing
> - 🦙 Runs LLaMA, Mixtral, Gemma models

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GROQ_API_KEY")?;
    let model = GroqClient::llama70b(&api_key)?;

    let agent = LlmAgentBuilder::new("groq_assistant")
        .description("Groq-powered assistant")
        .instruction("You are a helpful assistant powered by Groq. Be concise and fast.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Method | Description |
|-------|--------|-------------|
| `llama-4-scout` | `GroqClient::new(GroqConfig::new(key, "llama-4-scout"))` | Llama 4 Scout (17Bx16E) |
| `llama-3.2-90b-text-preview` | `GroqClient::new(GroqConfig::new(key, "llama-3.2-90b-text-preview"))` | Large text model |
| `llama-3.1-70b-versatile` | `GroqClient::llama70b()` | Versatile large model |
| `llama-3.1-8b-instant` | `GroqClient::llama8b()` | Fastest |
| `mixtral-8x7b-32768` | `GroqClient::mixtral()` | Good balance |
| Any model | `GroqClient::new(GroqConfig::new(key, "model"))` | Custom model |

### Example Output

```
👤 User: Quick! Name 5 programming languages

🤖 Groq (in 0.2 seconds): 
1. Rust
2. Python
3. JavaScript
4. Go
5. TypeScript
```

---

## Switching Providers

All providers implement the same `Llm` trait, so switching is easy:

```rust
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

// Just change the model - everything else stays the same!
let model: Arc<dyn adk_core::Llm> = Arc::new(
    // Pick one:
    // GeminiModel::new(&api_key, "gemini-2.5-flash")?
    // OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5-mini"))?
    // AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-4-5-20250929"))?
    // DeepSeekClient::chat(&api_key)?
    // GroqClient::llama70b(&api_key)?
);

let agent = LlmAgentBuilder::new("assistant")
    .instruction("You are a helpful assistant.")
    .model(model)
    .build()?;
```

---

## Examples

```bash
# Gemini (default)
cargo run --example quickstart

# OpenAI
cargo run --example openai_basic --features openai

# Anthropic
cargo run --example anthropic_basic --features anthropic

# DeepSeek
cargo run --example deepseek_basic --features deepseek
cargo run --example deepseek_reasoner --features deepseek  # Thinking mode

# Groq
cargo run --example groq_basic --features groq
```

---

## Related

- [Ollama (Local)](./ollama.md) - Run models locally with Ollama
- [Local Models (mistral.rs)](./mistralrs.md) - Native Rust inference
- [LlmAgent](../agents/llm-agent.md) - Using models with agents
- [Function Tools](../tools/function-tools.md) - Adding tools to agents

---

**Previous**: [← Realtime Agents](../agents/realtime-agents.md) | **Next**: [Ollama (Local) →](./ollama.md)
