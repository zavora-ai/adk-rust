# MCP Sampling Example

Demonstrates ADK-Rust's MCP Sampling support — the ability for MCP servers to request LLM inference from the client via `sampling/createMessage`.

## What This Shows

- Configuring `McpToolset` with a `SamplingHandler` so MCP servers can use the client's LLM
- Using `LlmSamplingHandler` to route sampling requests through the Gemini provider
- The full MCP sampling flow: tool call → server requests sampling → client LLM generates → response flows back

## What's Inside

Two binaries:

- **`sampling-server`** — An MCP server (over stdio) with two tools that use sampling:
  - `summarize` — asks the client's LLM to summarize text
  - `translate` — asks the client's LLM to translate text to a target language

- **`sampling-client`** — An LLM-powered agent that connects to the server with `LlmSamplingHandler`, discovers tools, and runs an interactive console. When the agent calls a tool, the server sends a `sampling/createMessage` request back to the client's LLM.

## How It Works

```
User ──→ Agent ──→ MCP Tool Call ──→ Server
                                       │
                                       │ peer.create_message(params)
                                       │ (sampling/createMessage)
                                       │
                              ←── LlmSamplingHandler called
                              (routes to Gemini LLM)
                              ──→ SamplingResponse { text, model }
                                       │
                              ←── Tool Result with LLM-generated content
```

The key difference from a standard MCP connection:

```rust
// Without sampling (standard)
let client = ().serve(transport).await?;
let toolset = McpToolset::new(client);

// With sampling
let elicitation = Arc::new(AutoDeclineElicitationHandler);
let sampling = Arc::new(LlmSamplingHandler::new(my_llm.clone()));
let toolset = McpToolset::with_sampling_handler(transport, elicitation, sampling).await?;
```

## Prerequisites

- Rust 1.85+
- `GOOGLE_API_KEY` environment variable set

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | Yes | Gemini API key |
| `GEMINI_MODEL` | No | Override model (default: `gemini-2.5-flash`) |
| `RUST_LOG` | No | Log level filter (default: `info`) |

## Run

```bash
export GOOGLE_API_KEY=your_key

# Build both binaries
cargo build --manifest-path examples/mcp_sampling/Cargo.toml

# Run the agent (spawns the server automatically)
cargo run --manifest-path examples/mcp_sampling/Cargo.toml --bin sampling-client
```

Then try prompts like:
- "Summarize this: Rust is a systems programming language focused on safety, speed, and concurrency."
- "Translate 'hello world' to French"

## Key APIs

| API | Purpose |
|-----|---------|
| `SamplingHandler` trait | Handle `sampling/createMessage` requests from MCP servers |
| `LlmSamplingHandler::new(llm)` | Default handler that routes sampling through an LLM provider |
| `McpToolset::with_sampling_handler()` | Connect to MCP server with sampling support |
| `AutoDeclineElicitationHandler` | Decline elicitation requests (required by `AdkClientHandler`) |
| `peer.create_message(params)` | Server-side: request LLM inference from the client |

## Expected Output

```
╔══════════════════════════════════════════╗
║  MCP Sampling — ADK-Rust v0.7.0         ║
╚══════════════════════════════════════════╝

Starting MCP sampling server...
MCP server connected with sampling support!

Discovered 2 tools:
  - summarize: Summarize the given text. Uses the client's LLM via MCP sampling.
  - translate: Translate text to a target language. Uses the client's LLM via MCP sampling.

Try: 'Summarize this: Rust is a systems programming language...'
  or 'Translate hello world to French'

> Summarize this: Rust is a systems programming language focused on safety...
Summary (generated via MCP sampling by model 'gemini-2.5-flash'):
Rust is a systems language prioritizing safety, speed, and concurrency.

✅ MCP Sampling example completed successfully.
```
