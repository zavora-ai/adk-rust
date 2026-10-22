# The Harness

The harness is the ready-made agent you run first: file and shell tools, todos, plan mode,
project instructions and skills, approvals and resumable sessions. `HarnessAgent`
(`adk-harness`, spec 3.1) builds it as configuration and policy over an `LlmAgent`, and
every tool call it makes, or any member of its team makes, passes one governed path: the
session's mode, your standing rules, the operator's policy, approvals, and the action
ledger.

| Use it as | How |
|---|---|
| A command-line agent | `cargo install adk-cli`, then `adk-rust` in a project; see [The harness in your terminal](cli.md) |
| A library | `adk-rust` with the `harness` feature (in `full`), or `adk-harness` directly |
| A new project | `cargo adk new my-harness --template harness`; see [Configuration](configuration.md#the-harness-template) |
| A served agent | `adk-rust serve`, or `HarnessLauncher::run_serve_directly`; see [Serving the harness](serving.md) |

## Quick start

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["harness"] }
adk-devtools = "3.0.0"
tokio = { version = "1", features = ["full"] }
anyhow = "1"
```

```rust
use std::sync::Arc;

use adk_rust::futures::StreamExt;
use adk_rust::harness::{HarnessAgent, HarnessMode};
use adk_rust::model::GeminiModel;
use adk_rust::session::InMemorySessionService;
use adk_rust::UserId;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Plan)
        .build()?
        .into_harness("my-app", Arc::new(InMemorySessionService::new()))?;

    let user = UserId::new("alice")?;
    let session = harness.open_session(&user, None).await?;
    let mut events = harness.send(&user, &session, "Plan the fix for the failing test").await?;
    while let Some(event) = events.next().await {
        let _ = event?;
    }
    println!("todos: {:?}", harness.todos(&user, &session).await?.items);
    Ok(())
}
```

In plan mode the harness reads, keeps todos and proposes a plan with `exit_plan_mode`;
nothing it runs can change the workspace. Without a prompter, a call that needs a person is
held: `Harness::pending_approvals` lists it and `Harness::decide` answers it, and the next
turn runs it once (see [Modes and approvals](modes-and-approvals.md)).

## What it does

| Capability | Mechanism | Page |
|---|---|---|
| Modes | `read_only`, `plan`, `ask` (default), `auto` per session, in a harness-owned store; the model leaves plan mode only through an approved `exit_plan_mode` | [Modes and approvals](modes-and-approvals.md) |
| Approvals | A prompter at the terminal, or held calls decided later from any process; "always allow" becomes a standing rule bound to you and to the tool's declaration | [Modes and approvals](modes-and-approvals.md) |
| Shell | `bash` on the host behind approval, or in the OS sandbox (Seatbelt, bubblewrap) | [Shell and sandbox](shell-and-sandbox.md) |
| Instructions and skills | `AGENTS.md` after a workspace trust decision; skills loaded on demand | [Configuration](configuration.md) |
| Team | Local, ACP and A2A members under the parent's mode, budget and policy | [Team](team.md) |
| Plans and goals | An approved plan runs as a checkpointed graph; goals iterate until a command passes, as durable runs | [Plans and goals](plan-and-goals.md) |
| Durability | A completed call never runs twice; held approvals survive process loss | [Durability](durability.md) |
| Identity | Roles per tool class, per-call shell credentials, authenticated serving | [Identity](identity.md) |
| Observability | Spans, spend per agent and subject, a loop guard that tightens the mode | [Observability](observability.md) |
| Memory | Working notes outside the workspace, written behind approval; long-term facts with provenance, recalled into each turn | [Memory](memory.md) |

## Pages

| Page | Covers |
|---|---|
| [Modes and approvals](modes-and-approvals.md) | The mode table, switching modes, prompts, held calls and decisions, standing rules |
| [Configuration](configuration.md) | Builder options and defaults, `.adk/harness.toml`, features, the `harness` template |
| [The harness in your terminal](cli.md) | `adk-rust`, `code`, `goal`, `ultracode`, slash commands, `harness.db` |
| [Serving the harness](serving.md) | `adk-rust serve`, request identity, the approval inbox, background runs |
| [Shell and sandbox](shell-and-sandbox.md) | Host and sandboxed `bash`, isolation status, unsandboxed retries |
| [Team](team.md) | Members, isolation, budgets, discovery, review rounds |
| [Plans and goals](plan-and-goals.md) | The plan graph, gates and rewind; loop-until goals, durable and ambient |
| [Durability](durability.md) | The durability invariants and the tests that prove them |
| [Identity](identity.md) | RBAC, credentials, authenticated identity, audit |
| [Observability](observability.md) | Spans, spend, the loop guard, evaluation baselines |
| [Memory](memory.md) | Working notes and long-term facts |
| [Payments](payments.md) | The payments pack: purchases under a mandate, approved once each, metered and receipted |
| [Browser and computer use](packs.md) | The browser pack under the web rules; desktop actions through the governed reference graph, approved once each |
| [Voice](voice.md) | The voice surface: the same harness over a realtime session, spoken approvals, one audit trail |
| [Migrating from `CodingAgent`](../migration/coding-agent-to-harness.md) | Moving from `adk_agent::coding` to the harness |

## Status

`adk-harness` is Beta ([STABILITY.md](https://github.com/zavora-ai/adk-rust/blob/main/STABILITY.md)).
The payments pack (`harness-payments`) is in [Payments](payments.md); the voice surface
(`harness-voice`) is in [Voice](voice.md); the managed form
(`harness-managed`, Experimental like `adk-managed`) runs the harness on the managed runtime,
see [Serving the harness](serving.md#on-the-managed-runtime). The browser and computer-use
packs (`harness-browser`, `harness-computer-use`) are in
[Browser and computer use](packs.md). The design is
[spec 3.1](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.1-harness-agent.md).
