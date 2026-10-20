# ADK-Rust

**Rust Agent Development Kit (ADK-Rust)** — build AI agents in Rust with modular components for models, tools, memory, RAG, security, realtime voice, graph workflows, and more.

[![Crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
[![Documentation](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](../LICENSE)

`adk-rust` is the umbrella crate: it re-exports the ADK-Rust crates behind feature flags, grouped into four tiers. It is model-agnostic and deployment-agnostic.

## Supported Providers

| Provider | Feature | Catalog default model |
|----------|---------|-----------------------|
| Gemini | `gemini` (in every tier) | `gemini-3.7-flash` |
| OpenAI | `openai` | `gpt-5.6-terra` |
| Anthropic | `anthropic` | `claude-sonnet-5` |
| DeepSeek | `deepseek` | `deepseek-flash` |
| Groq | `groq` | `openai/gpt-oss-120b` |
| Ollama | `ollama` | `qwen3.5` |
| OpenRouter | `openrouter` | `qwen/qwen3.7-max` |
| OpenCode Go and Zen | `opencode` | — |
| Amazon Bedrock | `bedrock` | account- and region-specific |
| Azure AI Inference | `azure-ai` | endpoint-specific |
| Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, xAI, OrcaRouter, and other OpenAI-compatible APIs | `openai` (`OpenAICompatibleConfig` presets) | per preset |

The catalog defaults are `adk_rust::model::catalog` constants; every client takes any model ID.

## Quick Start

**1. Create a new project:**

```bash
cargo new my_agent && cd my_agent
```

**2. Add dependencies:**

```toml
[dependencies]
adk-rust = "3.0.0"
tokio = { version = "1", features = ["full"] }
dotenvy = "0.15"
```

**3. Set your API key:**

```bash
echo 'GOOGLE_API_KEY=your-key' > .env
```

**4. Write `src/main.rs`:**

```rust
use adk_rust::Launcher;
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

**5. Run:**

```bash
cargo run
```

With the default `minimal` tier, `Launcher` is `adk_runner::Launcher`, a console loop. With the `cli` feature it is `adk_cli::Launcher`, which also serves the web UI (`cargo run -- serve --port 8080`, on `127.0.0.1`).

### One Call — `adk_rust::run()`

```rust
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let response = run("You are a helpful assistant.", "What is Rust?").await?;
    println!("{response}");
    Ok(())
}
```

`run()` builds an agent on `provider_from_env()`, which first honors the Vertex opt-in flags `GOOGLE_GENAI_USE_ENTERPRISE` / `GOOGLE_GENAI_USE_VERTEXAI` (with `gemini-vertex`), then checks `ANTHROPIC_API_KEY` → `OPENAI_API_KEY` → `GOOGLE_API_KEY`, using the catalog default model of the first provider whose feature is compiled. The default `minimal` tier compiles only Gemini; add `openai` or `anthropic` to widen detection.

## Adding Tools

```rust
use adk_rust::prelude::*;
use adk_rust::tool::GoogleSearchTool;

fn researcher(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("researcher")
        .instruction("Search the web and summarize findings.")
        .model(model)
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()
}
```

`tools` is in the `standard` tier and above. Tool calls run through the governed path — tool policy, guardrails, and confirmation — before `execute()`; see [Tool Authorization](../docs/official_docs/security/tool-authorization.md).

## Workflow and Multi-Agent Systems

```rust
use adk_rust::prelude::*;

fn workflows(model: Arc<dyn Llm>, a: Arc<dyn Agent>, b: Arc<dyn Agent>) -> Result<()> {
    // Sequential, parallel, and loop workflows
    let _pipeline = SequentialAgent::new("pipeline", vec![a.clone(), b.clone()]);
    let _parallel = ParallelAgent::new("analysis", vec![a.clone(), b.clone()]);
    let _refiner = LoopAgent::new("refiner", vec![a.clone()]).with_max_iterations(5);

    // A coordinator that transfers to specialists
    let _coordinator = LlmAgentBuilder::new("coordinator")
        .instruction("Delegate tasks to specialists.")
        .model(model)
        .sub_agent(a)
        .sub_agent(b)
        .build()?;
    Ok(())
}
```

## Graph Workflows

```rust
use adk_rust::graph::prelude::*;
use serde_json::json;

fn graph() -> Result<GraphAgent> {
    GraphAgent::builder("processor")
        .node_fn("fetch", |_ctx| async move { Ok(NodeOutput::new().with_update("data", json!("raw"))) })
        .node_fn("transform", |ctx| async move {
            let data = ctx.get("data").cloned().unwrap_or_default();
            Ok(NodeOutput::new().with_update("result", data))
        })
        .edge(START, "fetch")
        .edge("fetch", "transform")
        .edge("transform", END)
        .checkpointer(MemoryCheckpointer::new())
        .build()
}
```

`adk-graph` has no default features, so each capability is forwarded from this crate:

| Feature | Enables | In `full` |
|---------|---------|-----------|
| `graph-functional` | `#[entrypoint]`/`#[task]` functional API | yes |
| `graph-node-cache` | node result caching with content keys | yes |
| `graph-delta` | delta checkpoints | yes |
| `graph-time-travel` | step, fork, and state history | yes |
| `graph-sqlite` | the SQLite checkpointer | no — needs a database |
| `graph-redis-cache` | Redis-backed node cache | no — needs a server |
| `graph-postgres`, `graph-redis`, `graph-mongodb`, `graph-neo4j`, `graph-firestore` | shared-database checkpointers | no — need a server |

## Realtime Voice Agents

```rust
use adk_rust::prelude::*;
use adk_rust::realtime::openai::OpenAIRealtimeModel;
use adk_rust::realtime::{RealtimeAgent, RealtimeModel};

fn voice(api_key: &str) -> Result<RealtimeAgent> {
    let model: Arc<dyn RealtimeModel> =
        Arc::new(OpenAIRealtimeModel::new(api_key, "gpt-realtime-2.1"));

    RealtimeAgent::builder("voice_assistant")
        .model(model)
        .instruction("You are a helpful voice assistant.")
        .voice("alloy")
        .server_vad()
        .build()
}
```

`realtime` is in the `enterprise` tier and enables `adk-realtime` without a provider transport. Each transport is its own feature: `openai-realtime` (OpenAI WebSocket), `gemini-live` (Gemini Live WebSocket), `vertex-live`, `livekit`, and `openai-webrtc`:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["openai-realtime"] }
```

See the [adk-realtime README](../adk-realtime/README.md).

## Declarative Security

```rust
use adk_rust::auth::{ContextScopeResolver, ScopeGuard};
use adk_rust::prelude::*;
use adk_rust::tool::FunctionTool;
use serde_json::{Value, json};

async fn transfer_funds(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    Ok(json!({ "transferred": args["amount"] }))
}

fn protected_transfer() -> impl Tool {
    // The tool declares the scopes it needs
    let transfer = FunctionTool::new("transfer", "Transfer funds", transfer_funds)
        .with_scopes(&["finance:write", "verified"]);

    // The guard enforces them before execution
    ScopeGuard::new(ContextScopeResolver).protect(transfer)
}
```

`adk-auth` also provides role-based access control, SSO/OIDC token validation, audit sinks, and a signed audit log; the runtime adds tool policies, a kill switch, run budgets, a spend ledger, and an action ledger in `adk-core`.

## Feature Tiers

```toml
# Minimal (default) — agents, models, Gemini, runner, sessions
adk-rust = "3.0.0"

# Standard — minimal + OpenAI, Anthropic (and the adk-anthropic client), skills, graph,
# auth, server, eval, guardrail, plugin, artifacts, tools, memory, telemetry, computer-use
adk-rust = { version = "3.0.0", features = ["standard"] }

# Enterprise — standard + realtime, browser, rag, payments, awp
adk-rust = { version = "3.0.0", features = ["enterprise"] }

# Full — enterprise + audio, code, sandbox, code-tools, and the graph-functional,
# graph-node-cache, graph-delta, and graph-time-travel capabilities
adk-rust = { version = "3.0.0", features = ["full"] }

# Gemini Enterprise Agent Platform — every Vertex/EAP integration except realtime
# transports; composable with any tier. Deploy-time tooling is host-side and not included.
adk-rust = { version = "3.0.0", features = ["standard", "gemini-agent-platform"] }

# gemini-agent-platform + the Vertex AI Live API (pulls in the realtime stack)
adk-rust = { version = "3.0.0", features = ["standard", "gemini-agent-platform-full"] }

# Custom selection
adk-rust = { version = "3.0.0", default-features = false, features = ["agents", "gemini", "runner", "sessions", "tools"] }
```

Features outside the tiers, composable with any of them:

| Group | Features |
|-------|----------|
| Providers | `deepseek`, `groq`, `ollama`, `openrouter`, `opencode`, `bedrock`, `azure-ai`, `gemini-vertex`, `gemini-interactions` |
| CLI | `cli`, `cli-openai`, `cli-anthropic`, `cli-deepseek`, `cli-groq`, `cli-ollama`, `cli-all-providers`, `cli-telemetry-otlp`, `optimize` |
| Production backends (need infrastructure) | `postgres-session`, `redis-session`, `mongodb-session`, `firestore-session`, `neo4j-session`, `vertex-session`, `sqlite-memory`, `database-memory`, `redis-memory`, `mongodb-memory`, `neo4j-memory`, `vertex-memory`, `auth-bridge` |
| Managed agents | `managed-runtime` (with the `gemini`, `openai`, `anthropic`, and `ollama` features for its model resolver), `enterprise-client` |
| Server | `a2a`, `yaml-agent`, `agent-registry`, `agent-engine`, `vertex-remote-engine` |
| Tools | `mcp`, `mcp-http`, `mcp-sampling`, `memory-tools`, `slack`, `bigquery`, `spanner`, `example-store`, `vertex-agent-registry`, `code-tools`, `code-embedded-js`, `code-embedded-python`, `code-docker`, `codeact`, `codeact-monty` |
| Telemetry | `telemetry-otlp`, `telemetry-sqlite`, `gcp-telemetry`, `record-payloads` |
| Realtime and audio | `openai-realtime`, `gemini-live`, `vertex-live`, `livekit`, `openai-webrtc`, `video-avatar`, `audio-*`, `whisper-onnx`, `distil-whisper`, `moonshine`, `kokoro`, `chatterbox`, `qwen3-tts`, `all-onnx` |
| Graph actions | `action`, `action-http`, `action-trigger`, `action-db`, `action-code`, `action-email`, `action-rss`, `action-full` |
| Other | `acp`, `skills-progressive-disclosure`, `personas`, `sandbox-workspace`, `sandbox-docker`, `aws-secrets`, `azure-keyvault`, `gcp-secrets`, `gcs-artifacts`, `vertex-sandbox`, `vertex-eval`, `vertex-rag`, `agent-retrieval`, `vertex-skill-registry`, `gcp-deploy` |

`adk-rust/Cargo.toml` is the authoritative list.

## Modules

Each enabled crate is re-exported as a module: `agent`, `model`, `tool`, `skill`, `session`, `artifact`, `memory`, `runner`, `server`, `telemetry`, `graph`, `realtime`, `browser`, `eval`, `guardrail`, `auth`, `payment`, `plugin`, `audio`, `rag`, `code`, `sandbox`, `computer_use`, `managed`, `anthropic_client`, `deploy`, and others. `adk_core` is re-exported at the crate root, and `adk_rust::prelude` brings the common types into scope.

## Changes in 3.0

- The server binds `127.0.0.1` and refuses unlisted hosts and cross-origin requests by default.
- Tool calls run through one governed path: tool policy, guardrails, and confirmation bound to the acting agent and the tool's origin.
- Run budgets, a spend ledger, an action ledger, durable runs, approvals, and audit records are part of the runtime, with stores on every session backend.
- `standard` includes `computer-use`; provider features also enable `adk-managed`'s model resolver when `managed-runtime` is on.

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md).

## Documentation

- [API Reference](https://docs.rs/adk-rust)
- [Official Guides](../docs/official_docs/index.md)
- [Examples](../examples/README.md) — standalone example crates in this repository; more in the [playground](https://github.com/zavora-ai/adk-playground)

## License

Apache-2.0
