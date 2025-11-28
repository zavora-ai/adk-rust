# adk-rust

**Agent Development Kit (ADK) for Rust** - Build AI agents with simplicity and power.

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

## Quick Start

```toml
[dependencies]
adk-rust = "0.1"
tokio = { version = "1.40", features = ["full"] }
```

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;
    
    println!("Agent '{}' ready!", agent.name());
    Ok(())
}
```

## Installation Options

### Simple (Everything)
```toml
[dependencies]
adk-rust = "0.1"
```

### Minimal (Just agents + Gemini)
```toml
[dependencies]
adk-rust = { version = "0.1", default-features = false, features = ["minimal"] }
```

### Custom (Pick components)
```toml
[dependencies]
adk-rust = { version = "0.1", default-features = false, features = [
    "agents",
    "gemini",
    "tools",
    "sessions"
] }
```

## Features

| Feature | Description | Included in `default` |
|---------|-------------|----------------------|
| `full` | Everything | ✅ |
| `minimal` | Agents + Gemini + Runner | ❌ |
| `agents` | Agent implementations | ✅ |
| `models` | Model integrations | ✅ |
| `gemini` | Gemini model support | ✅ |
| `tools` | Tool system | ✅ |
| `mcp` | MCP integration | ✅ |
| `sessions` | Session management | ✅ |
| `artifacts` | Artifact storage | ✅ |
| `memory` | Memory system | ✅ |
| `runner` | Execution runtime | ✅ |
| `server` | HTTP server | ✅ |
| `telemetry` | OpenTelemetry | ✅ |

## Documentation

- 📚 [Full Documentation](https://docs.rs/adk-rust)
- 🚀 [Quick Start Guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/quickstart.md)
- 📖 [Examples](https://github.com/zavora-ai/adk-rust/tree/main/examples)
- 🏗️ [Architecture](https://github.com/zavora-ai/adk-rust/blob/main/docs/ARCHITECTURE.md)

## Why ADK-Rust?

- ⚡ **High Performance**: Zero-cost abstractions, efficient async I/O
- 🔒 **Memory Safe**: Rust's guarantees prevent common bugs
- 🧩 **Modular**: Use only what you need
- 🤖 **Full-Featured**: All agent types, tools, and deployment options
- 📦 **Simple**: One command to get started

## Examples

See [examples/](https://github.com/zavora-ai/adk-rust/tree/main/examples) for 12 working examples.

## License

Apache 2.0 - Same as Google's ADK

## Related Projects

- [ADK for Go](https://github.com/google/adk-go)
- [ADK for Python](https://github.com/google/adk-python)
