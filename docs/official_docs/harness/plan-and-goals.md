# Harness Plans and Goals

The `HarnessAgent` (`adk-harness`) has two forms of stepwise work: an approved plan with
steps runs as a checkpointed graph (feature `graph`), and a goal iterates the agent until an
operator's command passes, a separate grader passes every criterion of a rubric, or both.

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

The graph is checkpointed in the harness's checkpointer — `plan_checkpointer(..)` or
`plan_checkpoint_store(url)`, in memory by default; in the CLI, the store the sessions are in
(`harness.db`, or `serve --store`) unless `--checkpointer` names another — so an approved plan
and a step that finished survive process loss. To run the
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

`PlanGraphs::rewind` moves the plan only; no file is restored. With snapshots on, the harness
also snapshots the workspace before each step, and `Harness::rewind_plan(.., RewindTarget::PlanStep(id))`
puts back the files and the plan together, so the step and the ones after it run again over
the files they first saw ([Snapshots and rewind](snapshots-and-rewind.md)).

| Record | Fields |
|---|---|
| `harness.plan.compiled` | `planHash`, `steps` (`id`, `title`, `dependsOn`, `gate`), `nodes`, `interruptBefore` |
| `harness.plan.step` | `planHash`, `stepId`, `gate`, `decision` (`pending`, `approved`, `denied`, `reviewed`), `run` |

## Goals

A goal iterates the harness agent until its checks pass or `max_iterations` is reached. A goal
has an `until` command, a rubric, or both:

| Check | Passes when | Set with |
|---|---|---|
| `until` | the operator's shell command exits 0 | `GoalSpec::with_until`, `--until CMD` |
| Rubric | a separate grader passes every criterion | `GoalSpec::with_rubric(GoalRubric)`, `--rubric FILE` |
| Both | the command exits 0, then the grader passes every criterion | both |

Goals run in Ask or Auto only; each iteration's tool calls are governed like any turn.

```rust,no_run
use std::time::Duration;

use adk_core::{SessionId, UserId};
use adk_harness::{GoalSpec, Harness};

async fn start(harness: &Harness, user: &UserId, session: &SessionId) -> adk_core::Result<()> {
    let goal = GoalSpec::new("make the failing test pass")
        .with_until("cargo test --quiet")
        .with_max_iterations(5)
        .with_check_timeout(Duration::from_secs(600));
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
| `GoalEngine::Graph` | a checkpointed graph: `iterate` → `check` → (`__end__` \| `iterate`) | with `plan_checkpointer(..)`; `adk-rust serve` |

`adk-rust` keeps `Loop` for the REPL, `code`, `goal` and `ultracode`, where goals are durable
runs, and `serve` runs goals on the `Graph` engine over its plan checkpointer (spec 3.1
§7.11).

`UntilCheckAgent` checks the kill switch, runs `until` through the harness bash tool's
launcher (the OS sandbox when one is active) for up to the goal's `check_timeout`, which may
exceed the workspace's limit for the model's own commands, grades the iteration when the goal
has a rubric, updates the goal state and stops the loop once every check passes (`Done`) or at
a cap (`Exhausted`). The next iteration reads the check's output and the grader's feedback in
a system notice (authored `system`), the harness's own message to the agent rather than another
agent's output quoted to it as data.
The command is the operator's, so it is not a governed call; it is audited.

### Rubric-graded goals

A rubric is a list of independently gradeable criteria. After each iteration a grader checks
the iteration's deliverable against every criterion, in a context of its own: its one request
holds the goal, the criteria, the iteration's final message and the rubric's `deliverables`
files, never the conversation that produced them. It answers pass or fail with feedback per
criterion, and the failed criteria with their feedback are what the next iteration reads.

```rust,no_run
use adk_core::{SessionId, UserId};
use adk_harness::{GoalRubric, GoalSpec, Harness};

