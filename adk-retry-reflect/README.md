# adk-retry-reflect

A Retry & Reflect plugin for ADK-Rust that intercepts tool call failures and injects structured reflection prompts to help the agent self-correct.

## Overview

Instead of immediately propagating errors, the plugin constructs a reflection prompt containing the error details, original arguments, and guidance text, then returns it as a modified tool result so the agent can retry with corrected arguments on the next turn.

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
use std::time::Duration;
use adk_retry_reflect::RetryReflectPluginBuilder;
use adk_plugin::EnhancedPluginManager;

// Create a plugin with exponential backoff
let plugin = RetryReflectPluginBuilder::new()
    .max_retries(3)
    .backoff_exponential(Duration::from_millis(100))
    .max_backoff(Duration::from_secs(10))
    .build()
    .expect("valid configuration");

// Register with EnhancedPluginManager
let manager = EnhancedPluginManager::new(vec![Box::new(plugin)]);
```

## Configuration

| Option | Default | Description |
|--------|---------|-------------|
| `max_retries` | 3 | Default max retries per tool |
| `per_tool_limit` | — | Per-tool retry limit overrides |
| `global_limit` | None | Global retry limit across all tools |
| `backoff_fixed` | — | Fixed delay between retries |
| `backoff_exponential` | — | Exponential backoff with base delay |
| `max_backoff` | 30s | Maximum backoff duration ceiling |
| `allowlist` | — | Only retry these tools |
| `denylist` | — | Don't retry these tools |
| `template` | Built-in | Custom reflection prompt template |
| `priority` | 200 | Plugin execution priority |
| `enable_global_tracking` | disabled | Circuit-breaker threshold |

## Template Placeholders

The reflection template supports these placeholders:

- `{tool_name}` — Name of the failed tool
- `{args}` — JSON-serialized original arguments
- `{error}` — Original error message verbatim
- `{attempt}` — Current attempt number (1-indexed)
- `{max_retries}` — Maximum retries configured for this tool
- `{guidance}` — Optional custom guidance text

## Error Detection

A tool result is considered an error if:

1. It is a JSON object with an `"error"` key at the top level
2. It is a JSON object with `"isError": true`
3. It is a JSON string starting with `"Error:"` or `"error:"`

Two errors pass through unchanged, with no retry suggested: an authorization or approval refusal, and a timeout of a tool whose `effect()` is `NonIdempotent`, which may already have taken effect.

## Tracing Events

The plugin emits structured tracing events:

- `retry_reflect.retry` (info) — When a retry is initiated
- `retry_reflect.exhausted` (warn) — When retry limit is exceeded
- `retry_reflect.circuit_broken` (warn) — When global threshold is exceeded
- `retry_reflect.skipped_authorization_denial` (info) — When a refusal passes through
- `retry_reflect.skipped_non_idempotent_timeout` (warn) — When a non-idempotent timeout passes through

## License

Apache-2.0
