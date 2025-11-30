# adk-model

LLM model integrations for ADK (Gemini, etc.).

[![Crates.io](https://img.shields.io/crates/v/adk-model.svg)](https://crates.io/crates/adk-model)
[![Documentation](https://docs.rs/adk-model/badge.svg)](https://docs.rs/adk-model)
[![License](https://img.shields.io/crates/l/adk-model.svg)](LICENSE)

## Overview

`adk-model` provides LLM integrations for ADK agents. Currently supports:

- **Gemini** - Google's Gemini models (2.0 Flash, Pro, etc.)
- **Streaming** - Real-time response streaming
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

```rust
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Use with an agent
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

## Supported Models

| Model | Description |
|-------|-------------|
| `gemini-2.0-flash-exp` | Fast, efficient model (recommended) |
| `gemini-1.5-pro` | Most capable model |
| `gemini-1.5-flash` | Balanced speed/capability |

## Features

- Async streaming with backpressure
- Automatic retry with exponential backoff
- Tool/function calling support
- System instruction handling
- Generation config (temperature, top_p, etc.)

## Environment Variables

```bash
GOOGLE_API_KEY=your-api-key
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
