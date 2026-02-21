# adk-model

LLM model integrations for Rust Agent Development Kit (ADK-Rust) with Gemini, OpenAI, xAI, Anthropic, DeepSeek, Groq, Ollama, Fireworks AI, Together AI, Mistral AI, Perplexity, Cerebras, SambaNova, Amazon Bedrock, and Azure AI Inference.

[![Crates.io](https://img.shields.io/crates/v/adk-model.svg)](https://crates.io/crates/adk-model)
[![Documentation](https://docs.rs/adk-model/badge.svg)](https://docs.rs/adk-model)
[![License](https://img.shields.io/crates/l/adk-model.svg)](LICENSE)

## Overview

`adk-model` provides LLM integrations for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)). Supports all major providers:

- **Gemini** - Google's Gemini models (3 Pro, 3 Flash, 2.5 Pro, 2.5 Flash, etc.)
- **OpenAI** - GPT-5.1, GPT-5, GPT-5 Mini, GPT-4o (legacy)
- **xAI** - Grok models through the OpenAI-compatible API
- **Anthropic** - Claude Opus 4.5, Claude Sonnet 4.5, Claude Haiku 4.5, Claude 4
- **DeepSeek** - DeepSeek R1, DeepSeek V3.1, DeepSeek-Chat with thinking mode
- **Groq** - Ultra-fast inference (LLaMA 3.3, Mixtral, Gemma)
- **Ollama** - Local LLMs (LLaMA, Mistral, Qwen, Gemma, etc.)
- **Fireworks AI** - Fast open-model inference (Llama, Mixtral, etc.)
- **Together AI** - Hosted open models (Llama, CodeLlama, etc.)
- **Mistral AI** - Mistral cloud models (Mistral Small, Large, etc.)
- **Perplexity** - Search-augmented LLM (Sonar, etc.)
- **Cerebras** - Ultra-fast inference (Llama 3.3, etc.)
- **SambaNova** - Fast inference (Llama 3.3, etc.)
- **Amazon Bedrock** - AWS-hosted models via IAM auth (Claude, Llama, Mistral, etc.)
- **Azure AI Inference** - Azure-hosted models (Cohere, Llama, Mistral, etc.)
- **Streaming** - Real-time response streaming for all providers
- **Multimodal** - Text, images, audio, video, and PDF input

The crate implements the `Llm` trait from `adk-core`, allowing models to be used interchangeably.

## Installation

```toml
[dependencies]
adk-model = "0.3.2"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.2", features = ["models"] }
```

## Quick Start

### Gemini (Google)

```rust
use adk_model::GeminiModel;
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.1-pro-preview")?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### OpenAI

```rust
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Anthropic (Claude)

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-5-20250929"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

#### Anthropic Advanced Features

```rust
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

// Extended thinking with token budget
let config = AnthropicConfig::new(api_key, "claude-sonnet-4-5-20250929")
    .with_thinking(8192)
    .with_prompt_caching(true)
    .with_beta_feature("prompt-caching-2024-07-31");
let client = AnthropicClient::new(config)?;

// Token counting
let count = client.count_tokens(&request).await?;

// Model discovery
let models = client.list_models().await?;
let info = client.get_model("claude-sonnet-4-5-20250929").await?;

// Rate limit inspection
let rate_info = client.latest_rate_limit_info().await;
```

### DeepSeek

```rust
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;

    // Standard chat model
    let model = DeepSeekClient::chat(api_key)?;

    // Or use the reasoner model with chain-of-thought
    // let model = DeepSeekClient::reasoner(api_key)?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Groq (Ultra-Fast)

```rust
use adk_model::groq::{GroqClient, GroqConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GROQ_API_KEY")?;
    let model = GroqClient::new(GroqConfig::llama70b(api_key))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Ollama (Local)

```rust
use adk_model::ollama::{OllamaModel, OllamaConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Requires: ollama serve && ollama pull llama3.2
    let model = OllamaModel::new(OllamaConfig::new("llama3.2"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Fireworks AI

```rust
use adk_model::fireworks::{FireworksClient, FireworksConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("FIREWORKS_API_KEY")?;
    let model = FireworksClient::new(FireworksConfig::new(
        api_key, "accounts/fireworks/models/llama-v3p1-8b-instruct",
    ))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Together AI

```rust
use adk_model::together::{TogetherClient, TogetherConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("TOGETHER_API_KEY")?;
    let model = TogetherClient::new(TogetherConfig::new(
        api_key, "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    ))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Mistral AI

```rust
use adk_model::mistral::{MistralClient, MistralConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("MISTRAL_API_KEY")?;
    let model = MistralClient::new(MistralConfig::new(api_key, "mistral-small-latest"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Perplexity

```rust
use adk_model::perplexity::{PerplexityClient, PerplexityConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("PERPLEXITY_API_KEY")?;
    let model = PerplexityClient::new(PerplexityConfig::new(api_key, "sonar"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Cerebras

```rust
use adk_model::cerebras::{CerebrasClient, CerebrasConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("CEREBRAS_API_KEY")?;
    let model = CerebrasClient::new(CerebrasConfig::new(api_key, "llama-3.3-70b"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### SambaNova

```rust
use adk_model::sambanova::{SambaNovaClient, SambaNovaConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("SAMBANOVA_API_KEY")?;
    let model = SambaNovaClient::new(SambaNovaConfig::new(api_key, "Meta-Llama-3.3-70B-Instruct"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Amazon Bedrock

```rust
use adk_model::bedrock::{BedrockClient, BedrockConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Uses AWS IAM credentials from the environment (no API key needed)
    let config = BedrockConfig::new("us-east-1", "anthropic.claude-sonnet-4-20250514-v1:0");
    let model = BedrockClient::new(config).await?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Azure AI Inference

```rust
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("AZURE_AI_API_KEY")?;
    let config = AzureAIConfig::new(
        "https://my-endpoint.eastus.inference.ai.azure.com",
        api_key,
        "meta-llama-3.1-8b-instruct",
    );
    let model = AzureAIClient::new(config)?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

## Supported Models

### Google Gemini

| Model | Description |
|-------|-------------|
| `gemini-3.1-pro` | Most intelligent AI model, enhancing reasoning and multimodal capabilities. (1M context) |
| `gemini-3-pro` | Intelligent model for complex agentic workflows (1M context) |
| `gemini-3-flash` | Fast and efficient for most tasks (1M context) |
| `gemini-2.5-pro` | Advanced reasoning and multimodal understanding |
| `gemini-2.5-flash` | Balanced speed and capability (recommended) |
| `gemini-2.5-flash-lite` | Ultra-fast for high-volume tasks |
| `gemini-2.0-flash` | Previous generation (retiring March 2026) |

See [Gemini models documentation](https://ai.google.dev/gemini-api/docs/models/gemini) for the full list.

### OpenAI

| Model | Description |
|-------|-------------|
| `gpt-5.1` | Latest iteration with improved performance (256K context) |
| `gpt-5` | State-of-the-art unified model with adaptive thinking |
| `gpt-5-mini` | Efficient version for most tasks (128K context) |
| `gpt-4o` | Multimodal model (deprecated August 2025) |
| `gpt-4o-mini` | Fast and affordable (deprecated August 2025) |

See [OpenAI models documentation](https://platform.openai.com/docs/models) for the full list.

### Anthropic Claude

| Model | Description |
|-------|-------------|
| `claude-opus-4-5-20251101` | Most capable model for complex autonomous tasks (200K context) |
| `claude-sonnet-4-5-20250929` | Balanced intelligence and cost for production (1M context) |
| `claude-haiku-4-5-20251001` | Ultra-efficient for high-volume workloads |
| `claude-opus-4-20250514` | Hybrid model with extended thinking |
| `claude-sonnet-4-20250514` | Balanced model with extended thinking |

See [Anthropic models documentation](https://docs.anthropic.com/claude/docs/models-overview) for the full list.

### DeepSeek

| Model | Description |
|-------|-------------|
| `deepseek-r1-0528` | Latest reasoning model with enhanced thinking depth (128K context) |
| `deepseek-r1` | Advanced reasoning comparable to o1 |
| `deepseek-v3.1` | Latest 671B MoE model for general tasks |
| `deepseek-chat` | 671B MoE model, excellent for code (V3) |
| `deepseek-vl2` | Vision-language model (32K context) |

**Features:**
- **Thinking Mode** - Chain-of-thought reasoning with `<thinking>` tags
- **Context Caching** - Automatic KV cache for repeated prefixes (10x cost reduction)
- **Tool Calling** - Full function calling support

See [DeepSeek API documentation](https://api-docs.deepseek.com/) for the full list.

### Groq

| Model | Description |
|-------|-------------|
| `llama-4-scout` | Llama 4 Scout (17Bx16E) - Fast via Groq LPU (128K context) |
| `llama-3.2-90b-text-preview` | Large text model |
| `llama-3.2-11b-text-preview` | Balanced text model |
| `llama-3.1-70b-versatile` | Versatile large model |
| `llama-3.1-8b-instant` | Ultra-fast instruction model |
| `mixtral-8x7b-32768` | MoE model with 32K context |

**Features:**
- **Ultra-Fast** - LPU-based inference (fastest in the industry)
- **Tool Calling** - Full function calling support
- **Large Context** - Up to 128K tokens

See [Groq documentation](https://console.groq.com/docs/models) for the full list.

### Ollama (Local)

| Model | Description |
|-------|-------------|
| `llama3.3:70b` | Llama 3.3 70B - Latest for local deployment (128K context) |
| `llama3.2:3b` | Efficient small model |
| `llama3.1:8b` | Popular balanced model |
| `deepseek-r1:14b` | Distilled reasoning model |
| `deepseek-r1:32b` | Larger distilled reasoning model |
| `qwen3:14b` | Strong multilingual and coding |
| `qwen2.5:7b` | Efficient multilingual model (recommended for tool calling) |
| `mistral:7b` | Fast and capable |
| `mistral-nemo:12b` | Enhanced Mistral variant (128K context) |
| `gemma3:9b` | Google's efficient open model |
| `devstral:24b` | Optimized for coding tasks |
| `codellama:13b` | Code-focused Llama variant |

**Features:**
- **Local Inference** - No API key required
- **Privacy** - Data stays on your machine
- **Tool Calling** - Full function calling support (uses non-streaming for reliability)
- **MCP Integration** - Connect to MCP servers for external tools

See [Ollama library](https://ollama.com/library) for all available models.

### New Providers

| Provider | Feature Flag | Default Model | API Key Env Var |
|----------|-------------|---------------|-----------------|
| Fireworks AI | `fireworks` | `accounts/fireworks/models/llama-v3p1-8b-instruct` | `FIREWORKS_API_KEY` |
| Together AI | `together` | `meta-llama/Llama-3.3-70B-Instruct-Turbo` | `TOGETHER_API_KEY` |
| Mistral AI | `mistral` | `mistral-small-latest` | `MISTRAL_API_KEY` |
| Perplexity | `perplexity` | `sonar` | `PERPLEXITY_API_KEY` |
| Cerebras | `cerebras` | `llama-3.3-70b` | `CEREBRAS_API_KEY` |
| SambaNova | `sambanova` | `Meta-Llama-3.3-70B-Instruct` | `SAMBANOVA_API_KEY` |
| Amazon Bedrock | `bedrock` | `anthropic.claude-sonnet-4-20250514-v1:0` | AWS IAM credentials |
| Azure AI Inference | `azure-ai` | (endpoint-specific) | `AZURE_AI_API_KEY` |

## Features

- **Streaming** - Real-time response streaming for all providers
- **Tool Calling** - Function calling support across all providers
- **Async** - Full async/await support with backpressure
- **Retry** - Automatic retry with exponential backoff
- **Generation Config** - Temperature, top_p, top_k, max_tokens

## Environment Variables

```bash
# Google Gemini
GOOGLE_API_KEY=your-google-api-key

# OpenAI
OPENAI_API_KEY=your-openai-api-key

# xAI
XAI_API_KEY=your-xai-api-key

# Anthropic
ANTHROPIC_API_KEY=your-anthropic-api-key

# DeepSeek
DEEPSEEK_API_KEY=your-deepseek-api-key

# Groq
GROQ_API_KEY=your-groq-api-key

# Fireworks AI
FIREWORKS_API_KEY=your-fireworks-api-key

# Together AI
TOGETHER_API_KEY=your-together-api-key

# Mistral AI
MISTRAL_API_KEY=your-mistral-api-key

# Perplexity
PERPLEXITY_API_KEY=your-perplexity-api-key

# Cerebras
CEREBRAS_API_KEY=your-cerebras-api-key

# SambaNova
SAMBANOVA_API_KEY=your-sambanova-api-key

# Azure AI Inference
AZURE_AI_API_KEY=your-azure-ai-api-key

# Amazon Bedrock (uses AWS IAM credentials)
AWS_ACCESS_KEY_ID=your-access-key
AWS_SECRET_ACCESS_KEY=your-secret-key
AWS_REGION=us-east-1

# Ollama (no key needed, just start the server)
# ollama serve
```

## Feature Flags

Enable specific providers with feature flags:

```toml
[dependencies]
# All providers (default)
adk-model = { version = "0.3.2", features = ["all-providers"] }

# Individual providers
adk-model = { version = "0.3.2", features = ["gemini"] }
adk-model = { version = "0.3.2", features = ["openai"] }
adk-model = { version = "0.3.2", features = ["xai"] }
adk-model = { version = "0.3.2", features = ["anthropic"] }
adk-model = { version = "0.3.2", features = ["deepseek"] }
adk-model = { version = "0.3.2", features = ["groq"] }
adk-model = { version = "0.3.2", features = ["ollama"] }
adk-model = { version = "0.3.2", features = ["fireworks"] }
adk-model = { version = "0.3.2", features = ["together"] }
adk-model = { version = "0.3.2", features = ["mistral"] }
adk-model = { version = "0.3.2", features = ["perplexity"] }
adk-model = { version = "0.3.2", features = ["cerebras"] }
adk-model = { version = "0.3.2", features = ["sambanova"] }
adk-model = { version = "0.3.2", features = ["bedrock"] }
adk-model = { version = "0.3.2", features = ["azure-ai"] }
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
