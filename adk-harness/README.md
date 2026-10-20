# adk-harness

The governed harness agent of ADK-Rust: file and shell tools, a planning loop, skills,
approvals and resumable sessions, with every tool call on one governed path
(spec 3.1, [`docs/design/spec-3.1-harness-agent.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.1-harness-agent.md)).

[![Crates.io](https://img.shields.io/crates/v/adk-harness.svg)](https://crates.io/crates/adk-harness)
[![Documentation](https://docs.rs/adk-harness/badge.svg)](https://docs.rs/adk-harness)
[![License](https://img.shields.io/crates/l/adk-harness.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

`HarnessAgent` is configuration and policy over an `LlmAgent`. Its `configure_run` composes
`HarnessPolicy` with the run's own policy (`StrictestPolicy`, the stricter decision wins),
so the policy reaches every runtime that runs the harness — a `Runner`, a transfer, an
agent tool — and the harness refuses to run without it.

| Capability | Mechanism |
|---|---|
| Modes | `ReadOnly`, `Plan`, `Ask` (default), `Auto` in a harness-owned `ModeStore` (in-memory, file, SQLite). Tightening applies at the next call; loosening only through `Harness::set_mode`, which cancels held calls first, or an approved `exit_plan_mode` |
| Tool classes | A call's class comes from its origin and name in the harness registry, never from an MCP `readOnlyHint` alone |
| Standing approvals | "Always allow `cargo test`" as a `StandingRule` bound to the subject and to the tool's declaration digest; `ShellPrefix` matches plain words only; a redeclared tool records `harness.rule.stale` and asks again |
| Prompts | `PromptingConfirmationHandler` serializes prompts, caps them per turn, escapes control characters, refuses a truncated call approved unexpanded, and returns edited arguments for the policy to check again |
| Shell | `bash` on the host behind approval (OS sandbox opt-in), a 250 ms kill-switch poll, a per-session shell ledger for unsandboxed retries, `harness.bash.*` audit records |
| Todos | `write_todos` / `update_todo` in session state, one list per session |
| Instructions | `AGENTS.md` from the git root down to the workspace, verbatim, hashed, delimited as untrusted, after a workspace trust decision; nested files listed, never inlined |
| Skills | Progressive disclosure (`list_skills`, `load_skill`, `load_skill_resource`) without convention documents |

## Mode table

| Class | ReadOnly | Plan | Ask | Auto |
|---|---|---|---|---|
| Read, Todo, Delegate | Allow | Allow | Allow | Allow |
| `exit_plan_mode` | Deny | Approval, non-waivable | Deny | Deny |
| Edit | Deny | Deny | Approval | Allow |
| Edit of a protected path | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Host shell | Deny | Deny | Approval | Approval (Allow with `allow_host_shell_in_auto`) |
| Unsandboxed retry | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| External tool, read-only, trusted origin | Allow | Allow | Allow | Allow |
| Any other external tool | Deny | Deny | Approval | Approval |

A standing rule turns an approval into Allow; it never applies to a non-waivable row, and a
Deny rule always wins.

## Example

```rust,ignore
use std::sync::Arc;

use adk_core::{SessionId, UserId};
use adk_harness::{HarnessAgent, HarnessMode};
use adk_model::gemini::GeminiModel;
use adk_session::InMemorySessionService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Plan)
        .build()?
        .into_harness("my-app", Arc::new(InMemorySessionService::new()))?;
    let user = UserId::new("alice")?;
    let session = harness.open_session(&user, None).await?;
    let _events = harness.send(&user, &session, "Plan the fix for the failing test").await?;
    Ok(())
}
```

The [`harness_agent`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_agent)
example runs the scripted scenarios against the harness and the 3.0 `CodingAgent`, and the
live scenarios on OpenAI, Anthropic and Gemini.

## Features

| Feature | Default | Pulls in |
|---|---|---|
| `runner` | on | `adk-runner`, `adk-session` (`Harness`) |
| `skills` | on | `adk-skill/progressive-disclosure` |
| `team` | on | `adk-tool`, `adk-agent/team-tools` |
| `graph` | on | `adk-graph` |
| `sandbox` | on | `adk-sandbox/sandbox-native` |
| `observe` | on | `adk-telemetry`, `adk-eval` |
| `auth` | on | `adk-auth` |
| `memory` | on | file memory |
| `sqlite` | off | SQLite mode, rule and shell-ledger stores |
| `skills-registry`, `team-acp`, `team-a2a`, `docker`, `memory-graph` | off | registry skills, ACP and A2A members, containers, graph memory |
| `browser`, `computer-use`, `payments`, `voice`, `managed` | off | the packs |

## Status

Beta (spec 3.1 phase 1). Team members (§7.10), the plan graph and goal engine (§7.11),
observability (§7.13), identity (§7.14), file memory (§7.15) and the packs (§7.16–§7.18)
land over the 3.1.x line; until then the builder refuses them with
`not yet implemented: spec 3.1 §…`. The OS-sandbox shell reports itself degraded until
`adk_sandbox::sandbox::sandboxed_command` and the devtools `CommandLauncher` land.
