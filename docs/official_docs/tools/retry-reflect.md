# Retry & Reflect

The `adk-retry-reflect` crate provides an enhanced plugin that turns a failed tool result into a reflection prompt, so the model can analyze the failure and call the tool again with corrected arguments.

## Overview

When a tool call fails, the default behavior is to return the error to the LLM and let it decide what to do. The Retry & Reflect plugin adds structured recovery in its `after_tool_call` hook:

1. **Detects** a failed result: an object with an `"error"` key, an object with `"isError": true`, or a string starting with `Error:` or `error:`
2. **Skips** failures that a retry cannot fix (see [Failures passed through](#failures-passed-through))
3. **Counts** the failure against per-tool and per-invocation limits
4. **Waits** for the configured backoff, if any
5. **Replaces** the failed result with `{"reflection": "<rendered template>"}`

The plugin never re-runs the tool itself. The model reads the reflection and decides whether to call the tool again.

## Installation

```toml
[dependencies]
adk-retry-reflect = "3.0.0"
adk-agent = { version = "3.0.0", features = ["enhanced-plugins"] }
adk-plugin = "3.0.0"
```

The umbrella crate does not re-export `adk-retry-reflect`; its `plugin` feature enables `adk-agent`'s `enhanced-plugins`, which `LlmAgentBuilder::enhanced_plugin` needs.

## Quick Start

```rust
use adk_agent::LlmAgentBuilder;
use adk_plugin::EnhancedPlugin;
use adk_retry_reflect::RetryReflectPluginBuilder;
use std::sync::Arc;
use std::time::Duration;

let plugin = RetryReflectPluginBuilder::new()
    .max_retries(3)
    .backoff_exponential(Duration::from_millis(500))
    .build()?;

let agent = LlmAgentBuilder::new("resilient_agent")
    .model(model)
    .instruction("You are a helpful assistant with access to external APIs.")
    .tool(Arc::new(flaky_api_tool))
    .enhanced_plugin(Arc::new(plugin) as Arc<dyn EnhancedPlugin>)
    .build()?;
```

## Configuration

```rust
use adk_retry_reflect::RetryReflectPluginBuilder;
use std::time::Duration;

let plugin = RetryReflectPluginBuilder::new()
    // Retry limits
    .max_retries(3)                    // Reflections per tool and arguments [default: 3]
    .per_tool_limit("db_query", 5)     // Override for one tool
    .global_limit(10)                  // Reflections per invocation [default: unlimited]

    // Backoff before the reflection is returned [default: none]
    .backoff_exponential(Duration::from_millis(500))
    .max_backoff(Duration::from_secs(30)) // [default: 30 s]

    // Scope: an allowlist or a denylist; the last call wins
    .allowlist(["api_call", "db_query"])

    // Reflection text
    .template("The tool '{tool_name}' failed with: {error}. Analyze what went wrong and call it again with corrected arguments.")

    // Circuit breaker across invocations [default: off]
    .enable_global_tracking(5)

    .build()?;
```

`build()` returns a `RetryReflectError::InvalidMaxRetries` error when `max_retries` is 0.

### Configuration Reference

| Method | Default | Description |
|--------|---------|-------------|
| `max_retries(n)` | 3 | Reflections for one tool and argument set in one invocation |
| `per_tool_limit(tool, n)` | none | Overrides `max_retries` for `tool` |
| `global_limit(n)` | unlimited | Reflections across all tools in one invocation |
| `backoff_fixed(delay)` | no backoff | Waits `delay` before each reflection |
| `backoff_exponential(base)` | no backoff | Waits `base * 2^(attempt - 1)` |
| `max_backoff(ceiling)` | 30 s | Upper bound on any backoff |
| `allowlist(tools)` / `denylist(tools)` | every tool | Which tools are reflected; the last call wins |
| `template(text)` | built-in | Reflection text with `{tool_name}`, `{args}`, `{error}`, `{attempt}`, `{max_retries}` and `{guidance}` placeholders; `{guidance}` renders empty |
| `priority(n)` | 200 | Enhanced-plugin order; lower values run first |
| `enable_global_tracking(threshold)` | off | Circuit breaker across invocations; see below |

## Retry Counts

One plugin instance serves every user and session of an agent. Retry counts are kept per
invocation, keyed by tool and arguments, so one invocation's failures never use up another's
budget:

| Event | Effect |
|-------|--------|
| A tool call fails | A reflection is added while fewer than `max_retries` reflections were added for that tool and argument set in this invocation, and fewer than `global_limit` across the invocation; each reflection raises both counts by one |
| A tool call succeeds | That tool's counts in this invocation are cleared, so its next failure is reflected again |
| More than 1,024 invocations are tracked | The oldest invocation's counts are forgotten |

## Circuit Breaker

`enable_global_tracking(threshold)` adds a circuit breaker across invocations for a tool
that keeps failing:

```
Closed (reflections) ─── threshold failures since the last success ──→ Open (errors pass through)
       ↑                                                                     │
       └──────────────────────── the tool succeeds once ─────────────────────┘
```

When the circuit is open, the tool's errors reach the model unchanged, with no reflection.
The tool still runs, so the first call that succeeds closes the circuit. There is no
time-based reset.

## How Reflection Works

When a tool call fails, the plugin replaces the tool's result with the rendered template. With
the default template:

```
[RETRY GUIDANCE - Attempt 1/3]

Tool: get_weather
Error: <the result's "error" text>

Original Arguments:
{
  "city": "nyc",
  "units": "kelvin"
}

Please analyze the error above and retry the tool call with corrected arguments.
```

`{error}` is the result's `"error"` text — what the model would have received. For a tool
that failed with an `AdkError`, `LlmAgent` writes that text as
`tool 'get_weather' failed: <component>.<category> (<code>)`, without the error's message,
unless the agent sets `expose_tool_error_messages(true)`. A tool that returns
`Ok(json!({"error": "Invalid units. Supported: celsius, fahrenheit"}))` puts its own text in
the reflection. See [Error Handling](function-tools.md#error-handling).

### Failures passed through

Two failures pass through unchanged, with no retry suggested, whatever the tool filter:

| Failure | Why |
|---------|-----|
| An authorization or approval refusal (missing scope, access denied, denied by confirmation policy) | Repeating the call is refused again |
| A timeout of a tool whose `effect()` is `NonIdempotent` | The call may have taken effect, so repeating it could repeat a payment or other side effect |

The plugin recognizes both from the full error message in `ctx.tool_outcome()`, which keeps
the message the model does not see. See [Tool Effects](tool-effects.md).

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

Enhanced plugins run their hooks in ascending `priority()` order; plugins with the same
priority run in registration order. Retry & Reflect defaults to 200:

```rust
use adk_plugin::EnhancedPlugin;

let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .enhanced_plugins(vec![
        Arc::new(logging_plugin) as Arc<dyn EnhancedPlugin>,
        Arc::new(retry_reflect_plugin),
        Arc::new(guardrail_plugin),
    ])
    .build()?;
```

## Observability

The plugin emits `tracing` events with the tool name and the result's error text:

| Event | Level | When |
|-------|-------|------|
| `retry_reflect.retry` | `info` | A reflection replaced a failed result (`attempt`, `max_retries`, `backoff_ms`) |
| `retry_reflect.exhausted` | `warn` | The tool-and-arguments count reached its limit |
| `retry_reflect.global_limit_exhausted` | `warn` | The invocation reached `global_limit` |
| `retry_reflect.circuit_broken` | `warn` | The tool's circuit is open |
| `retry_reflect.skipped_authorization_denial` | `info` | A refusal passed through |
| `retry_reflect.skipped_non_idempotent_timeout` | `warn` | A non-idempotent timeout reported as an `"error"` result passed through. `LlmAgent` answers such a timeout with an `outcome_unknown` response, which carries no `"error"` key, so it passes through without this event |

## Related

- [Plugins](../core/plugins.md) — Plugin system architecture
- [Function Tools](function-tools.md) — Creating tools
- [Evaluation](../evaluation/evaluation.md) — Testing agent resilience
- `examples/retry_reflect` — The plugin on a live agent

---

**Previous**: [← ACP Tools](acp-tools.md) | **Next**: [Action Nodes →](action-nodes.md)
