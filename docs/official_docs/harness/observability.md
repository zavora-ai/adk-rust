# Harness observability and self-correction

The harness reports what it does as spans, meters what it spends per agent and subject,
stops a run that loops, and keeps a regression baseline of its own behaviour. Everything
on this page is feature `observe` of `adk-harness`, on by default (spec 3.1 §7.13).

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["observe"] }
```

## Spans

Model and tool calls carry the OpenTelemetry GenAI attributes from `adk-telemetry`. The
harness adds its own spans with `harness.*` attributes:

| Span | Attributes | Emitted |
|---|---|---|
| `harness.run` | `harness.mode`, `harness.isolation`, `agent.name`, `invocation.id` | around every harness run |
| `harness.decide` | `harness.tool_kind`, `harness.mode`, `harness.isolation`, `harness.rule_id` (when a standing rule allowed the call), `harness.member` (when a team member made the call), `harness.decision` | every policy decision |
| `harness.member` | `harness.member` | `adk_harness::member_span`, for team members |
| `harness.goal` | `harness.goal.iteration`, `harness.goal.max_iterations` | `adk_harness::goal_iteration_span`, for goal iterations |

The attribute names are constants: `ATTR_MODE`, `ATTR_TOOL_KIND`, `ATTR_ISOLATION`,
`ATTR_RULE_ID`, `ATTR_MEMBER`, `ATTR_GOAL_ITERATION`. `harness.tool_kind` uses
`tool_kind_label` (`read`, `edit`, `shell:host`, `memory_write`, `external:read_only:trusted`, …)
and `harness.isolation` uses `isolation_label` (`host`, `os_sandbox:<enforcer>`, …).

## Cost

Every harness run reserves and commits model spend in a `SpendLedger`:

| Item | Behaviour |
|---|---|
| Ledger | `HarnessAgentBuilder::spend_ledger`, else the runner's, else an `InMemorySpendLedger` with no limits |
| Attribution | `SpendKey { org: app, agent: acting agent id, vendor }`; the harness attributes each charge to the subject the run acts for |
| Usage | a run without a `RunBudget` still gets a budget tracker, so model calls, tokens and cost are counted |
| Report | `Harness::spend(Some(&user))` (one subject) or `Harness::spend(None)` (all): a `SpendReport` with one `SpendEntry` per agent, subject and vendor, and totals; `SpendReport::render` is the CLI's `/cost` text |
| Audit | `harness.spend` at the end of every run, also one that ends with an error: `spendKey`, `modelCalls`, `tokens`, `cost`, `toolCalls`, `budgetRemaining` |

`RunBudget` (`HarnessAgentBuilder::budget`) bounds a run and stops it with a
`ResourceExhausted` stop event; `budgetRemaining` reports what is left of it.

```rust
use std::sync::Arc;

use adk_core::{InMemorySpendLedger, SpendKey, SpendLimits, SpendPeriod, UserId};
use adk_harness::HarnessAgent;
use adk_session::InMemorySessionService;
use futures::StreamExt;

async fn report(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org("adk-rust").per(SpendPeriod::Day), 5_000_000),
    ));
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .spend_ledger(ledger)
        .build()?
        .into_harness("adk-rust", Arc::new(InMemorySessionService::new()))?;
    let alice = UserId::new("alice")?;
    let session = harness.open_session(&alice, None).await?;
    let events: Vec<_> = harness.send(&alice, &session, "summarize README.md").await?.collect().await;
    println!("{} events", events.len());
    println!("{}", harness.spend(Some(&alice)).render());
    Ok(())
}
```

## Loop guard

`HarnessAgentBuilder::loop_guard(LoopGuard { window, max_identical_calls, max_redundant })`
turns the guard on; `LoopGuard::default()` is a window of 20 calls, 3 identical calls in a
row, or 6 redundant calls.

Before each model request after a tool round, the guard runs `adk-eval`'s `TraceAnalyzer`
over the invocation's tool calls, each named by its tool and canonical arguments, so a
repeated pattern means repeated identical calls rather than the same tools with new
arguments: an edit-then-test cycle with new edits is not a loop. On a loop it:

1. Records `harness.loop.detected` (`window`, `identicalCalls`, `redundantCalls`, `agent`, `action`).
2. Tightens the session to `ReadOnly` with `ModeSource::SystemTighten`, recorded as
   `harness.mode.changed`. A person loosens it again with `Harness::set_mode`.
3. Ends the turn with `harness.loop_detected` (`ResourceExhausted`); the runner saves a stop
   event carrying the reason.

The guard never answers a tool call. It runs for team members too, whose loops tighten the
shared session.

## Eval suite

The harness keeps an `adk-eval` suite of the spec 3.1 §5 scenarios with a `BaselineStore`
regression baseline:

| Case | Pass condition |
|---|---|
| `fix_failing_test` | `sh test.sh` exits 0 after the run |
| `plan_mode_writes_nothing` | the workspace is byte-for-byte unchanged |
| `goal_until` | the `until` check passes within 4 turns |

Metrics per case are `passed`, `trajectory` (`ToolTrajectoryScorer`) and `efficiency`
(`TraceAnalyzer`).

| Run | Where | Baseline |
|---|---|---|
| Scripted model, every pull request | `adk-harness` test `eval_suite` | `adk-harness/tests/eval/baseline.scripted.json` |
| Live models, nightly | `examples/harness_eval` (`--live openai`, `anthropic`, `gemini`) in the `harness-eval-live` job | `examples/harness_eval/baseline.live.<provider>.json`, on `passed` |

```bash
cargo run --manifest-path examples/harness_eval/Cargo.toml                            # scripted
cargo run --manifest-path examples/harness_eval/Cargo.toml -- --live anthropic        # live
ADK_HARNESS_SAVE_BASELINE=1 cargo test -p adk-harness --test integration eval_suite:: # refresh
```
