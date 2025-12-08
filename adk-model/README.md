# adk-model

LLM model integrations for Rust Agent Development Kit (ADK-Rust) with model Gemini, OpenAI, Anthropic.

[![Crates.io](https://img.shields.io/crates/v/adk-model.svg)](https://crates.io/crates/adk-model)
[![Documentation](https://docs.rs/adk-model/badge.svg)](https://docs.rs/adk-model)
[![License](https://img.shields.io/crates/l/adk-model.svg)](LICENSE)

## Overview

`adk-model` provides LLM integrations for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)). Supports all major providers:

- **Gemini** - Google's Gemini models (2.0 Flash, Pro, etc.)
- **OpenAI** - GPT-4o, GPT-4o-mini, Azure OpenAI
- **Anthropic** - Claude Opus 4.5, Claude Sonnet 4.5, Claude Sonnet 4, Claude 3.5
- **Streaming** - Real-time response streaming for all providers
- **Multimodal** - Text, images, audio, video, and PDF input

The crate implements the `Llm` trait from `adk-core`, allowing models to be used interchangeably.

## Installation

```toml
[dependencies]
adk-model = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["models"] }
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
| `gpt-4o` | Most capable GPT-4 model |
| `gpt-4o-mini` | Fast, cost-effective GPT-4 |
| `gpt-4-turbo` | GPT-4 Turbo with vision |
| `gpt-3.5-turbo` | Fast and economical |

See [OpenAI models documentation](https://platform.openai.com/docs/models) for the full list.

### Anthropic Claude

| Model | Description |
|-------|-------------|
| `claude-opus-4-20250514` | Claude Opus 4.5 - Most capable |
| `claude-sonnet-4-20250514` | Claude Sonnet 4.5 - Balanced |
| `claude-3-5-sonnet-20241022` | Claude 3.5 Sonnet |
| `claude-3-opus-20240229` | Claude 3 Opus |

See [Anthropic models documentation](https://docs.anthropic.com/claude/docs/models-overview) for the full list.

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
```

## Feature Flags

Enable specific providers with feature flags:

```toml
[dependencies]
# All providers (default)
adk-model = { version = "0.1", features = ["all-providers"] }

# Individual providers
adk-model = { version = "0.1", features = ["gemini"] }
adk-model = { version = "0.1", features = ["openai"] }
adk-model = { version = "0.1", features = ["anthropic"] }
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
