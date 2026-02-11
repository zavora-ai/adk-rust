# ADK-Rust

**Rust Agent Development Kit (ADK-Rust)** - Build AI agents in Rust with modular components for models, tools, memory, realtime voice, and more.

[![Crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
[![Documentation](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

A flexible framework for developing AI agents with simplicity and power. Model-agnostic, deployment-agnostic, optimized for frontier AI models. Includes support for realtime voice agents with OpenAI Realtime API and Gemini Live API.

## Quick Start

**1. Create a new project:**

```bash
cargo new my_agent && cd my_agent
```

**2. Add dependencies:**

```toml
[dependencies]
adk-rust = "0.3.0"
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
```

**3. Set your API key:**

```bash
echo 'GOOGLE_API_KEY=your-key' > .env
```

**4. Write `src/main.rs`:**

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

**5. Run:**

```bash
cargo run
```

## Adding Tools

```rust
let agent = LlmAgentBuilder::new("researcher")
    .instruction("Search the web and summarize findings.")
    .model(Arc::new(model))
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;
```

## Workflow Agents

```rust
// Sequential execution
let pipeline = SequentialAgent::new("pipeline", vec![agent1, agent2, agent3]);

// Parallel execution
let parallel = ParallelAgent::new("analysis", vec![analyst1, analyst2]);

// Loop until condition (max 5 iterations)
let loop_agent = LoopAgent::new("refiner", vec![agent])
    .with_max_iterations(5);
```

## Multi-Agent Systems

```rust
let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Delegate tasks to specialists.")
    .model(model)
    .sub_agent(code_agent)
    .sub_agent(test_agent)
    .build()?;
```

## Realtime Voice Agents

Build voice-enabled AI assistants with bidirectional audio streaming:

```rust
use adk_realtime::{RealtimeAgent, openai::OpenAIRealtimeModel, RealtimeModel};

let model: Arc<dyn RealtimeModel> = Arc::new(
    OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17")
);

let agent = RealtimeAgent::builder("voice_assistant")
    .model(model)
    .instruction("You are a helpful voice assistant.")
    .voice("alloy")
    .server_vad()  // Voice activity detection
    .build()?;
```

Features:
- OpenAI Realtime API & Gemini Live API
- Bidirectional audio (PCM16, G711)
- Server-side VAD
- Real-time tool calling
- Multi-agent handoffs

## Graph-Based Workflows

Build complex workflows using LangGraph-style graph agents:

```rust
use adk_graph::prelude::*;

let agent = GraphAgent::builder("processor")
    .node_fn("fetch", |ctx| async move { /* ... */ })
    .node_fn("transform", |ctx| async move { /* ... */ })
    .edge(START, "fetch")
    .edge("fetch", "transform")
    .edge("transform", END)
    .checkpointer(SqliteCheckpointer::new("state.db").await?)
    .build()?;
```

Features:
- Cyclic graphs for ReAct patterns
- Conditional routing
- State management with reducers
- Checkpointing (memory, SQLite)
- Human-in-the-loop interrupts

## Browser Automation

Give agents web browsing capabilities with 46 tools:

```rust
use adk_browser::{BrowserSession, BrowserToolset, BrowserConfig};
use std::sync::Arc;

let config = BrowserConfig::new().webdriver_url("http://localhost:4444");
let session = Arc::new(BrowserSession::new(config));
let toolset = BrowserToolset::new(session);
let tools = toolset.all_tools();  // 46 browser tools

let mut builder = LlmAgentBuilder::new("web_agent")
    .model(model);

for tool in tools {
    builder = builder.tool(tool);
}

let agent = builder.build()?;
```

Tools include navigation, extraction, forms, screenshots, JavaScript execution, and more.

## Agent Evaluation

Test and validate agent behavior:

```rust
use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria};

let evaluator = Evaluator::new(EvaluationConfig::with_criteria(
    EvaluationCriteria::exact_tools().with_response_similarity(0.8)
));

let report = evaluator.evaluate_file(agent, "tests/agent.test.json").await?;
assert!(report.all_passed());
```

## Deployment

```bash
# Console mode (default)
cargo run

# Server mode
cargo run -- serve --port 8080
```

## Installation Options

```toml
# Full (default)
adk-rust = "0.3.0"

# Minimal
adk-rust = { version = "0.3.0", default-features = false, features = ["minimal"] }

# Custom
adk-rust = { version = "0.3.0", default-features = false, features = ["agents", "gemini", "tools"] }
```

## Documentation

- [API Reference](https://docs.rs/adk-rust)
- [Official Guides](https://github.com/zavora-ai/adk-rust/tree/main/docs/official_docs)
- [Examples](https://github.com/zavora-ai/adk-rust/tree/main/examples)

## License

Apache 2.0
