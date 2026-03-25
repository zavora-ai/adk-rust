# Anthropic Server-Side Tools Example

Demonstrates Anthropic's web search tool (`web_search_20250305`) running server-side alongside user-defined function calling tools.

## Setup

```bash
cp .env.example .env
# Edit .env with your Anthropic API key
```

## Run

```bash
cargo run --manifest-path examples/anthropic_server_tools/Cargo.toml
```

## Scenarios

1. Web search + function tool coexistence — verifies `ServerToolCall`/`ServerToolResponse` parts flow through when Anthropic executes web search server-side
2. Custom function tool still works — verifies client-side function calling works alongside server-side web search

## How It Works

The web search tool is configured via `GenerateContentConfig` extensions:

```rust
let mut extensions = serde_json::Map::new();
extensions.insert("anthropic".to_string(), json!({
    "built_in_tools": [
        { "type": "web_search_20250305", "name": "web_search" }
    ]
}));
```

The Anthropic client reads `extensions["anthropic"]["built_in_tools"]` and appends them to the tools list sent to the API. Server-side tool results appear as `Part::ServerToolCall` and `Part::ServerToolResponse` in the response stream.
