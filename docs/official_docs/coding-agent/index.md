# Coding Agent

Build agents that **work on a codebase**: read and edit files, run commands and
tests in a sandbox, plan multi-step work, iterate toward a goal autonomously, and
orchestrate parallel reviewers — all native to ADK-Rust, on any model provider.

This is assembled from a few focused pieces rather than a monolithic framework:

| Piece | What it gives you | Page |
|-------|-------------------|------|
| **`adk-devtools`** | The inner-loop tools — `read_file`, `write_file`, `edit_file`, `glob`, `grep`, `bash` — scoped to a workspace directory | [Dev tools](devtools.md) |
| **`CodingAgent`** (in `adk-agent`, feature `coding`) | A one-call harness wiring those tools + a planning `write_todos` tool + a minimal prompt onto an `LlmAgent` | [Harness](harness.md) |
| **CLI** (`adk-rust code` / `goal` / `ultracode`) | Native commands: one-shot tasks, autonomous goal mode, and parallel ultra-review | [CLI](cli.md) |
| **Graph workflows** (`adk-graph`) | Fan out to parallel specialist agents and synthesize — the "ultra" pattern | [Workflows](workflows.md) |
| **Examples** | Three runnable example crates | [Examples](examples.md) |

> **New to ADK-Rust?** Read the [Introduction](../introduction.md) and
> [Quickstart](../quickstart.md) first. This section builds on agents, tools,
> sessions, and (optionally) [memory](../memory/index.md) and
> [graphs](../agents/graph-agents.md).

## What you can build

- **A one-shot coding task** — "make the failing test pass", "add a `/health`
  route". The agent explores, edits, runs tests, and reports.
- **Autonomous goal mode** — give a goal + a verifiable success condition (a
  command that must exit 0); the agent loops *plan → act → verify*, self-correcting
  until it passes or a budget runs out. Durable and resumable. (Codex/Hermes
  `/goal` style.)
- **Ultra-review workflows** — implement, then fan out to parallel
  correctness/edge-case/style reviewers, synthesize their verdicts, and revise
  until they approve. (Claude Code `ultracode`/`ultrareview` style.)

## Install

```toml
# The harness (pulls in the dev tools) + a model provider
adk-agent = { version = "3.0.0", features = ["coding"] }
adk-devtools = "3.0.0"
adk-model = { version = "3.0.0", features = ["gemini"] }
adk-runner = "3.0.0"
adk-session = "3.0.0"
```

The dev tools are sandbox-first and have no heavy dependencies, so the footprint
stays small; `adk-graph` is only needed for the parallel [workflows](workflows.md).

## 60-second quick start

```rust
use adk_agent::coding::CodingAgent;
use adk_devtools::Workspace;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_core::{Content, SessionId, UserId};
use std::sync::Arc;

# async fn run(model: std::sync::Arc<dyn adk_core::Llm>) -> anyhow::Result<()> {
// One call builds a coding agent over a confined workspace.
let coding = CodingAgent::builder()
    .model(model)
    .workspace(Workspace::new("./my-repo"))
    .build()?;

let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
sessions.create(CreateRequest {
    app_name: "demo".into(), user_id: "u".into(),
    session_id: Some("s".into()), state: Default::default(),
}).await?;

let runner = Runner::builder()
    .app_name("demo")
    .agent(coding.agent())          // -> Arc<dyn Agent>
    .session_service(sessions)
    .build()?;

let mut events = runner
    .run(UserId::new("u")?, SessionId::new("s")?,
         Content::new("user").with_text("Add a function add(a,b) to add.py and run it."))
    .await?;
// stream `events`: FunctionCall / FunctionResponse / Text …
# Ok(()) }
```

Prefer the terminal? `adk-rust code "make the failing test pass"` does the same —
see the [CLI](cli.md).

## Where to go next

1. **[Dev tools](devtools.md)** — the toolset and the sandboxed `Workspace`.
2. **[Harness](harness.md)** — `CodingAgent`, the plan loop, permission modes.
3. **[CLI](cli.md)** — `code`, `goal` (durable/resumable), `ultracode`.
4. **[Workflows](workflows.md)** — parallel ultra-review on `adk-graph`.
5. **[Examples](examples.md)** — three runnable example crates.

See also the full design rationale in
[`docs/design/coding-agent.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/coding-agent.md).
