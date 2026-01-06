# adk-agent

Agent implementations for ADK-Rust (LLM, Custom, Workflow agents).

[![Crates.io](https://img.shields.io/crates/v/adk-agent.svg)](https://crates.io/crates/adk-agent)
[![Documentation](https://docs.rs/adk-agent/badge.svg)](https://docs.rs/adk-agent)
[![License](https://img.shields.io/crates/l/adk-agent.svg)](LICENSE)

## Overview

`adk-agent` provides ready-to-use agent implementations for [ADK-Rust](https://github.com/zavora-ai/adk-rust):

- **LlmAgent** - Core agent powered by LLM reasoning with tools and callbacks
- **CustomAgent** - Define custom logic without LLM
- **SequentialAgent** - Execute agents in sequence
- **ParallelAgent** - Execute agents concurrently
- **LoopAgent** - Iterate until exit condition or max iterations
- **ConditionalAgent** - Branch based on conditions
- **LlmConditionalAgent** - LLM-powered routing to sub-agents

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

let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

let agent = LlmAgentBuilder::new("assistant")
    .description("Helpful AI assistant")
    .instruction("Be helpful and concise.")
    .model(model)
    .tool(Arc::new(calculator_tool))
    .build()?;
```

### LlmAgentBuilder Methods

| Method | Description |
|--------|-------------|
| `new(name)` | Create builder with agent name |
| `description(desc)` | Set agent description |
| `model(llm)` | Set the LLM model (required) |
| `instruction(text)` | Set static instruction |
| `instruction_provider(fn)` | Set dynamic instruction provider |
| `global_instruction(text)` | Set global instruction (shared across agents) |
| `tool(tool)` | Add a tool |
| `sub_agent(agent)` | Add a sub-agent for transfers |
| `input_schema(json)` | Set input JSON schema |
| `output_schema(json)` | Set output JSON schema |
| `output_key(key)` | Set state key for output |
| `input_guardrails(set)` | Add input validation guardrails |
| `output_guardrails(set)` | Add output validation guardrails |
| `before_callback(fn)` | Add before-agent callback |
| `after_callback(fn)` | Add after-agent callback |
| `before_model_callback(fn)` | Add before-model callback |
| `after_model_callback(fn)` | Add after-model callback |
| `before_tool_callback(fn)` | Add before-tool callback |
| `after_tool_callback(fn)` | Add after-tool callback |
| `build()` | Build the LlmAgent |

### Workflow Agents

```rust
use adk_agent::{SequentialAgent, ParallelAgent, LoopAgent};
use std::sync::Arc;

// Sequential: A -> B -> C
let pipeline = SequentialAgent::new("pipeline", vec![
    agent_a.clone(),
    agent_b.clone(),
    agent_c.clone(),
]);

// Parallel: A, B, C simultaneously
let team = ParallelAgent::new("team", vec![
    analyst_a.clone(),
    analyst_b.clone(),
]);

// Loop: repeat until exit or max iterations
let iterator = LoopAgent::new("iterator", vec![worker.clone()])
    .with_max_iterations(10);
```

### Conditional Agents

```rust
use adk_agent::{ConditionalAgent, LlmConditionalAgent};

// Function-based condition
let conditional = ConditionalAgent::new(
    "router",
    |ctx| async move { ctx.user_content().text().contains("urgent") },
    urgent_agent,
).with_else_agent(normal_agent);

// LLM-powered routing
let llm_router = LlmConditionalAgent::new("smart_router", model)
    .instruction("Route to the appropriate specialist based on the query.")
    .add_route("support", support_agent, "Technical support questions")
    .add_route("sales", sales_agent, "Sales and pricing inquiries")
    .build()?;
```

### Multi-Agent Systems

```rust
// Agent with sub-agents for transfer
let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Route to appropriate specialist. Transfer when needed.")
    .model(model)
    .sub_agent(support_agent)
    .sub_agent(sales_agent)
    .build()?;
```

### Guardrails

```rust
use adk_agent::LlmAgentBuilder;
use adk_guardrail::{GuardrailSet, ContentFilter, PiiRedactor};

let input_guardrails = GuardrailSet::new()
    .with(ContentFilter::harmful_content())
    .with(PiiRedactor::new());

let agent = LlmAgentBuilder::new("safe_assistant")
    .model(model)
    .input_guardrails(input_guardrails)
    .build()?;
```

### Custom Agent

```rust
use adk_agent::CustomAgentBuilder;

let custom = CustomAgentBuilder::new("processor")
    .description("Custom data processor")
    .handler(|ctx| async move {
        // Custom logic here
        Ok(Content::new("model").with_text("Processed!"))
    })
    .build()?;
```

## Features

- Automatic tool execution loop
- Agent transfer between sub-agents
- Streaming event output
- Callback hooks at every stage
- Input/output guardrails
- Schema validation

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Agent` trait
- [adk-model](https://crates.io/crates/adk-model) - LLM integrations
- [adk-tool](https://crates.io/crates/adk-tool) - Tool system
- [adk-guardrail](https://crates.io/crates/adk-guardrail) - Guardrails

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
