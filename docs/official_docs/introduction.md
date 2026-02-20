# Introduction to ADK-Rust

Agent Development Kit (ADK) is a flexible and modular framework for developing and deploying AI agents. While optimized for Gemini and the Google ecosystem, ADK is model-agnostic, deployment-agnostic, and built for compatibility with other frameworks. ADK was designed to make agent development feel more like software development, making it easier for developers to create, deploy, and orchestrate agentic architectures that range from simple tasks to complex workflows.

> **Note:** ADK-Rust v0.3.2 requires Rust 1.85.0 or higher

## Installation

Add ADK-Rust to your project:

```bash
cargo add adk-rust
```

Or add it to your `Cargo.toml`:

```toml
[dependencies]
adk-rust = "0.3.2"
tokio = { version = "1.40", features = ["full"] }
```

## Quick Example

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;
    
    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful AI assistant")
        .model(Arc::new(model))
        .build()?;
    
    println!("Agent '{}' ready!", agent.name());
    Ok(())
}
```

## Architecture Overview

ADK-Rust uses a layered architecture designed for modularity and extensibility:

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                         │
│              CLI • REST Server • Web UI                      │
├─────────────────────────────────────────────────────────────┤
│                      Runner Layer                            │
│           Agent Execution • Context Management               │
├─────────────────────────────────────────────────────────────┤
│                      Agent Layer                             │
│    LlmAgent • CustomAgent • Workflow Agents                  │
├─────────────────────────────────────────────────────────────┤
│                     Service Layer                            │
│      Models • Tools • Sessions • Artifacts • Memory          │
└─────────────────────────────────────────────────────────────┘
```

## Core Concepts

ADK-Rust is built around several key primitives that work together to create powerful AI agents:

### Agents

The fundamental worker unit designed for specific tasks. ADK-Rust provides several agent types:

- **LlmAgent**: Uses a Large Language Model for reasoning and decision-making. This is the primary agent type for most use cases.
- **RealtimeAgent**: Voice-enabled agents using OpenAI Realtime API or Gemini Live API for bidirectional audio streaming.
- **GraphAgent**: LangGraph-style workflow orchestration with state management, checkpointing, and human-in-the-loop support.
- **CustomAgent**: Allows you to implement custom logic with full control over agent behavior.
- **Workflow Agents**: Deterministic agents that follow predefined execution paths:
  - `SequentialAgent`: Executes sub-agents in order
  - `ParallelAgent`: Executes sub-agents concurrently
  - `LoopAgent`: Iteratively executes sub-agents until a condition is met

### Tools

Tools give agents abilities beyond conversation, letting them interact with external APIs, search information, or perform custom operations:

- **FunctionTool**: Wrap any async Rust function as a tool
- **GoogleSearchTool**: Built-in web search capability
- **BrowserToolset**: 46 WebDriver tools for web automation (navigation, forms, screenshots, etc.)
- **ExitLoopTool**: Control loop termination in LoopAgent
- **McpToolset**: Integration with Model Context Protocol servers

### Sessions

Sessions handle the context of a single conversation, including:

- **Session ID**: Unique identifier for the conversation
- **Events**: The conversation history (user messages, agent responses, tool calls)
- **State**: Working memory for the conversation with scoped prefixes (`app:`, `user:`, `temp:`)

### Callbacks

Custom code that runs at specific points in the agent's execution:

- `before_agent` / `after_agent`: Intercept agent invocations
- `before_model` / `after_model`: Intercept LLM calls
- `before_tool` / `after_tool`: Intercept tool executions

Callbacks enable logging, guardrails, caching, and behavior modification.

### Artifacts

Binary data storage for files, images, or other non-text content:

- Save and load artifacts with versioning
- Namespace scoping (session-level or user-level)
- Pluggable storage backends

### Events

The basic unit of communication representing things that happen during a session:

- User messages
- Agent responses
- Tool calls and results
- State changes

Events form the conversation history and enable replay and debugging.

### Models

The underlying LLM that powers LlmAgents. ADK-Rust is optimized for Gemini but supports multiple providers through the `Llm` trait:

- **Gemini**: Google's Gemini models (`gemini-3-pro`, `gemini-3-flash`, `gemini-2.5-flash`, `gemini-2.5-pro`)
- **OpenAI**: `gpt-5.1`, `gpt-5`, `gpt-5-mini`, Azure OpenAI
- **Anthropic**: `claude-opus-4-5-20251101`, `claude-sonnet-4-5-20250929`, `claude-haiku-4-5-20251001`
- **DeepSeek**: `deepseek-r1`, `deepseek-v3.1`, `deepseek-chat` with thinking mode
- **Groq**: Ultra-fast inference with `llama-4-scout`, `llama-3.1-70b-versatile`, `mixtral-8x7b-32768`
- **Ollama**: Local inference with `llama3.2:3b`, `qwen2.5:7b`, `mistral:7b`, `deepseek-r1:14b`
- **mistral.rs**: High-performance local inference with hardware acceleration

All providers implement the same trait for interchangeable use:

```rust
pub trait Llm: Send + Sync {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse>;
    async fn generate_stream(&self, request: LlmRequest) -> Result<LlmResponseStream>;
}
```

### Runner

The engine that manages execution flow, orchestrates agent interactions, and coordinates with backend services. The Runner handles:

- Agent invocation and response processing
- Tool execution
- Session and state management
- Event streaming

## Feature Flags

ADK-Rust uses Cargo features for modularity:

```toml
# Full installation (default)
adk-rust = "0.3.2"

# Minimal: Only agents + Gemini
adk-rust = { version = "0.3.2", default-features = false, features = ["minimal"] }

# Custom: Pick what you need
adk-rust = { version = "0.3.2", default-features = false, features = ["agents", "gemini", "tools"] }
```

Available features:
- `agents`: Agent implementations (LlmAgent, CustomAgent, workflow agents)
- `models`: Model integrations (Gemini)
- `openai`: OpenAI models (GPT-5, GPT-5 Mini)
- `anthropic`: Anthropic models (Claude 4.5, Claude 4)
- `deepseek`: DeepSeek models (chat, reasoner)
- `groq`: Groq ultra-fast inference
- `ollama`: Local Ollama models
- `tools`: Tool system and built-in tools
- `sessions`: Session management
- `artifacts`: Artifact storage
- `memory`: Memory system with semantic search
- `runner`: Agent execution runtime
- `server`: HTTP server (REST + A2A)
- `telemetry`: OpenTelemetry integration
- `cli`: CLI launcher

## Other Languages

ADK is available in multiple languages:

- **Python**: `pip install google-adk` - [Documentation](https://github.com/google/adk-python)
- **Go**: `go get google.golang.org/adk` - [Documentation](https://github.com/google/adk-go)
- **Java**: Maven/Gradle - [Documentation](https://github.com/google/adk-java)

---

**Next**: [Quickstart →](quickstart.md)
