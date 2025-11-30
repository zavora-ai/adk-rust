# ADK-Rust

**Agent Development Kit for Rust** - Build AI agents with simplicity and power.

[![Crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
[![Documentation](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

A flexible and modular framework for developing and deploying AI agents. While optimized for Gemini and the Google ecosystem, ADK is model-agnostic, deployment-agnostic, and built for compatibility with other frameworks.

## Quick Start

**1. Create a new project:**

```bash
cargo new my_agent
cd my_agent
```

**2. Add dependencies to `Cargo.toml`:**

```toml
[dependencies]
adk-rust = "0.1"
tokio = { version = "1.40", features = ["full"] }
dotenv = "0.15"
```

**3. Set up your API key:**

```bash
echo 'GOOGLE_API_KEY=your-api-key-here' > .env
```

Get a key from [Google AI Studio](https://aistudio.google.com/app/apikey).

**4. Write your agent in `src/main.rs`:**

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful AI assistant")
        .instruction("You are a friendly assistant. Answer questions concisely.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

**5. Run your agent:**

```bash
cargo run
```

You'll see an interactive chat where you can talk to your agent!

## Agent Types

### LlmAgent - AI-Powered Reasoning

The core agent type using Large Language Models:

```rust
let agent = LlmAgentBuilder::new("researcher")
    .description("Research assistant with web search")
    .instruction("Search for information and provide summaries.")
    .model(Arc::new(model))
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;
```

### Workflow Agents - Deterministic Pipelines

For predictable, multi-step workflows:

```rust
// Sequential: Execute in order
let pipeline = SequentialAgent::new("pipeline", vec![researcher, writer, reviewer]);

// Parallel: Execute concurrently
let parallel = ParallelAgent::new("analysis", vec![analyst1, analyst2]);

// Loop: Iterate until condition
let loop_agent = LoopAgent::new("refiner", refiner_agent, 5);
```

### Multi-Agent Systems

Build hierarchical agent teams:

```rust
let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Delegate tasks to specialists.")
    .model(model)
    .sub_agent(code_agent)
    .sub_agent(test_agent)
    .build()?;
```

## Tools

Give agents capabilities beyond conversation:

### Function Tools

Convert any async function into a tool:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
struct WeatherInput {
    city: String,
}

async fn get_weather(_ctx: ToolContext, input: WeatherInput) -> Result<String> {
    Ok(format!("Weather in {}: Sunny, 72°F", input.city))
}

let tool = FunctionTool::new("get_weather", "Get weather for a city", get_weather);
```

### Built-in Tools

- `GoogleSearchTool` - Web search via Google
- `ExitLoopTool` - Control loop termination
- `LoadArtifactsTool` - Access stored artifacts

### MCP Integration

Connect to Model Context Protocol servers:

```rust
let mcp_tools = McpToolset::from_command("npx", &[
    "-y", "@anthropic/mcp-server-filesystem", "/path/to/dir"
]).await?;
```

## Sessions & State

Manage conversation context:

```rust
let session = session_service.create("user_123", None).await?;

// Store state with scoped prefixes
session.state().set("app:config", "production");
session.state().set("user:name", "Alice");
session.state().set("temp:cache", "value");
```

## Callbacks

Intercept and customize behavior:

```rust
let agent = LlmAgentBuilder::new("monitored")
    .model(model)
    .before_agent(|ctx| Box::pin(async move {
        println!("Starting: {}", ctx.agent_name);
        Ok(None)
    }))
    .after_model(|ctx, response| Box::pin(async move {
        println!("Tokens: {}", response.usage.output_tokens);
        Ok(response)
    }))
    .build()?;
```

## Deployment

### Console Mode

```bash
cargo run
```

### Server Mode

```bash
cargo run -- serve --port 8080
```

Provides REST API endpoints:
- `POST /chat` - Send messages
- `GET /sessions` - List sessions
- `GET /health` - Health check

### A2A Protocol

Agent-to-Agent communication:

```rust
let card = AgentCard::new("my_agent", "https://my-agent.example.com")
    .with_description("A helpful assistant")
    .with_skill("research", "Can search and summarize");

let server = A2AServer::new(agent, card, session_service, artifact_service);
server.serve(8080).await?;
```

## Installation Options

### Full (Default)

```toml
[dependencies]
adk-rust = "0.1"
```

### Minimal

```toml
[dependencies]
adk-rust = { version = "0.1", default-features = false, features = ["minimal"] }
```

### Custom

```toml
[dependencies]
adk-rust = { version = "0.1", default-features = false, features = [
    "agents", "gemini", "tools", "sessions"
] }
```

## Feature Flags

| Feature | Description | Default |
|---------|-------------|---------|
| `agents` | Agent implementations | ✅ |
| `models` | Model integrations | ✅ |
| `gemini` | Gemini model support | ✅ |
| `tools` | Tool system | ✅ |
| `mcp` | MCP integration | ✅ |
| `sessions` | Session management | ✅ |
| `artifacts` | Artifact storage | ✅ |
| `memory` | Semantic memory | ✅ |
| `runner` | Execution runtime | ✅ |
| `server` | HTTP server | ✅ |
| `telemetry` | OpenTelemetry | ✅ |
| `cli` | CLI launcher | ✅ |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                        │
│              Launcher • REST Server • A2A                   │
├─────────────────────────────────────────────────────────────┤
│                      Runner Layer                           │
│           Agent Execution • Event Streaming                 │
├─────────────────────────────────────────────────────────────┤
│                      Agent Layer                            │
│    LlmAgent • CustomAgent • Sequential • Parallel • Loop    │
├─────────────────────────────────────────────────────────────┤
│                     Service Layer                           │
│      Models • Tools • Sessions • Artifacts • Memory         │
└─────────────────────────────────────────────────────────────┘
```

## Documentation

- [Full Documentation](https://docs.rs/adk-rust)
- [Official Guides](https://github.com/zavora-ai/adk-rust/tree/main/docs/official_docs)
- [Examples](https://github.com/zavora-ai/adk-rust/tree/main/examples)

### Guides

- [Introduction](docs/official_docs/introduction.md) - Overview and concepts
- [Quickstart](docs/official_docs/quickstart.md) - Build your first agent
- [LlmAgent](docs/official_docs/agents/llm-agent.md) - Core agent configuration
- [Workflow Agents](docs/official_docs/agents/workflow-agents.md) - Sequential, Parallel, Loop
- [Multi-Agent Systems](docs/official_docs/agents/multi-agent.md) - Agent hierarchies
- [Function Tools](docs/official_docs/tools/function-tools.md) - Custom tools
- [MCP Tools](docs/official_docs/tools/mcp-tools.md) - MCP integration
- [Sessions](docs/official_docs/sessions/sessions.md) - State management
- [Callbacks](docs/official_docs/callbacks/callbacks.md) - Behavior customization
- [Deployment](docs/official_docs/deployment/launcher.md) - Console & server modes

## Examples

The [examples](https://github.com/zavora-ai/adk-rust/tree/main/examples) directory contains working code for:

- **Agents**: LLM, Custom, Sequential, Parallel, Loop, Multi-agent
- **Tools**: Function tools, Google Search, MCP servers
- **Sessions**: State management, conversation history
- **Callbacks**: Logging, guardrails, caching
- **Deployment**: Console, REST server, A2A protocol

## Related Crates

ADK-Rust is modular - use individual crates as needed:

| Crate | Description |
|-------|-------------|
| [adk-core](https://docs.rs/adk-core) | Core traits and types |
| [adk-agent](https://docs.rs/adk-agent) | Agent implementations |
| [adk-model](https://docs.rs/adk-model) | LLM integrations |
| [adk-tool](https://docs.rs/adk-tool) | Tool system |
| [adk-session](https://docs.rs/adk-session) | Session management |
| [adk-artifact](https://docs.rs/adk-artifact) | Artifact storage |
| [adk-runner](https://docs.rs/adk-runner) | Execution runtime |
| [adk-server](https://docs.rs/adk-server) | HTTP server |
| [adk-telemetry](https://docs.rs/adk-telemetry) | Observability |

## License

Apache 2.0

## Related Projects

- [ADK for Python](https://github.com/google/adk-python)
- [ADK for Go](https://github.com/google/adk-go)
- [ADK for Java](https://github.com/google/adk-java)
