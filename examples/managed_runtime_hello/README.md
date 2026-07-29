# Managed Agent Runtime — Hello World Example

End-to-end smoke test of the managed agent runtime against a scripted LLM, so it needs no API key.

## What This Shows

- **`DefaultManagedAgentRuntime`** — register an agent, start a session, stream events
- **`ScriptedLlm`** — deterministic test double standing in for a provider
- **Session lifecycle** — send an event, observe status transitions, archive

## Prerequisites

- **Rust 1.95+** (edition 2024)
- Built with the adk-rust `managed-runtime` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/managed_runtime_hello/Cargo.toml
```
