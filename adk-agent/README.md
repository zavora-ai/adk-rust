# adk-agent

Agent implementations for ADK (LLM, Custom, Workflow agents).

[![Crates.io](https://img.shields.io/crates/v/adk-agent.svg)](https://crates.io/crates/adk-agent)
[![Documentation](https://docs.rs/adk-agent/badge.svg)](https://docs.rs/adk-agent)
[![License](https://img.shields.io/crates/l/adk-agent.svg)](LICENSE)

## Overview

`adk-agent` provides ready-to-use agent implementations for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **LlmAgent** - Core agent powered by LLM reasoning
- **CustomAgent** - Define custom logic without LLM
- **SequentialAgent** - Execute agents in sequence
- **ParallelAgent** - Execute agents concurrently
- **LoopAgent** - Iterate until exit condition
- **ConditionalAgent** - Branch based on conditions

## Installation

```toml
[dependencies]
adk-agent = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["agents"] }
```

## Quick Start

### LLM Agent

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use std::sync::Arc;

let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

let agent = LlmAgentBuilder::new("assistant")
    .description("Helpful AI assistant")
    .instruction("Be helpful and concise.")
    .model(Arc::new(model))
    .tool(Arc::new(calculator_tool))
    .build()?;
```

### Workflow Agents

```rust
use adk_agent::{SequentialAgent, ParallelAgent, LoopAgent};

// Sequential: A -> B -> C
let seq = SequentialAgent::new("pipeline", vec![
    Arc::new(agent_a),
    Arc::new(agent_b),
    Arc::new(agent_c),
]);

// Parallel: A, B, C simultaneously
let par = ParallelAgent::new("team", vec![
    Arc::new(analyst_a),
    Arc::new(analyst_b),
]);

// Loop: repeat until exit
let loop_agent = LoopAgent::new("iterator", Arc::new(worker), 10);
```

### Multi-Agent Systems

```rust
// Router with sub-agents
let router = LlmAgentBuilder::new("router")
    .instruction("Route to appropriate specialist")
    .sub_agent(Arc::new(support_agent))
    .sub_agent(Arc::new(sales_agent))
    .build()?;
```

## Features

- Automatic tool execution loop
- Agent transfer between sub-agents
- Streaming event output
- Callback hooks at every stage
- Long-running tool support

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Agent` trait
- [adk-model](https://crates.io/crates/adk-model) - LLM integrations
- [adk-tool](https://crates.io/crates/adk-tool) - Tool system

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
