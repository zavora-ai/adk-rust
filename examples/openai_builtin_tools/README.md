# OpenAI Built-In Tools Example

Demonstrates configuring **OpenAI-hosted built-in tools** through the Responses API. Built-in tools run entirely on OpenAI's infrastructure — you configure them via request extensions and receive results in `provider_metadata`. No external services or additional API keys are needed beyond your OpenAI key.

This example covers two built-in tools:
- **Image Generation** — generates images server-side with configurable size and quality
- **Tool Search** — intelligently selects which function tools to invoke from a larger set of definitions

## Prerequisites

- Rust 1.85.0+
- `OPENAI_API_KEY` environment variable set with a valid OpenAI API key

## Running

```bash
cargo run --manifest-path examples/openai_builtin_tools/Cargo.toml
```

## What It Does

1. **Image Generation** — Configures the `image_generation` built-in tool with `size: "1024x1024"` and `quality: "low"` via `extensions["openai"]["built_in_tools"]`, sends a descriptive prompt, and prints the generation result from `provider_metadata`
2. **Tool Search** — Defines four function tools (get_weather, search_flights, book_hotel, translate_text), configures the `tool_search` built-in tool via extensions, sends a travel planning prompt, and prints which tools the model selected from `provider_metadata`

## How Built-In Tools Work

Built-in tools are configured through the `extensions["openai"]["built_in_tools"]` field in `GenerateContentConfig`. Each tool is a JSON object with a `"type"` field and optional parameters:

```rust
gen_config.extensions.insert(
    "openai".to_string(),
    serde_json::json!({
        "built_in_tools": [
            { "type": "image_generation", "size": "1024x1024", "quality": "high" }
        ]
    }),
);
```

The model invokes these tools server-side and returns results in the response's `provider_metadata`.

## Related

- [`adk-model/src/openai/responses_convert.rs`](../../adk-model/src/openai/responses_convert.rs) — Built-in tool extension processing
- [`adk-model/src/openai/responses_client.rs`](../../adk-model/src/openai/responses_client.rs) — OpenAI Responses API client
- [OpenAI Responses API — Tools](https://platform.openai.com/docs/api-reference/responses/create#responses-create-tools) — Official documentation
- [OpenAI Image Generation](https://platform.openai.com/docs/guides/image-generation) — Image generation guide
