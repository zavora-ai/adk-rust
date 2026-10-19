# adk-retry-reflect

A Retry & Reflect plugin for ADK-Rust that intercepts tool call failures and injects structured reflection prompts to help the agent self-correct.

[![Crates.io](https://img.shields.io/crates/v/adk-retry-reflect.svg)](https://crates.io/crates/adk-retry-reflect)
[![Documentation](https://docs.rs/adk-retry-reflect/badge.svg)](https://docs.rs/adk-retry-reflect)
[![License](https://img.shields.io/crates/l/adk-retry-reflect.svg)](../LICENSE)

## Overview

`RetryReflectPlugin` is an `adk_plugin::EnhancedPlugin`. In its `after_tool_call` hook it turns a failed tool result into a reflection prompt containing the error, the original arguments, and guidance text, so the agent can retry with corrected arguments on the next turn.

## Installation

```toml
[dependencies]
adk-retry-reflect = "3.0.0"
adk-agent = { version = "3.0.0", features = ["enhanced-plugins"] }
```

## Features

- **Per-tool retry limits** with configurable defaults and per-tool overrides
- **Global retry limit** to prevent runaway retry loops
- **Configurable backoff** (none, fixed, or exponential with ceiling)
- **Tool eligibility filtering** via allowlist or denylist
- **Customizable reflection templates** with placeholder substitution
- **Global failure tracking** for circuit-breaker patterns
- **Structured tracing events** for monitoring retry behavior

## Quick Start

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Tool};
use adk_retry_reflect::RetryReflectPluginBuilder;

fn agent(model: Arc<dyn Llm>, tool: Arc<dyn Tool>) -> Result<LlmAgent, Box<dyn std::error::Error>> {
    // A plugin with exponential backoff
    let plugin = RetryReflectPluginBuilder::new()
        .max_retries(3)
        .backoff_exponential(Duration::from_millis(100))
        .max_backoff(Duration::from_secs(10))
        .build()?;

    // Register it on an agent (adk-agent's `enhanced-plugins` feature)
    Ok(LlmAgentBuilder::new("assistant")
        .model(model)
        .tool(tool)
        .enhanced_plugin(Arc::new(plugin))
        .build()?)
}
```

`adk_plugin::EnhancedPluginManager::new(vec![Arc::new(plugin)])` runs it outside an agent.

## Configuration

| Option | Default | Description |
|--------|---------|-------------|
| `max_retries` | 3 | Default max retries per tool call, per invocation |
| `per_tool_limit` | — | Per-tool retry limit overrides |
| `global_limit` | None | Retry limit across all tools in one invocation |
| `backoff_fixed` | — | Fixed delay between retries |
| `backoff_exponential` | — | Exponential backoff with base delay |
| `max_backoff` | 30s | Maximum backoff duration ceiling |
| `allowlist` | — | Only retry these tools |
| `denylist` | — | Don't retry these tools |
| `template` | Built-in | Custom reflection prompt template |
| `priority` | 200 | Plugin execution priority |
| `enable_global_tracking` | disabled | Circuit-breaker threshold: failures of one tool, across invocations, since its last success |
| `record_payloads` | false | Whether log lines carry the tool's error text; set it to the run's `RunConfig::record_payloads` |

## Retry Counts

One plugin serves every user and session. Retry counts are kept per invocation, keyed by
tool and arguments, so one invocation's failures never use up another's budget:

| Event | Effect on the counts |
|-------|----------------------|
| A tool call fails | Its tool-and-arguments count in this invocation rises by one |
| A tool call succeeds | That tool's counts in this invocation are cleared, and with global tracking its circuit closes |
| More than 1,024 invocations are tracked | The oldest invocation's counts are forgotten |

## Template Placeholders

The reflection template supports these placeholders:

- `{tool_name}` — Name of the failed tool
- `{args}` — JSON-serialized original arguments
- `{error}` — The `"error"` text of the tool result, as the model receives it
- `{attempt}` — Current attempt number (1-indexed)
- `{max_retries}` — Maximum retries configured for this tool
- `{guidance}` — Optional custom guidance text

## Error Detection

A tool result is considered an error if:

1. It is a JSON object with an `"error"` key at the top level
2. It is a JSON object with `"isError": true`
3. It is a JSON string starting with `"Error:"` or `"error:"`

Two errors pass through unchanged, with no retry suggested: an authorization or approval refusal, and a timeout of a tool whose `effect()` is `NonIdempotent`, which may already have taken effect. `LlmAgent` sends the model a tool error's category and code but not its message, so the plugin matches both on the full message in `ctx.tool_outcome()` when the agent provides one, and on the result's `"error"` text otherwise.

## Tracing Events

The plugin emits structured tracing events. Each names the error by its component,
category, and code (`error`); its text (`error.message`) is logged only under
`record_payloads(true)`:

- `retry_reflect.retry` (info) — When a retry is initiated
- `retry_reflect.exhausted` (warn) — When a tool's retry limit is exceeded
- `retry_reflect.global_limit_exhausted` (warn) — When the invocation's global limit is exceeded
- `retry_reflect.circuit_broken` (warn) — When global threshold is exceeded
- `retry_reflect.skipped_authorization_denial` (info) — When a refusal passes through
- `retry_reflect.skipped_non_idempotent_timeout` (warn) — When a non-idempotent timeout passes through

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `RetryTracker::increment`, `get`, and `total` take the invocation ID and tool name; `reset_tool` and `tracked_invocations` are new; at most 1,024 invocations are tracked | Pass the invocation ID |
| `GlobalRetryTracker::record_success` clears a tool's failures | — |
| Authorization and approval refusals, and timeouts of a `NonIdempotent` tool, pass through unchanged | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
