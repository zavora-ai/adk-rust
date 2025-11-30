# ADK-Rust

Production-ready Rust implementation of Google's Agent Development Kit (ADK). Build high-performance, memory-safe AI agent systems with streaming responses, workflow orchestration, and extensible tool integration.

## Overview

ADK-Rust provides a comprehensive framework for building AI agents in Rust, featuring:

- **Type-safe agent abstractions** with async execution and event streaming
- **Multiple agent types**: LLM agents, workflow agents (sequential, parallel, loop), and custom agents
- **Tool ecosystem**: Function tools, Google Search, MCP (Model Context Protocol) integration
- **Production features**: Session management, artifact storage, memory systems, REST/A2A APIs
- **Developer experience**: Interactive CLI, 8+ working examples, comprehensive documentation

**Status**: Core implementation complete (90%), actively maintained

## Quick Start

### Installation

Requires Rust 1.75 or later:

```bash
rustup update
export GOOGLE_API_KEY="your-api-key"
```

### Basic Example

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

### Run Examples

```bash
# Interactive console
cargo run --example quickstart

# REST API server
cargo run --example server

# Workflow agents
cargo run --example sequential_agent
cargo run --example parallel_agent

# See all examples
ls examples/*.rs
```

## Architecture

![ADK-Rust Architecture](assets/architecture.png)

ADK-Rust follows a clean layered architecture from application interface down to foundational services.

### Core Crates

| Crate | Purpose | Key Features |
|-------|---------|--------------|
| `adk-core` | Foundational traits and types | `Agent` trait, `Content`, `Part`, error types, streaming primitives |
| `adk-agent` | Agent implementations | `LlmAgent`, `SequentialAgent`, `ParallelAgent`, `LoopAgent`, builder patterns |
| `adk-model` | LLM integrations | Gemini API client, `GenerativeModel` trait, streaming, function calling |
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

- **Book**: [adk-rust-book/](adk-rust-book/) - Comprehensive guide from basics to production
- **Examples**: [examples/README.md](examples/README.md) - 13 working examples with detailed explanations
- **Official Docs**: [docs/official_docs/](docs/official_docs/) - ADK framework documentation

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
adk-core = { path = "path/to/adk-rust/adk-core" }
adk-agent = { path = "path/to/adk-rust/adk-agent" }
adk-model = { path = "path/to/adk-rust/adk-model" }
adk-tool = { path = "path/to/adk-rust/adk-tool" }
adk-runner = { path = "path/to/adk-rust/adk-runner" }

# Optional dependencies
adk-session = { path = "path/to/adk-rust/adk-session", optional = true }
adk-artifact = { path = "path/to/adk-rust/adk-artifact", optional = true }
adk-memory = { path = "path/to/adk-rust/adk-memory", optional = true }
```

## Examples

See [examples/](examples/) directory for 13 complete, runnable examples:

- `quickstart/` - Basic agent setup and chat loop
- `function_tool/` - Custom tool implementation
- `multiple_tools/` - Agent with multiple tools
- `sequential/` - Sequential workflow execution
- `parallel/` - Concurrent agent execution
- `loop_workflow/` - Iterative refinement patterns
- `load_artifacts/` - Working with images and PDFs
- `mcp/` - Model Context Protocol integration
- `server/` - REST API deployment
- `a2a/` - Agent-to-Agent communication
- `web/` - Web UI with streaming
- `research_paper/` - Complex multi-agent workflow
- `sequential_code/` - Code generation pipeline

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

## Project Status

**Current Phase**: Documentation and polish (90% implementation complete)

**Completed**:
- Core framework and agent types
- Model integration (Gemini)
- Tool system with MCP support
- Session and artifact management
- Memory system with vector search
- REST and A2A servers
- CLI with interactive mode
- 8+ production-quality examples

**Next Steps**:
- Complete comprehensive book/guide
- Performance benchmarking
- Deployment best practices documentation
- 0.1.0 release preparation
