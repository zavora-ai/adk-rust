# Harness Plans and Goals

The `HarnessAgent` (`adk-harness`) has two forms of stepwise work: an approved plan with
steps runs as a checkpointed graph (feature `graph`), and a goal iterates the agent until an
operator's command passes.

## Plan graph

In Plan mode the model presents a plan with `exit_plan_mode`. When the plan has `steps` and
the person approves it, the plan compiles to a graph; from the next turn, while the session
is in Ask or Auto, each turn runs the graph instead of a free-form turn, until every step has
run.

```json
{
  "plan": "Add the test, fix the bug, update the changelog.",
  "next_mode": "auto",
  "steps": [
    { "id": "1", "title": "Test", "instruction": "Add a failing test for the bug" },
    { "id": "2", "title": "Fix", "instruction": "Fix the bug", "depends_on": ["1"], "gate": "review" },
    { "id": "3", "title": "Changelog", "instruction": "Add a changelog fragment", "depends_on": ["2"], "gate": "approve" }
  ]
}
```

| Plan element | Graph |
|---|---|
| A step | A node `step:<id>` that runs the harness agent with the step's instruction |
| `depends_on` | Edges; a step with several dependencies waits for all of them |
| `gate: "approve"` | `interrupt_before` on the step: the run pauses on a held call named `plan_step_gate` |
| `gate: "review"` | A node `review:<id>` after the step: the reviewer member reviews it in an isolated delegation |
| The plan size | The recursion limit: one super-step per node, plus one |

An invalid plan — an empty id, a repeated id, an unknown or circular dependency, a review
gate without a reviewer — is refused before the mode changes. `compile_plan(steps, hash)`
returns the graph's shape, and `PlanGraphShape::workflow_schema()` exports it as an
`adk-graph` `WorkflowSchema`.

### Approve gates

The gate's request names the plan hash, the run and the step, so its fingerprint changes when
the plan is edited or the step runs again after a rewind. The person decides it like any held
call:

```rust,no_run
use adk_core::{SessionId, UserId};
use adk_harness::{Harness, HarnessDecision, PLAN_GATE_TOOL};

async fn approve_gate(harness: &Harness, user: &UserId, session: &SessionId) -> adk_core::Result<()> {
    for pending in harness.pending_approvals(user, session).await? {
        if pending.request.tool_name == PLAN_GATE_TOOL {
            harness.decide(user, session, &pending.fingerprint, HarnessDecision::Approve).await?;
        }
    }
    let _events = harness.resume(user, session).await?;
    Ok(())
}
```

A denied gate stops the plan (`PlanRunStatus::Cancelled`). A standing rule cannot cover a
gate: each is decided once.

### Checkpoints and rewind

The graph is checkpointed in the harness's checkpointer — `plan_checkpointer(..)`, in memory
by default; SQLite in the CLI — so a step that finished survives process loss. To run the
steps after one again, rewind to it:

```rust,no_run
use adk_harness::HarnessAgent;

async fn rewind(harness: &HarnessAgent) -> adk_core::Result<()> {
    let graphs = harness.runtime().plan_graph().expect("feature graph with plan_graph on");
    let thread = graphs.rewind("my-app", "alice", "session-1", "2").await?;
    println!("the next turn continues on {thread}");
    Ok(())
}
```

`rewind` forks the plan's thread with `TimeTravelHandle::fork_at` at the checkpoint just
after the step completed: the steps up to it never run again; the steps after it run on the
next turn, and their Approve gates ask again.

| Record | Fields |
|---|---|
| `harness.plan.compiled` | `planHash`, `steps` (`id`, `title`, `dependsOn`, `gate`), `nodes`, `interruptBefore` |
| `harness.plan.step` | `planHash`, `stepId`, `gate`, `decision` (`pending`, `approved`, `denied`, `reviewed`), `run` |

## Goals

A goal iterates the harness agent until `until` exits 0 or `max_iterations` is reached. Goals
run in Ask or Auto only; each iteration's tool calls are governed like any turn.

```rust,no_run
use std::time::Duration;

use adk_core::{SessionId, UserId};
use adk_harness::{GoalSpec, Harness};

async fn start(harness: &Harness, user: &UserId, session: &SessionId) -> adk_core::Result<()> {
    let goal = GoalSpec {
        goal: "make the failing test pass".into(),
        until: "cargo test --quiet".into(),
        max_iterations: 5,
        check_timeout: Duration::from_secs(600),
    };
    let _events = harness.start_goal(user, session, goal).await?;
    if let Some(state) = harness.goal(user, session).await? {
        println!("{:?} after {} of {}", state.status, state.iteration, state.max_iterations);
    }
    Ok(())
}
```

| Engine | Shape | Default |
|---|---|---|
| `GoalEngine::Loop` | `LoopAgent` over the harness agent and `UntilCheckAgent`; a held call resumes at the right sub-agent through `workflow_positions` | without a plan checkpointer |
| `GoalEngine::Graph` | a checkpointed graph: `iterate` → `check` → (`__end__` \| `iterate`) | with `plan_checkpointer(..)` |

`UntilCheckAgent` checks the kill switch, runs `until` through the harness bash tool's
launcher (the OS sandbox when one is active) for up to the goal's `check_timeout`, which may
exceed the workspace's limit for the model's own commands, updates the goal state and stops
the loop on exit 0 (`Done`) or at the cap (`Exhausted`). The next iteration reads the check's output.
The command is the operator's, so it is not a governed call; it is audited.

Goal state lives in a harness-owned `GoalStore` — `goal_store(..)`, in memory by default,
`SqliteGoalStore` under `sqlite` — and is mirrored to `harness:goal` for display. While a
goal runs, every turn of the session continues it.

| Record | Fields |
|---|---|
| `harness.bash.begin` / `.end` | `functionCallId`, `enforcer`, `goalCheck: true`; `exitCode`, `durationMs`, `timedOut` |
| `harness.goal.iteration` | `iteration`, `max`, `checkExitCode`, `status` |

Each check runs in a `harness.goal` span with `harness.goal.iteration`.

### Durable goals

With a run store, a goal is a durable run:

```rust,no_run
use std::sync::Arc;

use adk_core::DurableRunStore;
use adk_harness::Harness;

fn serve_goals(harness: Harness, runs: Arc<dyn DurableRunStore>) -> adk_server::WorkerHandle {
    let harness = harness.with_run_store(runs.clone());
    let (target, runner) = harness.goal_target();
    adk_server::DurableWorker::new(runs).with_agent(target, runner).spawn()
}
```

`start_goal` then enqueues `NewRun { target: "harness.goal", session_id, concurrency_key:
session, idempotency_key: "goal:<session>:<digest>" }` on the `default` queue.
`Harness::with_goal_queue(adk_harness::GOAL_QUEUE)` puts goals on a queue of their own; serve
it with `DurableWorkerConfig::with_queue(harness.goal_queue())`, so the goal worker and a
server's background worker over one store never take each other's runs, as `adk-rust goal`
does. A held call parks the run as
`AwaitingApproval`; `Harness::decide` from any process over the same stores wakes it, and the
run resumes at the same iteration. The same goal on the same session is the same run.

### Ambient goals

With feature `ambient`, `Harness::ambient_goal(source, user, session, spec)` returns an
`AmbientAgent` that starts the goal on every event of a `CronTrigger`, `FileWatchTrigger` or
`WebhookTrigger`, in a new session per event unless one is given.
