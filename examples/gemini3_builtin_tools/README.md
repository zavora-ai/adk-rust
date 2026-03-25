# Gemini 3 Built-in Tools Example

Demonstrates the fix for Gemini 3 models using built-in tools (`google_search`, `url_context`) alongside user-defined function calling tools.

Before this fix, Gemini 3 would silently truncate responses when both tool types were present because the `include_server_side_tool_invocations` flag was not set.

## Scenarios

1. **is_builtin verification** — confirms `GoogleSearchTool.is_builtin()` returns `true` and regular `FunctionTool` returns `false`
2. **Built-in + function tool coexistence** — runs a Gemini 3 agent with both `GoogleSearchTool` and a custom tool, verifying `ServerToolCall`/`ServerToolResponse` parts appear in the response
3. **Custom tool still works** — verifies standard function calling still works normally alongside built-in tools

## Setup

```bash
cp .env.example .env
# Edit .env with your Google API key
```

## Usage

```bash
export GOOGLE_API_KEY=your-key-here
cargo run --manifest-path examples/gemini3_builtin_tools/Cargo.toml
```

Optionally override the model:

```bash
export GEMINI_MODEL=gemini-3.0-flash
```

## Requirements

- A Google API key with access to Gemini 3 models
- Scenarios 2 and 3 make live API calls
