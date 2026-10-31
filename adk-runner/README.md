# adk-runner

Agent execution runtime for ADK-Rust.

[![Crates.io](https://img.shields.io/crates/v/adk-runner.svg)](https://crates.io/crates/adk-runner)
[![Documentation](https://docs.rs/adk-runner/badge.svg)](https://docs.rs/adk-runner)
[![License](https://img.shields.io/crates/l/adk-runner.svg)](../LICENSE)

## Overview

`adk-runner` provides the execution runtime for [ADK-Rust](https://github.com/zavora-ai/adk-rust):

- **`Runner`** — runs an agent tree against a session and streams its events
- **`RunnerConfigBuilder`** — typestate builder (`Runner::builder()`) that enforces `app_name`, `agent`, and `session_service` at compile time
- **Session integration** — loads the session, appends every event, and applies state deltas as they arrive
- **Services** — gives agents and tools the artifact service (scoped to the session), the memory service, and the request context
- **Agent transfers** — runs transfer targets, enforcing each source agent's transfer restrictions
- **Steering** — `Runner::steer` sends a screened message to a run in flight, which its agent takes at the next safe point
- **Governance** — the run's tool policy, kill switch, budgets, spend ledger, action ledger, approval store, and audit trail
- **Context compaction** — summarizes older events to bound model context
- **`Launcher`** — a minimal stdin/stdout console loop

## Installation

```toml
[dependencies]
adk-runner = "3.0.0"
```

Or use the umbrella crate, whose default `minimal` tier includes the runner:

```toml
[dependencies]
adk-rust = "3.0.0"
```

## Feature Flags

| Feature | Default | Enables |
|---------|---------|---------|
| `artifacts` | yes | `artifact_service` through `adk-artifact` |
| `plugins` | yes | `plugin_manager` through `adk-plugin` |
| `skills` | yes | `Runner::with_auto_skills` and skill injection through `adk-skill` |
| `context-compaction` | no | The `compaction` module: `CompactionStrategy`, `TruncationCompaction`, `SummarisationCompaction`, and `RunnerConfigBuilder::context_compaction` for token-budget overflow |
| `sandbox-runner` | no | The `sandbox_runner` module: `SandboxRunner` provisions an `adk-sandbox` workspace, binds its tools for the run, and stops it afterwards |

## Quick Start

The runner reads an existing session, so create it first:

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::Content;
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
    let agent = LlmAgentBuilder::new("assistant").model(model).build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "my_app".into(),
            user_id: "user_123".into(),
            session_id: Some("session_456".into()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("my_app")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;

    let mut stream = runner
        .run_str("user_123", "session_456", Content::new("user").with_text("Hello!"))
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if event.is_final_response() {
            println!("{:?}", event.content());
        }
    }

    runner.close().await;
    Ok(())
}
```

`run_str` validates the ids and calls `run(UserId, SessionId, Content)`. `run_with_config` overrides the runner's `RunConfig` for one run, and `start_with_config` also returns the invocation id before the first event. `Runner::new(RunnerConfig { .. })` builds from a struct literal that names every field.

## Builder Options

| Method | Purpose |
|--------|---------|
| `app_name`, `agent`, `session_service` | Required |
| `artifact_service`, `memory_service` | Services exposed to agents and tools |
| `plugin_manager` | Run, user-message, event, agent, model, and tool plugin callbacks (`plugins`) |
| `run_config` | The base `RunConfig` for every run |
| `budget` | A `RunBudget` applied to every run with fresh counters |
| `tool_policy` | The `ToolPolicy` for every run |
| `governance` | Share a `GovernanceControl` (kill switch) across runners |
| `action_ledger`, `approval_store`, `spend_ledger` | Durable stores installed on every run whose `RunConfig` leaves them unset |
| `durable_run_store` | `interrupt` also requests cancellation of the session's durable runs on every replica |
| `audit_log`, `audit_failure_mode` | Record governed actions; `AuditFailureMode::Block` (the default) refuses an action whose record cannot be written |
| `identity_source`, `agent_principal` | The principal the root agent acts as |
| `subject_delegation` | Which runs may act for a subject other than the session's user |
| `compaction_config` | Post-invocation `EventsCompactionConfig` |
| `intra_compaction_config`, `intra_compaction_summarizer` | Compaction within an invocation |
| `context_compaction` | Overflow compaction (`context-compaction`) |
| `context_cache_config`, `cache_capable` | Provider prompt-cache lifecycle |
| `request_context` | Auth middleware context |
| `cancellation_token` | Cooperative cancellation of every run |

`action_ledger`, `approval_store`, and `spend_ledger` accept any backend implementing the `adk-core` traits — in-memory, or SQLite, PostgreSQL, Redis, MongoDB, Neo4j, or Firestore from `adk-session`.

## Runner vs Direct Agent Execution

| Concern | Direct `agent.run()` | `Runner` |
|---------|---------------------|----------|
| Session load and event persistence | Manual | Automatic |
| State deltas | Manual | Applied as events arrive |
| Artifact and memory services | Manual | Attached to the invocation context |
| Agent transfers | Manual | Automatic, with restrictions enforced |
| Input guardrail screening before save | Manual | Automatic |
| Budgets, spend, policy, audit | Manual | Automatic |

## Agent Transfers

When an agent sets `transfer_to_agent` in `EventActions`, the runner finds the target in the agent tree, builds its invocation context, and continues streaming its events in the same run.

In an ordinary agent tree, the runner checks every handoff — from a model's `transfer_to_agent` call, a tool's actions, or a custom agent's event — against the source agent's sub-agents, its parent unless `Agent::allows_transfer_to_parent` says no, and its parent's other sub-agents unless `Agent::allows_transfer_to_peers` says no (`Runner::allowed_transfer_targets`); a refused handoff is logged and control stays with the source. Validated composite roots can provide an exact per-member transfer allowlist through the `Agent` policy hooks; the runner reports undeclared targets or depth overflow as errors. After allowlist and depth validation, the runner awaits `Agent::govern_transfer` before executing the target. The default allows the transfer; portable teams and other composite roots can deny it with a reason, which surfaces as `agent.transfer.denied`.

## Run Lifecycle

| Stage | Guarantee |
|-------|-----------|
| Identity | The run acts for its session's user; a run configured to act for another subject fails with `identity.subject_mismatch` unless `subject_delegation` allows it |
| Input screening | Every agent's input guardrails screen the user message before it is saved |
| Plugin `on_event` | Sees every event the run yields, including a `before_run` early exit, a cancelled run's answers to its calls in flight, and a budget stop event |
| Cancellation | `Runner::interrupt(session_id)` stops a run; each call still in flight is answered `{"status": "outcome_unknown"}`, persisted, and yielded |
| Steering | `Runner::steer(identity, content)` screens a message like the run's own and queues it on that run; an agent takes it after a tool step and the runner saves it as a user event, or its `SteerReceipt` returns it when the run ends first |
| Plugin `after_run` | Runs once for every run whose `before_run` ran, however it ends — including a consumer that drops the stream, whose calls in flight are answered outcome-unknown first. A durable attempt its worker drops writes nothing; the recovering worker finishes the run |
| `Runner::close` | Closes every agent's toolsets through `Agent::close`, then the plugins |

`Runner::recover_unanswered_calls` answers function calls a stopped process left unanswered from the action ledger (`already_succeeded`, `already_failed`, or `outcome_unknown`) without executing them.

## Run Budgets

`RunnerConfigBuilder::budget(RunBudget)` limits model calls, total tokens, cost, wall time, and tool calls. Each run gets a fresh `BudgetTracker`, shared with transfer targets, workflow sub-agents, and agent tools through the run config. Limits are checked before every model call and tool dispatch; a run that reaches one ends with a persisted event explaining why and an `AdkError` with category `ResourceExhausted`. Under a cost cap, a response with no known cost stops the run unless `RunBudget::allow_unpriced_models()` is set.

```rust
use std::sync::Arc;

use adk_core::{Agent, Result, RunBudget};
use adk_runner::Runner;
use adk_session::SessionService;

fn budgeted(agent: Arc<dyn Agent>, sessions: Arc<dyn SessionService>) -> Result<Runner> {
    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(sessions)
        .budget(RunBudget::new().max_cost_usd(50.0).max_tool_calls(100))
        .build()
}
```

## Spend Ledger

When `RunConfig::spend_ledger` is set, every model call of the run — agent turns, compaction summaries (post-run, intra-invocation, and `context-compaction`), LLM routing, and MCP sampling — reserves its upper-bound cost (`Llm::max_cost_micro_usd`) before it is sent and commits its reported `UsageMetadata::cost`, keyed by app, agent, and vendor, through `adk_core::ModelCallScope`. A reservation the ledger refuses fails the call with `spend.limit_exceeded`, so a capped run never spends past its cap. `Runner::with_llm_spend_estimate` sets what an unpriced call commits when it reports no cost. See [Spend Ledger](../docs/official_docs/security/spend-ledger.md).

## Model History After Overlapping or Interrupted Turns

- **Actual results:** late tool responses are paired with their calls before a later user turn.
- **Missing results:** unresolved calls from earlier turns are omitted from model history; no result is fabricated.
- **Current execution:** unresolved calls at the history tail remain available for tool execution and confirmation.
- **Persistence:** source session events are unchanged by this projection.

`temp:` state keys stay visible for the rest of their invocation — to later tools, transferred agents, and `{temp:key}` instructions — and session services drop them when they persist the event.

## Context Compaction

```rust
use std::sync::Arc;

use adk_agent::LlmEventSummarizer;
use adk_core::{Agent, Llm, Result};
use adk_runner::{EventsCompactionConfig, Runner};
use adk_session::SessionService;

fn compacting(
    agent: Arc<dyn Agent>,
    sessions: Arc<dyn SessionService>,
    summarizer_model: Arc<dyn Llm>,
) -> Result<Runner> {
    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(sessions)
        .compaction_config(EventsCompactionConfig {
            compaction_interval: 3, // when the user-message count is a multiple of 3
            overlap_size: 1,        // leave the latest invocation out of the summary
            summarizer: Arc::new(LlmEventSummarizer::new(summarizer_model)),
        })
        .build()
}
```

Compaction runs after each invocation completes. When the session's count of user events is a multiple of the interval, older events are summarized into a single compacted event, and `MutableSession::conversation_history()` uses the most recent summary instead of the original events. No compaction covers a tool call still held for confirmation: session, intra-invocation, and context compaction all stop before it, so its approval can still resume it. `BaseEventsSummarizer` and `EventsCompactionConfig` are re-exported from `adk-core`.

See [Context Compaction](../docs/official_docs/sessions/context-compaction.md) for the full guide.

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `RunnerConfig` gains `tool_policy`, `governance`, `action_ledger`, `approval_store`, `spend_ledger`, `durable_runs`, `audit`, `identity_source`, and `subject_delegation` | Set them to `None` in struct literals, or use `Runner::builder()` |
| `adk_runner::Callbacks` and its callback aliases are removed; runner plugins' agent, model, and tool callbacks now run | Register callbacks on the agent builder or as plugins |
| `LlmSpendRecorder` is removed; model calls meter through `ModelCallScope` | `LlmSpendEstimate` is re-exported from `adk-core` |
| Every transfer obeys the source agent's restrictions | — |
| A cancelled run persists `outcome_unknown` answers to its in-flight calls | — |
| Nested agents' input guardrails screen every user message before it is saved | — |
| A run acting for a subject other than its session's user fails with `identity.subject_mismatch` | Configure `subject_delegation` |
| `Runner::close`, `Runner::cancel_durable_runs`, `Runner::recover_unanswered_calls`, `Runner::allowed_transfer_targets`, and `Runner::steer` are new | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — core traits
- [adk-agent](https://crates.io/crates/adk-agent) — agent implementations
- [adk-session](https://crates.io/crates/adk-session) — session storage
- [adk-artifact](https://crates.io/crates/adk-artifact) — artifact storage
- [adk-cli](https://crates.io/crates/adk-cli) — console and server launcher

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
