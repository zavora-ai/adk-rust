# adk-model

LLM model integrations for Rust Agent Development Kit (ADK-Rust) with Gemini, OpenAI, Anthropic, and DeepSeek.

[![Crates.io](https://img.shields.io/crates/v/adk-model.svg)](https://crates.io/crates/adk-model)
[![Documentation](https://docs.rs/adk-model/badge.svg)](https://docs.rs/adk-model)
[![License](https://img.shields.io/crates/l/adk-model.svg)](LICENSE)

## Overview

`adk-model` provides LLM integrations for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)). Supports all major providers:

- **Gemini** - Google's Gemini models (3.0 Pro, 2.5 Pro, 2.0 Flash, etc.)
- **OpenAI** - GPT-5.2, GPT-5.1, GPT-5, GPT-4o, GPT-4o-mini, Azure OpenAI
- **Anthropic** - Claude Opus 4.5, Claude Sonnet 4.5, Claude Sonnet 4, Claude 3.5
- **DeepSeek** - DeepSeek-Chat, DeepSeek-Reasoner with thinking mode
- **Groq** - Ultra-fast inference (LLaMA 3.3, Mixtral, Gemma)
- **Ollama** - Local LLMs (LLaMA, Mistral, Qwen, Gemma, etc.)
- **Streaming** - Real-time response streaming for all providers
- **Multimodal** - Text, images, audio, video, and PDF input

The crate implements the `Llm` trait from `adk-core`, allowing models to be used interchangeably.

## Installation

```toml
[dependencies]
adk-model = "0.3.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.0", features = ["models"] }
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
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

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
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-4o"))?;

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
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-20250514"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
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

## Supported Models

### Google Gemini

| Model | Description |
|-------|-------------|
| `gemini-3-pro-preview` | Most intelligent multimodal model with agentic capabilities |
| `gemini-2.5-pro` | Advanced reasoning model |
| `gemini-2.5-flash` | Latest fast model (recommended) |
| `gemini-2.5-flash-lite` | Lightweight, cost-effective |
| `gemini-2.0-flash` | Fast and efficient |

See [Gemini models documentation](https://ai.google.dev/gemini-api/docs/models/gemini) for the full list.

### OpenAI

| Model | Description |
|-------|-------------|
| `gpt-5.2` | Latest GPT-5 with enhanced reasoning |
| `gpt-5.1` | GPT-5 with improved tool use |
| `gpt-5` | GPT-5 base model |
| `gpt-4o` | Most capable GPT-4 model |
| `gpt-4o-mini` | Fast, cost-effective GPT-4 |

See [OpenAI models documentation](https://platform.openai.com/docs/models) for the full list.

### Anthropic Claude

| Model | Description |
|-------|-------------|
| `claude-opus-4-20250514` | Claude Opus 4.5 - Most capable |
| `claude-sonnet-4-20250514` | Claude Sonnet 4.5 - Balanced |
| `claude-3-5-sonnet-20241022` | Claude 3.5 Sonnet |
| `claude-3-opus-20240229` | Claude 3 Opus |

See [Anthropic models documentation](https://docs.anthropic.com/claude/docs/models-overview) for the full list.

### DeepSeek

| Model | Description |
|-------|-------------|
| `deepseek-chat` | General-purpose chat model |
| `deepseek-reasoner` | Reasoning model with chain-of-thought |

**Features:**
- **Thinking Mode** - Chain-of-thought reasoning with `<thinking>` tags
- **Context Caching** - Automatic KV cache for repeated prefixes (10x cost reduction)
- **Tool Calling** - Full function calling support

See [DeepSeek API documentation](https://api-docs.deepseek.com/) for the full list.

### Groq

| Model | Description |
|-------|-------------|
| `llama-3.3-70b-versatile` | LLaMA 3.3 70B - Most capable |
| `llama-3.1-8b-instant` | LLaMA 3.1 8B - Ultra fast |
| `mixtral-8x7b-32768` | Mixtral 8x7B - 32K context |
| `gemma2-9b-it` | Gemma 2 9B |

**Features:**
- **Ultra-Fast** - LPU-based inference (fastest in the industry)
- **Tool Calling** - Full function calling support
- **Large Context** - Up to 128K tokens

See [Groq documentation](https://console.groq.com/docs/models) for the full list.

### Ollama (Local)

| Model | Description |
|-------|-------------|
| `llama3.2` | LLaMA 3.2 - Fast and capable |
| `mistral` | Mistral 7B |
| `qwen2.5:7b` | Qwen 2.5 with excellent tool support (recommended) |
| `gemma2` | Gemma 2 |

**Features:**
- **Local Inference** - No API key required
- **Privacy** - Data stays on your machine
- **Tool Calling** - Full function calling support (uses non-streaming for reliability)
- **MCP Integration** - Connect to MCP servers for external tools

See [Ollama library](https://ollama.com/library) for all available models.

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

# Anthropic
ANTHROPIC_API_KEY=your-anthropic-api-key

# DeepSeek
DEEPSEEK_API_KEY=your-deepseek-api-key

# Groq
GROQ_API_KEY=your-groq-api-key

# Ollama (no key needed, just start the server)
# ollama serve
```

## Feature Flags

Enable specific providers with feature flags:

```toml
[dependencies]
# All providers (default)
adk-model = { version = "0.3.0", features = ["all-providers"] }

# Individual providers
adk-model = { version = "0.3.0", features = ["gemini"] }
adk-model = { version = "0.3.0", features = ["openai"] }
adk-model = { version = "0.3.0", features = ["anthropic"] }
adk-model = { version = "0.3.0", features = ["deepseek"] }
adk-model = { version = "0.3.0", features = ["groq"] }
adk-model = { version = "0.3.0", features = ["ollama"] }
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
