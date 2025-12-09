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
- **Realtime voice agents**: Bidirectional audio streaming with OpenAI Realtime API and Gemini Live API
- **Tool ecosystem**: Function tools, Google Search, MCP (Model Context Protocol) integration
- **Production features**: Session management, artifact storage, memory systems, REST/A2A APIs
- **Developer experience**: Interactive CLI, 15+ working examples, comprehensive documentation

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
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

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
    let model = OpenAIModel::from_env("gpt-4.1")?;

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
    let model = AnthropicModel::from_env("claude-sonnet-4")?;

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
| `adk-realtime` | Real-time voice agents | OpenAI Realtime API, Gemini Live API, bidirectional audio, VAD |
| `adk-graph` | Graph-based workflows | LangGraph-style orchestration, state management, checkpointing, human-in-the-loop |
| `adk-browser` | Browser automation | 46 WebDriver tools, navigation, forms, screenshots, PDF generation |
| `adk-eval` | Agent evaluation | Test definitions, trajectory validation, LLM-judged scoring, rubrics |

## Key Features

### Agent Types

**LLM Agents**: Powered by large language models with tool use, function calling, and streaming responses.

**Workflow Agents**: Deterministic orchestration patterns.
- `SequentialAgent`: Execute agents in sequence
- `ParallelAgent`: Execute agents concurrently
- `LoopAgent`: Iterative execution with exit conditions

**Custom Agents**: Implement the `Agent` trait for specialized behavior.

**Realtime Voice Agents**: Build voice-enabled AI assistants with bidirectional audio streaming.

**Graph Agents**: LangGraph-style workflow orchestration with state management and checkpointing.

### Realtime Voice Agents

Build voice-enabled AI assistants using the `adk-realtime` crate:

```rust
use adk_realtime::{RealtimeAgent, openai::OpenAIRealtimeModel, RealtimeModel};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model: Arc<dyn RealtimeModel> = Arc::new(
        OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17")
    );

    let agent = RealtimeAgent::builder("voice_assistant")
        .model(model)
        .instruction("You are a helpful voice assistant.")
        .voice("alloy")
        .server_vad()  // Enable voice activity detection
        .build()?;

    Ok(())
}
```

**Supported Realtime Models**:
| Provider | Model | Description |
|----------|-------|-------------|
| OpenAI | `gpt-4o-realtime-preview-2024-12-17` | Stable realtime model |
| OpenAI | `gpt-realtime` | Latest model with improved speech quality and function calling |
| Google | `gemini-2.0-flash-live-preview-04-09` | Gemini Live API |

**Features**:
- OpenAI Realtime API and Gemini Live API support
- Bidirectional audio streaming (PCM16, G711)
- Server-side Voice Activity Detection (VAD)
- Real-time tool calling during voice conversations
- Multi-agent handoffs for complex workflows

**Run realtime examples**:
```bash
cargo run --example realtime_basic --features realtime-openai
cargo run --example realtime_tools --features realtime-openai
cargo run --example realtime_handoff --features realtime-openai
```

### Graph-Based Workflows

Build complex, stateful workflows using the `adk-graph` crate (LangGraph-style):

```rust
use adk_graph::{prelude::*, node::AgentNode};
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;

// Create LLM agents for different tasks
let translator = Arc::new(LlmAgentBuilder::new("translator")
    .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?))
    .instruction("Translate the input text to French.")
    .build()?);

let summarizer = Arc::new(LlmAgentBuilder::new("summarizer")
    .model(model.clone())
    .instruction("Summarize the input text in one sentence.")
    .build()?);

// Create AgentNodes with custom input/output mappers
let translator_node = AgentNode::new(translator)
    .with_input_mapper(|state| {
        let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
        adk_core::Content::new("user").with_text(text)
    })
    .with_output_mapper(|events| {
        let mut updates = HashMap::new();
        for event in events {
            if let Some(content) = event.content() {
                let text: String = content.parts.iter()
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("");
                updates.insert("translation".to_string(), json!(text));
            }
        }
        updates
    });

// Build graph with parallel execution
let agent = GraphAgent::builder("text_processor")
    .description("Translates and summarizes text in parallel")
    .channels(&["input", "translation", "summary"])
    .node(translator_node)
    .node(summarizer_node)  // Similar setup
    .edge(START, "translator")
    .edge(START, "summarizer")  // Parallel execution
    .edge("translator", "combine")
    .edge("summarizer", "combine")
    .edge("combine", END)
    .build()?;

// Execute
let mut input = State::new();
input.insert("input".to_string(), json!("AI is transforming how we work."));
let result = agent.invoke(input, ExecutionConfig::new("thread-1")).await?;
```

**Features**:
- **AgentNode**: Wrap LLM agents as graph nodes with custom input/output mappers
- **Parallel & Sequential**: Execute agents concurrently or in sequence
- **Cyclic Graphs**: ReAct pattern with tool loops and iteration limiting
- **Conditional Routing**: Dynamic routing via `Router::by_field` or custom functions
- **Checkpointing**: Memory and SQLite backends for fault tolerance
- **Human-in-the-Loop**: Dynamic interrupts based on state, resume from checkpoint
- **Streaming**: Multiple modes (values, updates, messages, debug)

