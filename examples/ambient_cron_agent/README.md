# Ambient Cron Agent Example

Runs an ambient agent on a `CronTrigger`, wrapping a Gemini-powered quote generator with the full trigger lifecycle.

## What This Shows

- **`AmbientAgent`** — wraps an agent with an event-source lifecycle (`start`/`stop`)
- **`CronTrigger`** — fires trigger events on a cron schedule
- **`TriggerHandler`** — receives each trigger event and drives the agent through a `Runner`

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set

## Run

```bash
cargo run --manifest-path examples/ambient_cron_agent/Cargo.toml
```
