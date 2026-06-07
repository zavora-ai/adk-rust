# Plugins

The `adk-plugin` crate provides a lifecycle hook system for agents. Plugins intercept tool calls, model calls, and execution events without modifying agent code — useful for logging, guardrails, caching, cost tracking, and custom middleware.

## Overview

The plugin system is built around the `EnhancedPlugin` trait. You implement only the hooks you need:

- **before_run / after_run** — Wrap entire agent invocations
- **before_tool / after_tool** — Intercept tool execution (modify args, short-circuit, inspect results)
- **before_model / after_model** — Intercept LLM calls (modify requests, cache responses)
- **on_event** — Observe every event emitted by the agent

Plugins run in a priority-ordered pipeline, enabling composable middleware stacks.

## Installation

```toml
[dependencies]
adk-plugin = "1.0.0"

# Or via umbrella crate (included in standard tier)
adk-rust = { version = "1.0.0", features = ["standard"] }
```

## Quick Start

```rust
use adk_plugin::{EnhancedPlugin, PluginContext, ToolCallInfo, ToolResultInfo};
use adk_core::{Content, Result};
use async_trait::async_trait;
use serde_json::Value;

struct LoggingPlugin;

#[async_trait]
impl EnhancedPlugin for LoggingPlugin {
    fn name(&self) -> &str { "logging" }
    fn priority(&self) -> i32 { 0 }

    async fn before_tool(
        &self,
        ctx: &PluginContext,
        tool_call: &mut ToolCallInfo,
    ) -> Result<Option<Value>> {
        tracing::info!(
            tool = tool_call.name,
            args = %tool_call.args,
            "tool call started"
        );
        Ok(None) // Continue to actual tool execution
    }

    async fn after_tool(
        &self,
        ctx: &PluginContext,
        tool_result: &mut ToolResultInfo,
    ) -> Result<()> {
        tracing::info!(
            tool = tool_result.name,
            duration_ms = tool_result.duration_ms,
            "tool call completed"
        );
        Ok(())
    }
}
```

## EnhancedPlugin Trait

```rust
#[async_trait]
pub trait EnhancedPlugin: Send + Sync {
    /// Unique plugin identifier
    fn name(&self) -> &str;

    /// Execution order (lower = runs first) [default: 0]
    fn priority(&self) -> i32 { 0 }

    /// Called before agent execution starts
    async fn before_run(&self, ctx: &PluginContext) -> Result<()> { Ok(()) }

    /// Called after agent execution completes
    async fn after_run(&self, ctx: &PluginContext) -> Result<()> { Ok(()) }

    /// Called before each tool execution.
    /// Return Some(value) to short-circuit (skip tool, return this value).
    /// Return None to continue with normal execution.
    async fn before_tool(
        &self,
        ctx: &PluginContext,
        tool_call: &mut ToolCallInfo,
    ) -> Result<Option<Value>> { Ok(None) }

    /// Called after each tool execution.
    /// Can modify the result before it's returned to the LLM.
    async fn after_tool(
        &self,
        ctx: &PluginContext,
        tool_result: &mut ToolResultInfo,
    ) -> Result<()> { Ok(()) }

    /// Called before each model (LLM) call.
    /// Can modify the request or short-circuit with a cached response.
    async fn before_model(
        &self,
        ctx: &PluginContext,
        request: &mut ModelCallInfo,
    ) -> Result<Option<Content>> { Ok(None) }

    /// Called after each model call.
    /// Can modify the response before it's processed.
    async fn after_model(
        &self,
        ctx: &PluginContext,
        response: &mut ModelResultInfo,
    ) -> Result<()> { Ok(()) }

    /// Called for every event emitted during execution.
    async fn on_event(
        &self,
        ctx: &PluginContext,
        event: &Event,
    ) -> Result<()> { Ok(()) }
}
```

## Tool-Call Interception

### Argument Modification

Modify tool arguments before execution:

```rust
async fn before_tool(
    &self,
    ctx: &PluginContext,
    tool_call: &mut ToolCallInfo,
) -> Result<Option<Value>> {
    // Inject default values
    if tool_call.name == "search" {
        if tool_call.args.get("limit").is_none() {
            tool_call.args["limit"] = serde_json::json!(10);
        }
    }
    Ok(None) // Continue to tool execution
}
```

### Short-Circuit (Skip Tool Execution)

Return a value directly without calling the tool:

```rust
async fn before_tool(
    &self,
    ctx: &PluginContext,
    tool_call: &mut ToolCallInfo,
) -> Result<Option<Value>> {
    // Check cache
    let cache_key = format!("{}:{}", tool_call.name, tool_call.args);
    if let Some(cached) = self.cache.get(&cache_key).await {
        return Ok(Some(cached)); // Short-circuit: return cached result
    }
    Ok(None) // Cache miss: proceed with tool execution
}
```

### Result Modification

