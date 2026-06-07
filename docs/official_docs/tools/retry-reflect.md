# Retry & Reflect

The `adk-retry-reflect` crate provides a plugin that intercepts tool failures, injects reflection prompts into the LLM context, and retries with exponential backoff. This gives agents the ability to self-correct after transient errors or malformed tool calls.

## Overview

When a tool call fails, the default behavior is to return the error to the LLM and let it decide what to do. The Retry & Reflect plugin adds structured recovery:

1. **Intercepts** the tool failure before it reaches the LLM
2. **Injects** a reflection prompt asking the model to analyze what went wrong
3. **Retries** the tool call with corrected arguments
4. **Backs off** exponentially if failures persist
5. **Circuits break** after repeated failures to prevent infinite loops

## Installation

```toml
[dependencies]
adk-retry-reflect = "1.0.0"

# Or via umbrella crate (included in standard tier)
adk-rust = { version = "1.0.0", features = ["standard"] }
```

## Quick Start

```rust
use adk_retry_reflect::RetryReflectPlugin;
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let plugin = RetryReflectPlugin::builder()
    .max_retries(3)
    .initial_backoff_ms(500)
    .backoff_multiplier(2.0)
    .build();

let agent = LlmAgentBuilder::new("resilient_agent")
    .model(model)
    .instruction("You are a helpful assistant with access to external APIs.")
    .tool(Arc::new(flaky_api_tool))
    .plugin(Arc::new(plugin))
    .build()?;
```

## Configuration

```rust
use adk_retry_reflect::{RetryReflectPlugin, RetryReflectConfig};

let plugin = RetryReflectPlugin::builder()
    // Retry settings
    .max_retries(3)                    // Maximum retry attempts [default: 3]
    .initial_backoff_ms(500)           // First retry delay in ms [default: 500]
    .backoff_multiplier(2.0)           // Multiply delay each retry [default: 2.0]
    .max_backoff_ms(30_000)            // Cap delay at this value [default: 30000]

    // Circuit breaker
    .circuit_breaker_threshold(5)      // Open circuit after N failures [default: 5]
    .circuit_breaker_reset_ms(60_000)  // Reset circuit after this duration [default: 60000]

    // Reflection
    .reflection_prompt(                // Custom reflection prompt template
        "The tool '{tool_name}' failed with: {error}. \
         Analyze what went wrong and provide corrected arguments."
    )

    // Scope
    .include_tools(&["api_call", "db_query"])  // Only retry these tools
    .exclude_tools(&["exit_loop"])             // Never retry these tools

    .build();
```

### Configuration Reference

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_retries` | 3 | Maximum retry attempts per tool call |
| `initial_backoff_ms` | 500 | Delay before first retry (milliseconds) |
| `backoff_multiplier` | 2.0 | Multiply delay by this factor each attempt |
| `max_backoff_ms` | 30,000 | Maximum delay cap (milliseconds) |
| `circuit_breaker_threshold` | 5 | Consecutive failures before circuit opens |
| `circuit_breaker_reset_ms` | 60,000 | Time before circuit resets to closed |
| `reflection_prompt` | (built-in) | Template for reflection injection |
| `include_tools` | all | Only retry these tools (empty = all) |
| `exclude_tools` | none | Never retry these tools |

## Circuit Breaker

The circuit breaker prevents infinite retry loops when a tool is persistently failing:

```
Closed (normal) ─── failure count >= threshold ──→ Open (all calls fail fast)
       ↑                                                    │
       └──────── reset_ms elapsed, next call succeeds ──────┘
                              (Half-Open)
```

When the circuit is open:
- Tool calls fail immediately with a circuit-breaker error
- No retries are attempted
- After `circuit_breaker_reset_ms`, the next call is allowed through (half-open)
- If it succeeds, the circuit closes; if it fails, the circuit stays open

## How Reflection Works

When a tool call fails, the plugin injects a reflection prompt into the conversation:

```
[User]: What's the weather in NYC?
[Model]: *calls get_weather({"city": "nyc", "units": "kelvin"})*
[Tool Error]: Invalid units. Supported: celsius, fahrenheit
[Plugin injects]: The tool 'get_weather' failed with: "Invalid units. 
    Supported: celsius, fahrenheit". Analyze what went wrong and provide 
    corrected arguments.
[Model]: *calls get_weather({"city": "NYC", "units": "celsius"})*
[Tool Success]: {"temperature": 22, "condition": "sunny"}
```

The reflection prompt gives the LLM explicit context about the failure so it can self-correct rather than repeating the same mistake.

## When to Use

**Good fit:**
- Tools that call external APIs with transient failures
- Tools where the LLM might provide slightly malformed arguments
- Database queries that can fail due to connection issues
- File operations on networked storage

**Not a good fit:**
- Tools that are deterministically broken (fix the tool instead)
- Long-running tools where retries are expensive
- Tools with side effects that aren't idempotent (e.g., sending emails)
- Control-flow tools like `exit_loop`

## Combining with Other Plugins

Retry & Reflect respects plugin priority ordering:

```rust
use adk_plugin::PluginManager;

let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .plugin(Arc::new(logging_plugin))         // Priority 1 (runs first)
    .plugin(Arc::new(retry_reflect_plugin))   // Priority 2
    .plugin(Arc::new(guardrail_plugin))       // Priority 3 (runs last)
    .build()?;
```

## Observability

The plugin emits tracing spans for retry attempts:

```
WARN adk_retry_reflect: tool call failed, retrying
    tool_name=get_weather attempt=1 max=3 backoff_ms=500
    error="Invalid units"

INFO adk_retry_reflect: retry succeeded
    tool_name=get_weather attempt=2

WARN adk_retry_reflect: circuit breaker opened
    tool_name=broken_api failures=5 reset_ms=60000
```

## Related

- [Plugins](../core/plugins.md) — Plugin system architecture
- [Function Tools](function-tools.md) — Creating tools
- [Evaluation](../evaluation/evaluation.md) — Testing agent resilience

---

**Previous**: [← ACP Tools](acp-tools.md) | **Next**: [Action Nodes →](action-nodes.md)
