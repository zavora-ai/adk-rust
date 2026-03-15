# Resilient Agent Example

Demonstrates the resilience features from the browser production hardening spec.

## Features

- **RetryBudget** — automatic retry on transient tool failures with configurable delay
- **Per-tool retry override** — different retry policies per tool name
- **Circuit Breaker** — disable tools after N consecutive failures within an invocation
- **on_tool_error callback** — provide fallback results for failed tools
- **ToolOutcome** — structured metadata (tool name, success, duration, attempt) in after-tool callbacks
- **Combined resilience** — all features working together

## Requirements

`GOOGLE_API_KEY` environment variable set.

## Running

```bash
cargo run --example resilient_agent
```
