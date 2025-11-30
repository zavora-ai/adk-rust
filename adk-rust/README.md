# ADK-Rust

**Agent Development Kit for Rust** - Build AI agents with simplicity and power.

[![Crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
[![Documentation](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

A flexible framework for developing AI agents. Model-agnostic, deployment-agnostic, optimized for Gemini.

## Quick Start

**1. Create a new project:**

```bash
cargo new my_agent && cd my_agent
```

**2. Add dependencies:**

```toml
[dependencies]
adk-rust = "0.1"
tokio = { version = "1.40", features = ["full"] }
dotenv = "0.15"
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
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

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

// Loop until condition
let loop_agent = LoopAgent::new("refiner", agent, 5);
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
adk-rust = "0.1"

# Minimal
adk-rust = { version = "0.1", default-features = false, features = ["minimal"] }

# Custom
adk-rust = { version = "0.1", default-features = false, features = ["agents", "gemini", "tools"] }
```

## Documentation

- [API Reference](https://docs.rs/adk-rust)
- [Official Guides](https://github.com/zavora-ai/adk-rust/tree/main/docs/official_docs)
- [Examples](https://github.com/zavora-ai/adk-rust/tree/main/examples)

## License

Apache 2.0
