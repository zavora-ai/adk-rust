# adk-rust-macros

Proc macros for ADK-Rust — `#[tool]` attribute for zero-boilerplate tool registration.

## Features

- `#[tool]` attribute macro to derive `Tool` trait implementations from annotated functions
- Automatic JSON Schema generation for tool parameters
- Async function support

## Installation

```toml
[dependencies]
adk-rust-macros = "0.4.1"
```

## Usage

```rust,ignore
use adk_rust_macros::tool;

#[tool(description = "Adds two numbers")]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}
```

## License

Apache-2.0