async fn start(harness: &Harness, user: &UserId, session: &SessionId) -> adk_core::Result<()> {
    let rubric = GoalRubric::from_markdown(
        "- blurb.md names the product, Ferris Notes\n\
         - It states the monthly price\n\
         - It is at most 60 words long",
    )?
    .with_deliverables(["blurb.md"])
    .with_max_evaluations(3);
    let goal = GoalSpec::new("Write the launch blurb to blurb.md").with_rubric(rubric);
    let _events = harness.start_goal(user, session, goal).await?;
    if let Some(outcome) = harness.goal(user, session).await?.and_then(|state| state.outcome()) {
        println!("{outcome}");
    }
    Ok(())
}
```

| Setting | Default | Effect |
|---|---|---|
| `criteria` | — | One pass/fail verdict each; `GoalRubric::new`, `from_markdown` (one list item per criterion), or `from_goal` (the goal text's list items, else the goal itself) |
| `deliverables` | none | Workspace files the grader reads besides the final message, up to 64 KiB each, through the workspace's containment and exclusions |
| `max_evaluations` | 3 (`adk_eval::DEFAULT_OUTCOME_ITERATIONS`) | Grading rounds before the goal is `Exhausted`; at most 20 (`adk_eval::MAX_OUTCOME_ITERATIONS`) |
| `hidden` | `false` | Shows the criteria to the grader only; the agent learns the failed ones from the feedback |
| `HarnessAgentBuilder::goal_grader(model)` | the harness model | The model that grades |

- **Order.** With `until` as well, the grader runs only once the command exits 0, so a failing
  command costs no grading call and only graded rounds count toward `max_evaluations`.
- **Budget.** The grading call runs in the run's `ModelCallScope`: it counts against the run
  budget and spend ledger, and a refused call stops the run with the budget's error before it
  reaches the provider.
- **Fail closed.** A criterion the grader gives no verdict on fails, and a reply with no
  readable JSON fails every criterion. The request carries a JSON response schema, which a
  provider with structured outputs enforces.
- **Outcome.** `GoalState` keeps `evaluations` and `last_grade` (an `adk_eval::OutcomeGrade`),
  and `GoalState::outcome` states how the goal ended, naming the criteria that still fail.

`adk-rust goal` and a launcher's `goal` take the rubric as flags, or from a `[goal]` table in
the workspace's `.adk/harness.toml` when no flag names one:

| Flag | `[goal]` key | Effect |
|---|---|---|
| `--rubric FILE` | `rubric = "rubric.md"` | Criteria from a Markdown file; the project file's path must lie inside the workspace |
| `--rubric-from-goal` | — | Criteria from the goal text |
| `--deliverable PATH` (repeatable) | `deliverables = ["…"]` | Files the grader reads |
| `--max-evaluations N` | `max_evaluations = N` | Grading rounds, 1 to 20 |
| `--rubric-hidden` | `hidden = true` | Criteria for the grader only |

```bash
adk-rust goal "Write the launch blurb to blurb.md" --rubric rubric.md --deliverable blurb.md
```

`adk_eval::RubricCheckAgent` is the same check for any `LoopAgent`; see
[Evaluation](../evaluation/evaluation.md#rubric-graded-outcomes).

Goal state lives in a harness-owned `GoalStore` — `goal_store(..)`, in memory by default,
`SqliteGoalStore` under `sqlite` — and is mirrored to `harness:goal` for display. While a
goal runs, every turn of the session continues it.

| Record | Fields |
|---|---|
| `harness.bash.begin` / `.end` | `functionCallId`, `enforcer`, `goalCheck: true`; `exitCode`, `durationMs`, `timedOut` |
| `harness.goal.grade` | `goal`, `iteration`, `evaluation`, `maxEvaluations`, `model`, `criteria`, `failed` (criterion numbers), `satisfied` |
| `harness.goal.iteration` | `iteration`, `max`, `checkExitCode`, `rubricSatisfied`, `status` |

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
