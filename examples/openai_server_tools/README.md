# OpenAI Server-Side Tools Example

Demonstrates OpenAI's built-in tools (`web_search_preview`) running server-side via the Responses API alongside user-defined function calling tools.

## Setup

```bash
cp .env.example .env
# Edit .env with your OpenAI API key
```

## Run

```bash
cargo run --manifest-path examples/openai_server_tools/Cargo.toml
```

## Scenarios

1. Web search + function tool coexistence — verifies `ServerToolCall` parts flow through when OpenAI executes web search server-side
2. Custom function tool still works — verifies client-side function calling works alongside server-side web search

## How It Works

The web search tool is configured via `GenerateContentConfig` extensions:

```rust
let mut extensions = serde_json::Map::new();
extensions.insert("openai".to_string(), json!({
    "built_in_tools": [
        { "type": "web_search_preview" }
    ]
}));
```

The OpenAI Responses client reads `extensions["openai"]["built_in_tools"]` and appends them to the tools list sent to the API. Server-side tool results appear as `Part::ServerToolCall` in the response stream.
