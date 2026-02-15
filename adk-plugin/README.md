# adk-plugin

Plugin system for ADK-Rust agents.

[![Crates.io](https://img.shields.io/crates/v/adk-plugin.svg)](https://crates.io/crates/adk-plugin)
[![Documentation](https://docs.rs/adk-plugin/badge.svg)](https://docs.rs/adk-plugin)
[![License](https://img.shields.io/crates/l/adk-plugin.svg)](LICENSE)

## Overview

`adk-plugin` provides a plugin architecture for extending ADK agent behavior through callbacks at various lifecycle points. 

## Features

- **Run lifecycle hooks**: Before/after the entire agent run
- **User message processing**: Modify or inspect user input
- **Event processing**: Modify or inspect agent events
- **Agent callbacks**: Before/after agent execution
- **Model callbacks**: Before/after LLM calls, error handling
- **Tool callbacks**: Before/after tool execution, error handling

## Installation

```toml
[dependencies]
adk-plugin = "0.3.1"
```

## Quick Start

```rust
use adk_plugin::{Plugin, PluginConfig, PluginManager, PluginBuilder};

// Create a logging plugin using the builder
let logging_plugin = PluginBuilder::new("logging")
    .on_user_message(Box::new(|ctx, content| {
        Box::pin(async move {
            println!("User: {:?}", content);
            Ok(None) // Don't modify
        })
    }))
    .on_event(Box::new(|ctx, event| {
        Box::pin(async move {
            println!("Event: {}", event.id);
            Ok(None) // Don't modify
        })
    }))
    .build();

// Create a caching plugin
let cache_plugin = Plugin::new(PluginConfig {
    name: "cache".to_string(),
    before_model: Some(Box::new(|ctx, request| {
        Box::pin(async move {
            // Check cache...
            Ok(BeforeModelResult::Continue(request))
        })
    })),
    ..Default::default()
});

// Create plugin manager
let manager = PluginManager::new(vec![logging_plugin, cache_plugin]);

// Use with Runner
```

## Callback Types

### Run Lifecycle

| Callback | Description |
|----------|-------------|
| `on_user_message` | Called when user message received, can modify |
| `on_event` | Called for each event, can modify |
| `before_run` | Called before run starts, can skip run |
| `after_run` | Called after run completes (cleanup) |

### Agent Callbacks

| Callback | Description |
|----------|-------------|
| `before_agent` | Called before agent execution |
| `after_agent` | Called after agent execution |

### Model Callbacks

| Callback | Description |
|----------|-------------|
| `before_model` | Called before LLM call, can modify request or skip |
| `after_model` | Called after LLM call, can modify response |
| `on_model_error` | Called on LLM error, can provide fallback |

### Tool Callbacks

| Callback | Description |
|----------|-------------|
| `before_tool` | Called before tool execution |
| `after_tool` | Called after tool execution |
| `on_tool_error` | Called on tool error, can provide fallback |

## Example: Caching Plugin

```rust
use adk_plugin::{Plugin, PluginConfig};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// Simple in-memory cache
let cache = Arc::new(RwLock::new(HashMap::new()));
let cache_read = cache.clone();
let cache_write = cache.clone();

let caching_plugin = Plugin::new(PluginConfig {
    name: "response-cache".to_string(),
    before_model: Some(Box::new(move |ctx, request| {
        let cache = cache_read.clone();
        Box::pin(async move {
            let key = format!("{:?}", request.contents);
            let guard = cache.read().await;
            if let Some(cached) = guard.get(&key) {
                return Ok(BeforeModelResult::Skip(cached.clone()));
            }
            Ok(BeforeModelResult::Continue(request))
        })
    })),
    after_model: Some(Box::new(move |ctx, response| {
        let cache = cache_write.clone();
        Box::pin(async move {
            // Store in cache (simplified)
            Ok(None)
        })
    })),
    ..Default::default()
});
```

## Example: Metrics Plugin

```rust
use adk_plugin::{Plugin, PluginConfig, collect_metrics};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

let run_count = Arc::new(AtomicU64::new(0));
let run_count_start = run_count.clone();
let run_count_end = run_count.clone();

let (before_run, after_run) = collect_metrics(
    move || { run_count_start.fetch_add(1, Ordering::SeqCst); },
    move || { println!("Runs: {}", run_count_end.load(Ordering::SeqCst)); },
);

let metrics_plugin = Plugin::new(PluginConfig {
    name: "metrics".to_string(),
    before_run: Some(before_run),
    after_run: Some(after_run),
    ..Default::default()
});
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations
- [adk-runner](https://crates.io/crates/adk-runner) - Agent execution runtime

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
