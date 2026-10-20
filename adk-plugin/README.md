# adk-plugin

Plugin system for ADK-Rust agents.

[![Crates.io](https://img.shields.io/crates/v/adk-plugin.svg)](https://crates.io/crates/adk-plugin)
[![Documentation](https://docs.rs/adk-plugin/badge.svg)](https://docs.rs/adk-plugin)
[![License](https://img.shields.io/crates/l/adk-plugin.svg)](../LICENSE)

## Overview

`adk-plugin` extends agent behavior through callbacks at lifecycle points. It has two plugin models:

| Model | Types | Installed on |
|-------|-------|--------------|
| Run plugins | `Plugin`, `PluginConfig`, `PluginBuilder`, `PluginManager` | `Runner::builder().plugin_manager(...)` — run, user-message, event, agent, model, and tool callbacks for every agent the run reaches |
| Enhanced plugins | `EnhancedPlugin`, `EnhancedPluginManager`, `PluginContext`, `AdaptedPlugin` | `LlmAgentBuilder::enhanced_plugin(...)` (`adk-agent`'s `enhanced-plugins` feature) — priority-ordered tool and model call interception with shared typed state |

## Installation

```toml
[dependencies]
adk-plugin = "3.0.0"
```

## Quick Start

```rust
use std::sync::Arc;

use adk_core::{Agent, BeforeModelResult, Result};
use adk_plugin::{Plugin, PluginBuilder, PluginConfig, PluginManager};
use adk_runner::Runner;
use adk_session::SessionService;

fn runner(agent: Arc<dyn Agent>, sessions: Arc<dyn SessionService>) -> Result<Runner> {
    // A logging plugin built with the builder
    let logging_plugin = PluginBuilder::new("logging")
        .on_user_message(Box::new(|_ctx, content| {
            Box::pin(async move {
                tracing::info!(parts = content.parts.len(), "user message");
                Ok(None) // keep the message
            })
        }))
        .on_event(Box::new(|_ctx, event| {
            Box::pin(async move {
                tracing::info!(event.id = %event.id, "event");
                Ok(None) // keep the event
            })
        }))
        .build();

    // A plugin built from a config
    let cache_plugin = Plugin::new(PluginConfig {
        name: "cache".to_string(),
        before_model: Some(Box::new(|_ctx, request| {
            Box::pin(async move { Ok(BeforeModelResult::Continue(request)) })
        })),
        ..Default::default()
    });

    let manager = PluginManager::new(vec![logging_plugin, cache_plugin]);
    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(sessions)
        .plugin_manager(Arc::new(manager))
        .build()
}
```

The runner calls the run, user-message, and event callbacks itself, and installs the manager on the run's `RunConfig::invocation_hooks`, so `LlmAgent` and `CodeActAgent` run its agent, model, and tool callbacks ahead of their own for every agent the run reaches. A plugin callback that returns a value short-circuits the agent's callbacks of that kind. For callbacks that can modify data, the first plugin to return a modification wins.

## Callback Types

### Run Lifecycle

| Callback | Description |
|----------|-------------|
| `on_user_message` | Called when a user message is received; can modify it |
| `on_event` | Called for each event the run yields, including a `before_run` early exit, a cancelled run's answers to its calls in flight, and a budget stop event; can modify it |
| `before_run` | Called before the run starts; a returned content ends the run early |
| `after_run` | Called once the run ends, however it ends — including a consumer that drops the stream |
| `close_fn` | Called by `PluginManager::close`, which `Runner::close` runs (bounded by `PluginManagerConfig::close_timeout`) |

### Agent Callbacks

| Callback | Description |
|----------|-------------|
| `before_agent` | Called before an agent runs |
| `after_agent` | Called after an agent runs, including after a transfer, a turn-ending tool result, and a cancellation |

### Model Callbacks

| Callback | Description |
|----------|-------------|
| `before_model` | Called before a model call; can modify the request or skip the call |
| `after_model` | Called after a model call; can modify the response |
| `on_model_error` | Called when a model call or its stream fails; a returned response answers the turn |

### Tool Callbacks

| Callback | Description |
|----------|-------------|
| `before_tool` | Called before a tool executes; a returned content skips the tool |
| `after_tool` | Called after a tool executes |
| `on_tool_error` | Called when a tool fails; can provide a fallback result |

## Example: Caching Plugin

```rust
use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{BeforeModelResult, LlmResponse};
use adk_plugin::{Plugin, PluginConfig};
use tokio::sync::RwLock;

fn caching_plugin() -> Plugin {
    let cache: Arc<RwLock<HashMap<String, LlmResponse>>> = Arc::new(RwLock::new(HashMap::new()));
    let cache_read = cache.clone();

    Plugin::new(PluginConfig {
        name: "response-cache".to_string(),
        before_model: Some(Box::new(move |_ctx, request| {
            let cache = cache_read.clone();
            Box::pin(async move {
                let key = format!("{:?}", request.contents);
                if let Some(cached) = cache.read().await.get(&key) {
                    return Ok(BeforeModelResult::Skip(cached.clone()));
                }
                Ok(BeforeModelResult::Continue(request))
            })
        })),
        after_model: Some(Box::new(move |_ctx, _response| {
            Box::pin(async move {
                // Store the response under its request key here.
                Ok(None)
            })
        })),
        ..Default::default()
    })
}
```

## Example: Metrics Plugin

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_plugin::{Plugin, PluginConfig, collect_metrics};

fn metrics_plugin() -> Plugin {
    let run_count = Arc::new(AtomicU64::new(0));
    let run_count_start = run_count.clone();
    let run_count_end = run_count.clone();

    let (before_run, after_run) = collect_metrics(
        move || {
            run_count_start.fetch_add(1, Ordering::SeqCst);
        },
        move || {
            tracing::info!(runs = run_count_end.load(Ordering::SeqCst), "run finished");
        },
    );

    Plugin::new(PluginConfig {
        name: "metrics".to_string(),
        before_run: Some(before_run),
        after_run: Some(after_run),
        ..Default::default()
    })
}
```

## Enhanced Plugins

`EnhancedPlugin` implements only the hooks it needs; `priority()` orders plugins (lower first, default 100). `before_tool_call` returns `BeforeToolCallResult::Continue(args)` (possibly rewritten) or `ShortCircuit(result)`, and `after_tool_call` may rewrite the result; `before_model_call` and `after_model_call` do the same for model calls. `PluginContext` holds typed state shared across the hooks of one invocation; `PluginContext::shared()` holds state every invocation of the agent shares. `AdaptedPlugin` runs a run plugin's tool and model callbacks as an enhanced plugin; a `before_tool` that returns content short-circuits the call with it.

```rust
use std::sync::Arc;

use adk_core::{CallbackContext, Result, Tool};
use adk_plugin::{BeforeToolCallResult, EnhancedPlugin, PluginContext};
use async_trait::async_trait;
use serde_json::{Value, json};

struct BlockDeletes;

#[async_trait]
impl EnhancedPlugin for BlockDeletes {
    fn name(&self) -> &str {
        "block-deletes"
    }

    async fn before_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        if tool.name().starts_with("delete_") {
            return Ok(BeforeToolCallResult::ShortCircuit(json!({ "error": "deletes are disabled" })));
        }
        Ok(BeforeToolCallResult::Continue(args))
    }
}
```

Enhanced plugins run before the governed authorization path, so the tool policy, guardrails, and confirmation see the arguments a plugin rewrote.

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| A runner's `PluginManager` agent, model, and tool callbacks now run (through `RunConfig::invocation_hooks`) | Callbacks that did nothing before now take effect |
| `on_model_error` runs for a failed model call or stream, and its response answers the turn | — |
| `after_run` runs once, after the run's last event, however the run ends | — |
| An `AdaptedPlugin`'s legacy `before_tool` refusal stops the tool | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — core traits and types
- [adk-agent](https://crates.io/crates/adk-agent) — agent implementations
- [adk-runner](https://crates.io/crates/adk-runner) — execution runtime
- [adk-retry-reflect](https://crates.io/crates/adk-retry-reflect) — a retry-and-reflect plugin

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
