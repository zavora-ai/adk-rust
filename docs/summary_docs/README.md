# ADK-Rust Documentation

Welcome to the Agent Development Kit (ADK) for Rust documentation.

## Overview

ADK-Rust is a high-performance, memory-safe Rust implementation of Google's Agent Development Kit. It provides a flexible and modular framework for building AI agents that can range from simple chatbots to complex multi-agent workflows.

## Documentation Structure

### Getting Started

1. **[Introduction](01_introduction.md)**  
   Learn about ADK-Rust, its features, and why you should use it.

2. **[Installation & Setup](02_installation.md)**  
   Install Rust, configure your environment, and set up ADK-Rust.

3. **[Quick Start](03_quickstart.md)**  
   Build your first agent in 5 minutes.

### Core Documentation

4. **[Core Concepts](04_concepts.md)**  
   Understand agents, models, tools, sessions, and the runtime.

5. **[API Reference](05_api_reference.md)**  
   Complete API documentation for all crates.

### Advanced Topics

6. **[MCP Integration](06_mcp.md)**  
   Integrate Model Context Protocol servers for extended capabilities.

7. **[Workflow Patterns](07_workflows.md)**  
   Build complex orchestrations with sequential, parallel, loop, and conditional agents.

8. **[Deployment Guide](08_deployment.md)**  
   Deploy to production with Docker, Kubernetes, or serverless.

9. **[CLI Usage](09_cli.md)**  
   Use the `adk-cli` command-line tool.

10. **[Troubleshooting](10_troubleshooting.md)**  
    Common issues and solutions.

## Quick Links

- **[Examples](../examples/README.md)**: 12 working examples demonstrating all features
- **[Architecture Guide](ARCHITECTURE.md)**: System design and patterns
- **[CHANGELOG](../CHANGELOG.md)**: Version history and changes
- **[GitHub Repository](https://github.com/your-org/adk-rust)**: Source code

## Key Features

### 🚀 Performance
- Zero-cost abstractions
- Efficient async I/O with Tokio
- Minimal allocations
- Streaming responses

### 🔒 Safety
- Memory-safe by design
- No buffer overflows or data races
- Compile-time error checking
- Type-safe error handling

### 🧩 Modular
- 10 independent crates
- Trait-based extensibility
- Mix and match components
- Easy to customize

### 🤖 Agent Types
- **LlmAgent**: AI-powered with Gemini
- **CustomAgent**: User-defined logic
- **SequentialAgent**: Step-by-step workflows
- **ParallelAgent**: Concurrent execution
- **LoopAgent**: Iterative refinement
- **ConditionalAgent**: Branching logic

### 🔧 Tools & Integration
- Function tools (custom Rust functions)
- Google Search
- MCP (Model Context Protocol)
- Toolsets for composition
- Exit Loop, Load Artifacts, and more

### 💾 State Management
- In-memory sessions
- SQLite/PostgreSQL storage
- Artifact versioning
- Long-term memory with semantic search

### 🌐 Deployment Options
- CLI (interactive console)
- REST API server
- A2A (Agent-to-Agent) protocol
- Docker containers
- Kubernetes
- Embedded library

## Learning Path

### Beginner Path

1. ✅ Read [Introduction](01_introduction.md)
2. ✅ Follow [Installation](02_installation.md)
3. ✅ Complete [Quick Start](03_quickstart.md)
4. ✅ Run `cargo run --example quickstart`
5. ✅ Try other [Examples](../examples/README.md)

### Intermediate Path

1. ✅ Study [Core Concepts](04_concepts.md)
2. ✅ Review [API Reference](05_api_reference.md)
3. ✅ Build a custom agent
4. ✅ Add custom tools
5. ✅ Try [MCP Integration](06_mcp.md)

### Advanced Path

1. ✅ Master [Workflow Patterns](07_workflows.md)
2. ✅ Study [Architecture Guide](ARCHITECTURE.md)
3. ✅ Implement complex multi-agent systems
4. ✅ Read [Deployment Guide](08_deployment.md)
5. ✅ Deploy to production

## Code Examples

### Simple Agent

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use std::sync::Arc;

let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

let agent = LlmAgentBuilder::new("assistant")
    .model(Arc::new(model))
    .build()?;
```

### Agent with Tools

```rust
use adk_tool::{GoogleSearchTool, FunctionTool};

let agent = LlmAgentBuilder::new("researcher")
    .model(Arc::new(model))
    .tool(Arc::new(GoogleSearchTool::new()))
    .tool(Arc::new(calculator))
    .build()?;
```

### Workflow

```rust
use adk_agent::SequentialAgent;

let workflow = SequentialAgent::new(
    "pipeline",
    vec![analyzer, processor, summarizer],
);
```

### Full Application

```rust
use adk_runner::Runner;
use adk_session::InMemorySessionService;

let runner = Runner::new(
    "my-app",
    Arc::new(agent),
    Arc::new(InMemorySessionService::new()),
);

let events = runner.run(user_id, session_id, content).await?;
```

## Version Information

**Current Version**: 0.1.0  
**Rust Version**: 1.75+  
**Status**: Production Ready

### Compatibility

| ADK-Rust | Rust | Gemini API | MCP |
|----------|------|------------|-----|
| 0.1.x | 1.75+ | 2.0 | 1.0 |

## Crates Overview

| Crate | Description | Documentation |
|-------|-------------|---------------|
| `adk-core` | Core traits and types | [API Reference](05_api_reference.md#adk-core) |
| `adk-agent` | Agent implementations | [API Reference](05_api_reference.md#adk-agent) |
| `adk-model` | Model integrations | [API Reference](05_api_reference.md#adk-model) |
| `adk-tool` | Tool system | [API Reference](05_api_reference.md#adk-tool) |
| `adk-session` | Session management | [API Reference](05_api_reference.md#adk-session) |
| `adk-artifact` | Artifact storage | [API Reference](05_api_reference.md#adk-artifact) |
| `adk-memory` | Memory system | [API Reference](05_api_reference.md#adk-memory) |
| `adk-runner` | Execution runtime | [API Reference](05_api_reference.md#adk-runner) |
| `adk-server` | HTTP servers | [API Reference](05_api_reference.md#adk-server) |
| `adk-cli` | CLI tool | [CLI Usage](09_cli.md) |

## Community

- **GitHub**: [github.com/your-org/adk-rust](https://github.com/your-org/adk-rust)
- **Issues**: Report bugs or request features
- **Discussions**: Ask questions and share ideas
- **Contributing**: See [CONTRIBUTING.md](../CONTRIBUTING.md)

## Related Projects

- [ADK for Go](https://github.com/google/adk-go) - Original Go implementation
- [ADK for Python](https://github.com/google/adk-python) - Python version
- [MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) - Official MCP SDK
- [Gemini Rust](https://github.com/your-org/gemini-rust) - Gemini API client

## License

Apache 2.0 - Same asGoogle's ADK

## Need Help?

- 📖 Read the [Documentation](README.md)
- 🔍 Check [Troubleshooting](10_troubleshooting.md)
- 💬 Ask on GitHub Discussions
- 🐛 Report bugs on GitHub Issues
- 📧 Contact the maintainers

---

**Ready to get started?** → [Installation Guide](02_installation.md)
