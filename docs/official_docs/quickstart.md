# Quickstart

Create your first AI agent in under 5 minutes.

## Prerequisites

- Rust 1.95.0 or later (`rustup update stable`)
- An OpenAI API key for this walkthrough

## Step 1: Scaffold an OpenAI server

```bash
cargo install cargo-adk
cargo adk new quickstart_agent --template api --provider openai
cd quickstart_agent
```

This creates a complete Rust project with an OpenAI agent, in-memory sessions,
the ADK HTTP runtime and its streaming endpoints.

### Other Templates

```bash
# Agent with custom tools using #[tool] macro
cargo adk new my_agent --template tools

# RAG agent with Gemini embeddings and in-memory vector search
cargo adk new my_agent --template rag

# REST API server with streaming endpoints
cargo adk new my_agent --template api

# OpenAI agent with an interactive console
cargo adk new my_agent --template openai

# A2A protocol agent with builder API
cargo adk new my_agent --template a2a

# Use any provider with any template
cargo adk new my_agent --template tools --provider anthropic

# Add optional addons to any template
cargo adk new my_agent --template tools --addon docker --addon telemetry
```

| Template | What you get |
|----------|-------------|
| `basic` | Gemini agent with interactive console (default; alias for `llm`) |
| `tools` | Agent with `#[tool]` macro custom tools + schemars schema generation |
| `rag` | RAG pipeline — Gemini embeddings, in-memory vector store, document ingestion |
| `api` | Axum REST server, health check, and streaming endpoints |
| `openai` | OpenAI `gpt-5.6-terra` agent with console |
| `a2a` | A2A protocol agent with `A2aServer` builder and agent card (alias for `a2a-server`) |

Run `cargo adk templates` for every template, including the workflow agent types and the
enterprise patterns.

> **Tip:** Use the `--addon` flag to compose templates with optional addons such as `docker`, `telemetry`, `auth` and `server`. Run `cargo adk addons` for the list, and see [Composable Templates](development/composable-templates.md) for the enterprise patterns.

## Step 2: Add your API key

```bash
cp .env.example .env
# Open .env and replace the OPENAI_API_KEY placeholder.
```

The generated `.gitignore` excludes `.env`. Keep the key local and never commit
it.

## Step 3: Run the agent

```bash
cargo run
```

The server binds `127.0.0.1` unless you set `BIND_HOST`, and `PORT` changes the port.

## Step 4: Talk to it

Check that the server is up:

```bash
curl -fsS http://127.0.0.1:8080/api/health
```

Create a session and post a message; the reply streams as server-sent events:

```bash
curl -s -X POST http://127.0.0.1:8080/api/sessions \
  -H 'content-type: application/json' \
  -d '{"appName":"quickstart_agent","userId":"user1"}'
# use the returned "id" as SESSION_ID
curl -N -X POST http://127.0.0.1:8080/api/run/quickstart_agent/user1/SESSION_ID \
  -H 'content-type: application/json' \
  -d '{"new_message":"Explain what this agent can do in three concise Markdown bullets."}'
```

You now have one executable that runs the agent, manages sessions and streams
events. For a page where you talk to your agents, decide what they ask to do and review what
they did, run [the web home](surfaces/web-home.md): `adk-rust serve` serves it at
`http://127.0.0.1:8080/ui/`, and a server you build mounts it with `adk-surface`'s `web`
feature.

### Prefer a terminal-only agent?

Use the OpenAI console template instead:

```bash
cargo adk new quickstart_console --template openai
cd quickstart_console
cp .env.example .env   # replace the OPENAI_API_KEY placeholder
cargo run
```

---

## Zero-Config Alternative — `adk_rust::run()`

If you just want to run a quick agent without scaffolding, use the one-liner. It needs
`adk-rust`, `tokio`, `dotenvy` and `anyhow` in `Cargo.toml`:

```rust
use adk_rust::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Minimal default: set GOOGLE_API_KEY. Add provider features for OpenAI/Anthropic.
    let response = run("You are a helpful assistant.", "Explain Rust in one sentence.").await?;
    println!("{response}");
    Ok(())
}
```

This handles provider detection for compiled providers, session creation, agent building, and execution in a single call. It checks `ANTHROPIC_API_KEY`, then `OPENAI_API_KEY`, then `GOOGLE_API_KEY`, using each provider's default model, and skips providers whose feature is not compiled; with the default features only `GOOGLE_API_KEY` applies.

---

## Understanding the generated code

The API scaffold's `src/main.rs` builds an ordinary `LlmAgent`, then mounts it in
the standard server. The important wiring is (logging setup omitted):

