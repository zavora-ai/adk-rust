# Output Schema Agent Example

Demonstrates structured-output enforcement on an `LlmAgent`.

## What This Shows

- **`output_type::<T>()`** — declare the expected response type
- **Schema enforcement** — the response is validated against the generated JSON schema
- **Typed extraction** — parse the final event straight into a Rust struct

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set

## Run

```bash
cargo run --manifest-path examples/output_schema_agent/Cargo.toml
```
