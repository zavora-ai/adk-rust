# ADK-Rust

[![CI](https://github.com/zavora-ai/adk-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/zavora-ai/adk-rust/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
[![docs.rs](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)

Production-ready Rust implementation of Google's Agent Development Kit (ADK). Build high-performance, memory-safe AI agent systems with streaming responses, workflow orchestration, and extensible tool integration.

## Overview

ADK-Rust provides a comprehensive framework for building AI agents in Rust, featuring:

- **Type-safe agent abstractions** with async execution and event streaming
- **Multiple agent types**: LLM agents, workflow agents (sequential, parallel, loop), and custom agents
- **Tool ecosystem**: Function tools, Google Search, MCP (Model Context Protocol) integration
- **Production features**: Session management, artifact storage, memory systems, REST/A2A APIs
- **Developer experience**: Interactive CLI, 8+ working examples, comprehensive documentation

**Status**: Production-ready, actively maintained

## Quick Start

### Installation

Requires Rust 1.75 or later. Add to your `Cargo.toml`:

```toml
[dependencies]
adk-rust = "0.1"

# Or individual crates
adk-core = "0.1"
adk-agent = "0.1"
adk-model = "0.1"  # Add features for providers: features = ["openai", "anthropic"]
adk-tool = "0.1"
adk-runner = "0.1"
```

Set your API key:

```bash
# For Gemini (default)
export GOOGLE_API_KEY="your-api-key"

# For OpenAI
export OPENAI_API_KEY="your-api-key"

# For Anthropic
export ANTHROPIC_API_KEY="your-api-key"
```

### Basic Example (Gemini)

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("Helpful AI assistant")
        .instruction("You are a helpful assistant. Be concise and accurate.")
        .model(Arc::new(model))
        .build()?;

    // Run agent (see examples for full usage)
    Ok(())
}
```

### OpenAI Example

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::openai::OpenAIModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let model = OpenAIModel::from_env("gpt-4o")?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Anthropic Example

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::anthropic::AnthropicModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let model = AnthropicModel::from_env("claude-sonnet-4-20250514")?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Ok(())
}
```

### Run Examples

```bash
# Interactive console (Gemini)
cargo run --example quickstart

# OpenAI examples (requires --features openai)
cargo run --example openai_basic --features openai
cargo run --example openai_tools --features openai

# REST API server
cargo run --example server

# Workflow agents
cargo run --example sequential_agent
cargo run --example parallel_agent

# See all examples
ls examples/
```

## Architecture

![ADK-Rust Architecture](assets/architecture.png)

ADK-Rust follows a clean layered architecture from application interface down to foundational services.

### Core Crates

| Crate | Purpose | Key Features |
|-------|---------|--------------|
| `adk-core` | Foundational traits and types | `Agent` trait, `Content`, `Part`, error types, streaming primitives |
| `adk-agent` | Agent implementations | `LlmAgent`, `SequentialAgent`, `ParallelAgent`, `LoopAgent`, builder patterns |
| `adk-model` | LLM integrations | Gemini, OpenAI, Anthropic clients, streaming, function calling |
| `adk-tool` | Tool system and extensibility | `FunctionTool`, Google Search, MCP protocol, schema validation |
| `adk-session` | Session and state management | SQLite/in-memory backends, conversation history, state persistence |
| `adk-artifact` | Artifact storage system | File-based storage, MIME type handling, image/PDF/video support |
| `adk-memory` | Long-term memory | Vector embeddings, semantic search, Qdrant integration |
| `adk-runner` | Agent execution runtime | Context management, event streaming, session lifecycle, callbacks |
| `adk-server` | Production API servers | REST API, A2A protocol, middleware, health checks |
| `adk-cli` | Command-line interface | Interactive REPL, session management, MCP server integration |

## Key Features

### Agent Types

**LLM Agents**: Powered by large language models with tool use, function calling, and streaming responses.

**Workflow Agents**: Deterministic orchestration patterns.
- `SequentialAgent`: Execute agents in sequence
- `ParallelAgent`: Execute agents concurrently
- `LoopAgent`: Iterative execution with exit conditions

**Custom Agents**: Implement the `Agent` trait for specialized behavior.

### Tool System

Built-in tools:
- Function tools (custom Rust functions)
- Google Search
- Artifact loading
- Loop termination

**MCP Integration**: Connect to Model Context Protocol servers for extended capabilities.

**XML Tool Call Markup**: For models without native function calling, ADK supports XML-based tool call parsing:
```text
<tool_call>
function_name
<arg_key>param1</arg_key>
<arg_value>value1</arg_value>
</tool_call>
```

### Multi-Provider Support

ADK supports multiple LLM providers with a unified API:

| Provider | Model Examples | Feature Flag |
|----------|---------------|--------------|
| Gemini | `gemini-2.0-flash-exp`, `gemini-1.5-pro` | (default) |
| OpenAI | `gpt-4o`, `gpt-4-turbo`, `gpt-3.5-turbo` | `openai` |
| Anthropic | `claude-sonnet-4-20250514`, `claude-3-opus` | `anthropic` |

All providers support streaming, function calling, and multimodal inputs (where available).

### Production Features

**Session Management**:
- In-memory and database-backed sessions
- Conversation history and state persistence
- SQLite support for production deployments

**Memory System**:
- Long-term memory with semantic search
- Vector embeddings for context retrieval
- Scalable knowledge storage

**Servers**:
- REST API with streaming support (SSE)
- A2A protocol for agent-to-agent communication
- Health checks and monitoring endpoints

## Documentation

- **API Reference**: [docs.rs/adk-rust](https://docs.rs/adk-rust) - Full API documentation
- **Official Docs**: [docs/official_docs/](docs/official_docs/) - ADK framework documentation
- **Examples**: [examples/README.md](examples/README.md) - 13 working examples with detailed explanations

## Development

### Testing

```bash
# Run all tests
cargo test

# Test specific crate
cargo test --package adk-core

# With output
cargo test -- --nocapture
```

### Code Quality

```bash
# Linting
cargo clippy

# Formatting
cargo fmt

# Security audit
cargo audit
```

### Building

```bash
# Development build
cargo build

# Optimized release build
cargo build --release
```

## Use as Library

Add to your `Cargo.toml`:

```toml
[dependencies]
# All-in-one crate
adk-rust = "0.1"

# Or individual crates for finer control
adk-core = "0.1"
adk-agent = "0.1"
adk-model = { version = "0.1", features = ["openai", "anthropic"] }  # Enable providers
adk-tool = "0.1"
adk-runner = "0.1"

# Optional dependencies
adk-session = { version = "0.1", optional = true }
adk-artifact = { version = "0.1", optional = true }
adk-memory = { version = "0.1", optional = true }
adk-server = { version = "0.1", optional = true }
adk-cli = { version = "0.1", optional = true }
```

## Examples

See [examples/](examples/) directory for complete, runnable examples:

**Getting Started**
- `quickstart/` - Basic agent setup and chat loop
- `function_tool/` - Custom tool implementation
- `multiple_tools/` - Agent with multiple tools
- `agent_tool/` - Use agents as callable tools

**OpenAI Integration** (requires `--features openai`)
- `openai_basic/` - Simple OpenAI GPT agent
- `openai_tools/` - OpenAI with function calling
- `openai_multimodal/` - Vision and image support
- `openai_workflow/` - Multi-agent workflows with OpenAI

**Workflow Agents**
- `sequential/` - Sequential workflow execution
- `parallel/` - Concurrent agent execution
- `loop_workflow/` - Iterative refinement patterns
- `sequential_code/` - Code generation pipeline

**Production Features**
- `load_artifacts/` - Working with images and PDFs
- `mcp/` - Model Context Protocol integration
- `server/` - REST API deployment
- `a2a/` - Agent-to-Agent communication
- `web/` - Web UI with streaming
- `research_paper/` - Complex multi-agent workflow

## Performance

Optimized for production use:
- Zero-cost abstractions with Rust's ownership model
- Efficient async I/O via Tokio runtime
- Minimal allocations and copying
- Streaming responses for lower latency
- Connection pooling and caching support

## License

Apache 2.0 (same as Google's ADK)

## Related Projects

- [ADK](https://google.github.io/adk-docs/) - Google's Agent Development Kit
- [MCP Protocol](https://modelcontextprotocol.io/) - Model Context Protocol for tool integration
- [Gemini API](https://ai.google.dev/gemini-api/docs) - Google's multimodal AI model

## Contributing

Contributions welcome! Please open an issue or pull request on GitHub.

## Roadmap

**Implemented**:
- Core framework and agent types
- Multi-provider LLM support (Gemini, OpenAI, Anthropic)
- Tool system with MCP support
- Agent Tool - Use agents as callable tools
- Session and artifact management
- Memory system
- REST and A2A servers
- CLI with interactive mode

**Planned** (see [docs/roadmap/](docs/roadmap/)):
- [VertexAI Sessions](docs/roadmap/vertex-ai-session.md) - Cloud-based session persistence
- [GCS Artifacts](docs/roadmap/gcs-artifacts.md) - Google Cloud Storage backend
- [Evaluation Framework](docs/roadmap/evaluation.md) - Testing and benchmarking
