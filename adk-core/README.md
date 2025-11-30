# adk-core

Core traits and types for ADK agents, tools, sessions, and events.

[![Crates.io](https://img.shields.io/crates/v/adk-core.svg)](https://crates.io/crates/adk-core)
[![Documentation](https://docs.rs/adk-core/badge.svg)](https://docs.rs/adk-core)
[![License](https://img.shields.io/crates/l/adk-core.svg)](LICENSE)

## Overview

`adk-core` provides the foundational abstractions for the Agent Development Kit (ADK). It defines the core traits and types that all other ADK crates build upon, including:

- **Agent trait** - The fundamental abstraction for all agents
- **Tool traits** - For extending agents with custom capabilities
- **Session/State** - For managing conversation context
- **Event system** - For streaming agent responses
- **Error types** - Unified error handling

This crate is model-agnostic and contains no LLM-specific code.

## Installation

```toml
[dependencies]
adk-core = "0.1"
```

Or use the meta-crate for all components:

```toml
[dependencies]
adk-rust = "0.1"
```

## Core Traits

### Agent

```rust
use adk_core::{Agent, InvocationContext, EventStream};

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> Option<&str>;
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
}
```

### Tool

```rust
use adk_core::{Tool, ToolContext};

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
}
```

## Key Types

- `Event` - Represents agent response events (text, tool calls, etc.)
- `Content` / `Part` - Message content structures
- `Session` / `State` - Conversation state management
- `InvocationContext` - Execution context for agents
- `AdkError` / `Result` - Error handling

## Features

- Zero runtime dependencies on specific LLM providers
- Async-first design with `async-trait`
- Streaming support via `EventStream`
- Flexible state management with typed prefixes

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations
- [adk-tool](https://crates.io/crates/adk-tool) - Tool implementations
- [adk-model](https://crates.io/crates/adk-model) - LLM integrations

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/anthropics/adk-rust) framework for building AI agents in Rust.