**Run graph examples**:
```bash
cargo run --example graph_agent       # Parallel LLM agents with callbacks
cargo run --example graph_workflow    # Sequential multi-agent pipeline
cargo run --example graph_conditional # LLM-based routing
cargo run --example graph_react       # ReAct pattern with tools
cargo run --example graph_supervisor  # Multi-agent supervisor
cargo run --example graph_hitl        # Human-in-the-loop approval
cargo run --example graph_checkpoint  # State persistence
```

### Browser Automation

Give agents web browsing capabilities using the `adk-browser` crate:

```rust
use adk_browser::{BrowserSession, BrowserToolset, BrowserConfig};

// Create browser session
let config = BrowserConfig::new("http://localhost:4444");
let session = BrowserSession::new(config).await?;

// Get all 46 browser tools
let toolset = BrowserToolset::new(session);
let tools = toolset.all_tools();

// Add to agent
let agent = LlmAgentBuilder::new("web_agent")
    .model(model)
    .instruction("Browse the web and extract information.")
    .tools(tools)
    .build()?;
```

**46 Browser Tools**:
- Navigation: `browser_navigate`, `browser_back`, `browser_forward`, `browser_refresh`
- Extraction: `browser_extract_text`, `browser_extract_links`, `browser_extract_html`
- Interaction: `browser_click`, `browser_type`, `browser_select`, `browser_submit`
- Forms: `browser_fill_form`, `browser_get_form_fields`, `browser_clear_field`
- Screenshots: `browser_screenshot`, `browser_screenshot_element`
- JavaScript: `browser_evaluate`, `browser_evaluate_async`
- Cookies, frames, windows, and more

**Requirements**: WebDriver (Selenium, ChromeDriver, etc.)
```bash
docker run -d -p 4444:4444 selenium/standalone-chrome
cargo run --example browser_agent
```

### Agent Evaluation

Test and validate agent behavior using the `adk-eval` crate:

```rust
use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria};

let config = EvaluationConfig::with_criteria(
    EvaluationCriteria::exact_tools()
        .with_response_similarity(0.8)
);

let evaluator = Evaluator::new(config);
let report = evaluator
    .evaluate_file(agent, "tests/my_agent.test.json")
    .await?;

assert!(report.all_passed());
```

**Evaluation Capabilities**:
- Trajectory validation (tool call sequences)
- Response similarity (Jaccard, Levenshtein, ROUGE)
- LLM-judged semantic matching
- Rubric-based scoring with custom criteria
- Safety and hallucination detection
- Detailed reporting with failure analysis

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
| Gemini | `gemini-2.5-flash`, `gemini-2.5-pro`, `gemini-2.0-flash` | (default) |
| OpenAI | `gpt-4.1`, `gpt-4.1-mini`, `o3-mini`, `gpt-4o` | `openai` |
| Anthropic | `claude-sonnet-4`, `claude-opus-4`, `claude-haiku-4` | `anthropic` |

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
- **Examples**: [examples/README.md](examples/README.md) - 50+ working examples with detailed explanations

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
adk-realtime = { version = "0.1", features = ["openai"], optional = true }
adk-graph = { version = "0.1", features = ["sqlite"], optional = true }
adk-browser = { version = "0.1", optional = true }
adk-eval = { version = "0.1", optional = true }
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

**Realtime Voice Agents** (requires `--features realtime-openai`)
- `realtime_basic/` - Basic text-only realtime session
- `realtime_vad/` - Voice assistant with VAD
- `realtime_tools/` - Tool calling in realtime sessions
- `realtime_handoff/` - Multi-agent handoffs

**Graph Workflows**
- `graph_agent/` - GraphAgent with parallel LLM agents and callbacks
- `graph_workflow/` - Sequential multi-agent pipeline
- `graph_conditional/` - LLM-based classification and routing
- `graph_react/` - ReAct pattern with tools and cycles
- `graph_supervisor/` - Multi-agent supervisor routing
- `graph_hitl/` - Human-in-the-loop with risk-based interrupts
- `graph_checkpoint/` - State persistence and time travel debugging

**Browser Automation**
- `browser_basic/` - Basic browser session and tools
- `browser_agent/` - AI agent with browser tools
- `browser_interactive/` - Full 46-tool interactive example

**Agent Evaluation**
- `eval_basic/` - Basic evaluation setup
- `eval_trajectory/` - Tool call trajectory validation
- `eval_semantic/` - LLM-judged semantic matching
- `eval_rubric/` - Rubric-based scoring

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
- Realtime voice agents (OpenAI Realtime API, Gemini Live API)
- Graph-based workflows (LangGraph-style) with checkpointing and human-in-the-loop
- Browser automation (46 WebDriver tools)
- Agent evaluation framework with trajectory validation and LLM-judged scoring

**Planned** (see [docs/roadmap/](docs/roadmap/)):
- [VertexAI Sessions](docs/roadmap/vertex-ai-session.md) - Cloud-based session persistence
- [GCS Artifacts](docs/roadmap/gcs-artifacts.md) - Google Cloud Storage backend