```rust
use adk_rust::prelude::*;
use adk_rust::server::{ServerConfig, create_app};
use adk_rust::session::InMemorySessionService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")?;

    let model = adk_rust::model::openai::OpenAIClient::new(
        adk_rust::model::openai::OpenAIConfig::new(&api_key, "gpt-5.6-terra"),
    )?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("quickstart_agent")
            .description("REST API agent")
            .instruction("You are a helpful assistant accessible via REST API.")
            .model(Arc::new(model))
            .build()?,
    );

    let config = ServerConfig::new(
        Arc::new(adk_rust::SingleAgentLoader::new(agent)),
        Arc::new(InMemorySessionService::new()),
    );
    let app = create_app(config);

    // Loopback by default. Before setting BIND_HOST=0.0.0.0, configure authentication and
    // list the public host names in SecurityConfig::allowed_hosts.
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let host = std::env::var("BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

| Part | What it does |
|------|-------------|
| `OpenAIClient` | Creates the model client from `OPENAI_API_KEY` |
| `LlmAgentBuilder` | Builder pattern: name, description, instruction (system prompt), model, tools |
| `InMemorySessionService` | Stores local development sessions for the runner and the server |
| `create_app` | Mounts the REST/SSE API in one Axum router |

---

## Adding Custom Tools

The fastest way to add tools is the `#[tool]` macro. Add `adk-tool` to your dependencies:

```toml
[dependencies]
adk-tool = "3.0.0"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Then define a tool — the doc comment becomes the description, the args struct becomes the JSON schema:

```rust
use adk_tool::{tool, AdkError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to look up
    city: String,
}

/// Get the current weather for a city.
#[tool]
async fn get_weather(args: WeatherArgs) -> std::result::Result<Value, AdkError> {
    Ok(json!({ "temp": 22, "city": args.city, "condition": "sunny" }))
}
```

The macro generates a `GetWeather` struct implementing `Tool`, whose parameters schema comes from `WeatherArgs`; arguments that do not match it are answered with a `tool.invalid_arguments` error the model can correct. Add it to your agent:

```rust
let agent = LlmAgentBuilder::new("weather_agent")
    .instruction("Use the get_weather tool for weather questions.")
    .model(Arc::new(model))
    .tool(Arc::new(GetWeather))  // Generated by #[tool]
    .build()?;
```

> **Tip:** Or scaffold a project with tools already set up: `cargo adk new my-agent --template tools`

### Built-in Tools

ADK also includes ready-to-use tools (the `tools` feature of `adk-rust`):

```rust
// Google Search (handled server-side by Gemini)
.tool(Arc::new(GoogleSearchTool::new()))

// Exit a LoopAgent
.tool(Arc::new(ExitLoopTool::new()))
```

---

## Running as a Web Server

Scaffold a server project when you want HTTP serving:

```bash
cargo adk new my-api --template api --provider openai
cd my-api
cp .env.example .env
cargo run
```

The server answers on `http://127.0.0.1:8080/api/`. The default basic template uses the
lightweight console launcher instead.

---

## Using Other Models

Enable providers via feature flags. The default build stays Gemini-only for fast installs, so add only the provider you need:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["openai"] }
```

Or scaffold with a provider: `cargo adk new my-agent --provider openai`

### OpenAI

```rust
let api_key = std::env::var("OPENAI_API_KEY")?;
let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5.6-terra"))?;
```

### Anthropic

```rust
let api_key = std::env::var("ANTHROPIC_API_KEY")?;
let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5"))?;
```

### DeepSeek

```rust
let api_key = std::env::var("DEEPSEEK_API_KEY")?;
let model = DeepSeekClient::chat(api_key)?;         // deepseek-flash
// let model = DeepSeekClient::reasoner(api_key)?;   // deepseek-v4-pro with thinking
```

### Groq

```rust
let api_key = std::env::var("GROQ_API_KEY")?;
let model = GroqClient::new(GroqConfig::gpt_oss_120b(api_key))?;
```

### Ollama (Local)

```rust
// Requires: ollama serve && ollama pull llama3.2 (the `ollama` feature)
let model = OllamaModel::new(OllamaConfig::new("llama3.2"))?;
```

### Supported Models

| Provider | Model Examples | Feature Flag |
|----------|---------------|--------------|
| Gemini | `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash-lite`, `gemini-3.1-pro-preview` | (default) |
| OpenAI | `gpt-5.6-terra`, `gpt-5.6-sol`, `gpt-5.6-luna` | `openai` |
| Anthropic | `claude-sonnet-5`, `claude-opus-5`, `claude-fable-5` | `anthropic` |
| DeepSeek | `deepseek-flash`, `deepseek-v4-pro` | `deepseek` |
| Groq | `openai/gpt-oss-120b`, `openai/gpt-oss-20b` | `groq` |
| Ollama | Any pulled model, such as `qwen3.5` or `llama3.2` | `ollama` |

---

## Next Steps

- [LlmAgent Configuration](agents/llm-agent.md) — all configuration options
- [Function Tools](tools/function-tools.md) — create custom tools with `#[tool]`
- [Workflow Agents](agents/workflow-agents.md) — sequential, parallel, loop pipelines
- [Sessions](sessions/sessions.md) — manage conversation state
- [Callbacks](callbacks/callbacks.md) — customize agent behavior

---

**Previous**: [Introduction](introduction.md) | **Next**: [LlmAgent](agents/llm-agent.md)