Modify tool results after execution:

```rust
async fn after_tool(
    &self,
    ctx: &PluginContext,
    tool_result: &mut ToolResultInfo,
) -> Result<()> {
    // Redact sensitive data from results
    if let Some(obj) = tool_result.result.as_object_mut() {
        if obj.contains_key("ssn") {
            obj.insert("ssn".into(), serde_json::json!("***-**-****"));
        }
    }
    Ok(())
}
```

## Model-Call Interception

### Request Modification

Modify LLM requests before they're sent:

```rust
async fn before_model(
    &self,
    ctx: &PluginContext,
    request: &mut ModelCallInfo,
) -> Result<Option<Content>> {
    // Add system context to every request
    if let Some(ref mut instruction) = request.system_instruction {
        instruction.push_str("\nAlways respond in JSON format.");
    }
    Ok(None)
}
```

### Response Caching

```rust
async fn before_model(
    &self,
    ctx: &PluginContext,
    request: &mut ModelCallInfo,
) -> Result<Option<Content>> {
    let key = self.hash_request(request);
    if let Some(cached) = self.cache.get(&key).await {
        return Ok(Some(cached)); // Return cached response
    }
    Ok(None)
}

async fn after_model(
    &self,
    ctx: &PluginContext,
    response: &mut ModelResultInfo,
) -> Result<()> {
    // Cache the response for future calls
    let key = self.hash_request(&response.original_request);
    self.cache.set(&key, response.content.clone()).await;
    Ok(())
}
```

## Priority-Based Pipeline

Plugins execute in priority order (lower numbers run first):

```rust
struct AuthPlugin;
impl EnhancedPlugin for AuthPlugin {
    fn name(&self) -> &str { "auth" }
    fn priority(&self) -> i32 { -10 } // Runs first
    // ...
}

struct LogPlugin;
impl EnhancedPlugin for LogPlugin {
    fn name(&self) -> &str { "log" }
    fn priority(&self) -> i32 { 0 } // Runs second
    // ...
}

struct CachePlugin;
impl EnhancedPlugin for CachePlugin {
    fn name(&self) -> &str { "cache" }
    fn priority(&self) -> i32 { 10 } // Runs last
    // ...
}
```

For `before_*` hooks, plugins run lowest-priority-first. For `after_*` hooks, they run in reverse order (highest-priority-first), creating a nested middleware pattern.

## PluginContext — Shared State

`PluginContext` provides shared state accessible across all plugins during an invocation:

```rust
use adk_plugin::PluginContext;

async fn before_tool(
    &self,
    ctx: &PluginContext,
    tool_call: &mut ToolCallInfo,
) -> Result<Option<Value>> {
    // Read shared state
    let call_count: u64 = ctx.get("tool_call_count").unwrap_or(0);

    // Write shared state
    ctx.set("tool_call_count", call_count + 1);

    // Access invocation metadata
    let user_id = ctx.user_id();
    let session_id = ctx.session_id();
    let agent_name = ctx.agent_name();

    Ok(None)
}
```

## Registering Plugins with an Agent

```rust
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("my_agent")
    .model(model)
    .instruction("You are a helpful assistant.")
    .tool(Arc::new(my_tool))
    .plugin(Arc::new(LoggingPlugin))
    .plugin(Arc::new(CachePlugin::new(cache_store)))
    .plugin(Arc::new(CostTrackingPlugin::new()))
    .build()?;
```

## Example: Cost Tracking Plugin

```rust
use adk_plugin::{EnhancedPlugin, PluginContext, ModelResultInfo};
use adk_core::Result;
use std::sync::atomic::{AtomicU64, Ordering};

struct CostPlugin {
    total_tokens: AtomicU64,
}

#[async_trait]
impl EnhancedPlugin for CostPlugin {
    fn name(&self) -> &str { "cost_tracker" }
    fn priority(&self) -> i32 { 100 }

    async fn after_model(
        &self,
        ctx: &PluginContext,
        response: &mut ModelResultInfo,
    ) -> Result<()> {
        if let Some(usage) = &response.usage {
            let tokens = usage.prompt_tokens + usage.completion_tokens;
            self.total_tokens.fetch_add(tokens as u64, Ordering::Relaxed);
            tracing::info!(
                total_tokens = self.total_tokens.load(Ordering::Relaxed),
                "token usage updated"
            );
        }
        Ok(())
    }
}
```

## Related

- [Retry & Reflect](../tools/retry-reflect.md) — Built-in plugin for tool failure recovery
- [Guardrails](../security/guardrails.md) — Input/output validation plugins
- [Telemetry](../observability/telemetry.md) — Observability integration
- [Callbacks](../callbacks/callbacks.md) — Simpler hook system for common cases

---

**Previous**: [← Action Nodes](../tools/action-nodes.md) | **Next**: [Sessions →](../sessions/sessions.md)
