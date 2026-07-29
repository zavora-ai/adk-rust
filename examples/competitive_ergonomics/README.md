# Ergonomics and Encrypted Sessions Example

Validation example for the convenience APIs and encrypted session storage.

## What This Shows

- **`provider_from_env()`** — provider auto-detection across the compiled provider features
- **`Runner::run_str()`** — string-based convenience entry point
- **`EncryptedSession`** — AES-256-GCM session storage with key rotation

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`One of ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY`** environment variable set

## Run

```bash
cargo run --manifest-path examples/competitive_ergonomics/Cargo.toml
```
