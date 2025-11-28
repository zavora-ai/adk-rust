# Agent Development Kit (Rust)

The Agent Development Kit (ADK) is a flexible and modular framework for developing and deploying AI agents. While optimized for Gemini and the Google ecosystem, ADK is model-agnostic, deployment-agnostic, and is built for compatibility with other frameworks. ADK was designed to make agent development feel more like software development, to make it easier for developers to create, deploy, and orchestrate agentic architectures that range from simple tasks to complex workflows.

**ADK-Rust** is a high-performance, memory-safe Rust implementation of Google's ADK framework, providing zero-cost abstractions, efficient async I/O, and compile-time safety guarantees.

> [!IMPORTANT]
> ADK-Rust v0.1.0 requires Rust 1.75 or higher

## Latest Release

**Version**: 0.1.0 (November 2025)  
**Status**: Production Ready

Get started:

```bash
# Add to Cargo.toml
[dependencies]
adk-core = "0.1"
adk-agent = "0.1"
adk-model = "0.1"
adk-tool = "0.1"
```

Or install the CLI:

```bash
cargo install adk-cli
```

## Why ADK-Rust?

### Performance & Safety
- **Zero-cost abstractions**: Trait-based polymorphism compiles to direct calls
- **Memory safety**: No buffer overflows, use-after-free, or data races
- **Efficient async I/O**: Built on Tokio for high-performance concurrency
- **Minimal allocations**: Streaming responses, careful memory management

### Developer Experience
- **Type-safe**: Compile-time error checking catches bugs early
- **Ergonomic builders**: Fluent APIs for constructing agents and configurations
- **Rich error messages**: Detailed error types with context
- **Comprehensive examples**: 12 working examples demonstrating all features

### Feature Complete
- ✅ All core ADK features from Go implementation
- ✅ MCP (Model Context Protocol) integration
- ✅ Multiple agent types (LLM, Custom, Workflow)
- ✅ Tool system with function calling
- ✅ Session and artifact management
- ✅ Memory system with semantic search
- ✅ REST and A2A server support
- ✅ Production-ready telemetry

## Key Features

### Agent Types
Build agents for any use case:
- **LlmAgent**: Powered by language models (Gemini)
- **CustomAgent**: User-defined logic and behavior
- **SequentialAgent**: Chain agents in sequence
- **ParallelAgent**: Run agents concurrently
- **LoopAgent**: Iterative execution with exit conditions
- **ConditionalAgent**: Branching logic based on context

### Model Integration
- **Gemini 2.0 Flash**: Latest Google AI model
- **Streaming responses**: Real-time event streaming
- **Function calling**: Native tool integration
- **Multi-turn conversations**: Stateful interactions

### Tool System
Extend agent capabilities:
- **Function tools**: Custom Rust functions as tools
- **Google Search**: Web search integration
- **MCP integration**: Connect to Model Context Protocol servers
- **Toolsets**: Compose and organize tools
- **Exit Loop**: Control iterative workflows
- **Load Artifacts**: Access stored artifacts

### State Management
Persistent and transient state:
- **Sessions**: In-memory or SQLite storage
- **Artifacts**: File versioning and storage
- **Memory**: Long-term memory with semantic search
- **Event history**: Full conversation replay

### Deployment Options
Run anywhere:
- **CLI**: Interactive console or script mode
- **REST API**: HTTP server with standard endpoints
- **A2A Protocol**: Agent-to-Agent communication
- **Embedded**: Use as a library in your application

## Architecture Overview

ADK-Rust uses a layered architecture:

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                         │
│         (CLI, REST Server, A2A Server, Examples)            │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Runner Layer                           │
│    (Agent Execution, Context Management, Event Streaming)    │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Agent Layer                            │
│   (Agent Trait, LLMAgent, WorkflowAgents, CustomAgent)      │
└─────────────────────────────────────────────────────────────┘
                              │
┌──────────────┬──────────────┬──────────────┬────────────────┐
│   Model      │    Tool      │   Session    │   Services     │
│   Layer      │    Layer     │   Layer      │   Layer        │
└──────────────┴──────────────┴──────────────┴────────────────┘
```

### Core Packages

| Package | Description | Use When |
|---------|-------------|----------|
| `adk-core` | Core traits and types | Always (foundation) |
| `adk-agent` | Agent implementations | Building agents |
| `adk-model` | Model integrations | Using LLMs |
| `adk-tool` | Tool system | Adding capabilities |
| `adk-session` | Session management | Persisting conversations |
| `adk-artifact` | Artifact storage | Storing files/outputs |
| `adk-memory` | Memory system | Long-term context |
| `adk-runner` | Execution runtime | Running agents |
| `adk-server` | HTTP servers | Deploying as service |
| `adk-cli` | Command-line interface | Interactive usage |

## Quick Example

Here's a minimal example of creating an agent:

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_tool::GoogleSearchTool;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create Gemini model
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    
    // Build agent with tools
    let agent = LlmAgentBuilder::new("assistant")
        .description("Helpful research assistant")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;
    
    println!("Agent '{}' created successfully!", agent.name());
    Ok(())
}
```

## What's Next?

- **[Installation](02_installation.md)**: Set up your development environment
- **[Quick Start](03_quickstart.md)**: Build your first agent in 5 minutes
- **[Core Concepts](04_concepts.md)**: Understand the ADK architecture
- **[Examples](../examples/README.md)**: Explore 12 working examples
- **[API Reference](05_api_reference.md)**: Detailed API documentation

## Community & Support

- **Documentation**: [Full documentation](README.md)
- **Examples**: [12 working examples](../examples/README.md)
- **Architecture**: [Architecture guide](ARCHITECTURE.md)
- **Issues**: Report bugs or request features on GitHub
- **License**: Apache 2.0 (same as Google's ADK)

## Version Compatibility

| ADK-Rust | Rust | Gemini API | MCP |
|----------|------|------------|-----|
| 0.1.x | 1.75+ | 2.0 | 1.0 |

---

Ready to build your first agent? Continue to **[Installation →](02_installation.md)**
