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
| Prompts | `PromptingConfirmationHandler` serializes prompts, caps them per turn of each session, escapes control characters, refuses a truncated call approved unexpanded, and returns edited arguments for the policy to check again. It re-checks a held call against the run's whole policy, so an operator policy's approval always reaches the person and a standing rule answers only the harness's own |
| Durability | An action ledger on every run (in memory by default, `HarnessAgentBuilder::action_ledger` for a durable one, or the run's own): `bash`, `write_file` and `edit_file` are recorded before they execute and a re-issued or resumed call is answered from its record, never run twice. `Harness::send` and `Harness::resume` first settle a held call whose process stopped before saving its result: answered from the ledger, or held again |
| Shell | `bash` on the host behind approval, or opt-in under the OS sandbox (Seatbelt, bubblewrap) through `SandboxLauncher`: workspace read-write with protected paths read-only (read-only in Plan), scratch `HOME`, no network, the enforcer probed at build and per call (`Degraded` or `harness.sandbox_unavailable`, never a host fallback); a failure hint and a once-only, non-waivable unsandboxed retry; `auto_allow_sandboxed` gated on the S3 conformance probe and the protected paths; a 250 ms kill-switch poll; `harness.bash.*` audit records |
| Todos | `write_todos` / `update_todo` in session state, one list per session |
| Instructions | `AGENTS.md` from the git root down to the workspace, verbatim, hashed, delimited as untrusted, after a workspace trust decision bound to the files' content (an edited file asks again and records `harness.instructions.changed`); nested files listed, never inlined |
| Skills | Progressive disclosure (`list_skills`, `load_skill`, `load_skill_resource`) without convention documents |
| Observability | `harness.run` and `harness.decide` spans with `harness.mode`, `harness.tool_kind`, `harness.isolation`, `harness.rule_id`, `harness.member`; spend metered per acting agent and subject (`Harness::spend`, `harness.spend`); `LoopGuard` tightens a looping session to ReadOnly and ends the turn; an `adk-eval` suite with a `BaselineStore` baseline |
| Identity | `RbacMap` permissions per tool class through `ScopeGuard`/`ProtectedTool`; per-call, subject-scoped shell credentials through a `CredentialBroker` and a credential helper revoked after the call; `require_authenticated_identity` for served harnesses; every run audited in Block mode, to a signed `AuditLog` (`EphemeralAuditLog` by default) |
| Working notes | `FileMemory` topic files and a derived `MEMORY.md` per user, outside the workspace; `memory_read`, and `memory_write`/`memory_delete` behind a one-time approval; the index last in the instruction |
| Team | Members (`explore` and `general` by default, local `LlmAgent`s, ACP-compatible coding agents, remote A2A agents, any `Agent`, or discovered in a `TeamAgentRegistry`) lowered to one `TeamSpec`: isolated context, a timeout and circuit breaker on every edge, delegation depth 1, the team budget, the parent's mode and policy; ACP permission requests decided on the governed path; `harness.member.*` records |
| Plan graph | An approved plan with `steps` runs as a checkpointed graph: one node per step, `interrupt_before` on `approve` gates (a held `plan_step_gate` call bound to the plan hash and step), a reviewer after `review` gates, rewind through `TimeTravelHandle::fork_at` |
| Goals | `Harness::start_goal` iterates until the operator's `until` command passes (`LoopAgent` or a checkpointed graph); goal state in a `GoalStore`; durable goals as `harness.goal` runs for a `DurableWorker`; ambient goals on cron, file-watch and webhook triggers |
| Durability | A completed call never runs twice after a crash (the turn after one answers it from the action ledger), held approvals survive process loss, resumed runs follow the current mode |
| Long-term facts | `MemoryFact`s with provenance in a `FactStore` (in-memory, file, SQLite), found by a `FactIndex` (`LocalFactIndex` by embedding or shared words, `MemoryServiceIndex` over any `adk-memory` backend) and recalled into a `<memory-facts>` block on each user turn; written by `memory_remember` (assumed only), by distilling a session on a cheaper model, and by a person; `Known` only from the user's own quoted words or a person's confirmation; redacted before storage; `Harness::{facts, accept_fact, forget_fact, correct_fact}` |

## Mode table

| Class | ReadOnly | Plan | Ask | Auto |
|---|---|---|---|---|
| Read, Todo, Delegate | Allow | Allow | Allow | Allow |
| `exit_plan_mode` | Deny | Approval, non-waivable | Deny | Deny |
| Edit | Deny | Deny | Approval | Allow |
| Edit of a protected path | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Sandboxed shell | Deny | Allow, read-only workspace (active enforcer only) | Approval (Allow with `auto_allow_sandboxed`) | Allow when protected paths hold; Approval otherwise |
| Host shell (also a degraded sandbox) | Deny | Deny | Approval | Approval (Allow with `allow_host_shell_in_auto`) |
| Unsandboxed retry | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Memory write (`memory_write`, `memory_delete`) | Deny | Deny | Approval, non-waivable | Approval, non-waivable |
| Fact proposal (`memory_remember`, assumed facts only) | Deny | Deny | Allow | Allow |
| External tool, read-only, trusted origin | Allow | Allow | Allow | Allow |
| Any other external tool | Deny | Deny | Approval | Approval |

A standing rule turns an approval into Allow; it never applies to a non-waivable row, and a
Deny rule always wins. A path is protected as written or at the target it resolves to, so a
symlink inside the workspace to `AGENTS.md` or into `.git` is an edit of a protected path.
Protected at any depth: `.git`, `.adk`, `.skills`, `.claude`, `.cargo/config*`,
`AGENTS.md`, `AGENT.md`, `CLAUDE.md`, `MEMORY.md`, `.envrc`, `lefthook.yml`. Protected from
the workspace root (spec 3.4 §7.5): `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`,
`CODEOWNERS`, `publish.sh`, `.github/**`, `scripts/**`, `xtask/**`, `adk-eval/**`, the
governance sources of `adk-core/src`, `adk-harness/src/{policy.rs,approvals/**,selfimprove/**}`,
`adk-harness/tests/suite/**` and `.adk/selfimprove/**`.

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
[`harness_openai`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_openai) and
[`harness_anthropic`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_anthropic)
walk through the spec 3.1 §5 capabilities on one provider each, scripted or live.

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
| `memory` | on | working notes and long-term facts (`adk-memory`, `adk-guardrail`) |
| `memory-embeddings` | off | `RagEmbedder`: `adk-rag`'s OpenAI and Gemini embedders for long-term facts |
| `sqlite` | off | SQLite mode, rule, shell-ledger and goal stores |
| `ambient` | off | `adk-agent/ambient`: goals started by triggers |
| `skills-registry`, `team-acp`, `team-a2a`, `docker`, `memory-graph` | off | registry skills, ACP and A2A members, containers, graph memory |
| `browser`, `computer-use`, `payments`, `voice`, `managed` | off | the packs |

## Status

Beta (spec 3.1 phases 1 and 3). The packs (§7.16–§7.18) land over the 3.1.x line; until then
the builder refuses them with `not yet implemented: spec 3.1 §…`. The [team](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/team.md),
[plans and goals](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/plan-and-goals.md)
and [durability](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/durability.md)
guides cover §7.10–§7.12. Container isolation for the shell (`docker`) is not
implemented yet. [`examples/harness_redteam`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_redteam)
runs the shell red-team scenarios; the [shell and sandbox guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/harness/shell-and-sandbox.md)
covers the sandbox.
