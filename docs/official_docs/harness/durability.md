# Harness Durability

The `HarnessAgent` (`adk-harness`) states its durability as invariants (spec 3.1 §7.12). Each
row below is a test in `adk-harness/tests/durability_tests.rs`, which simulates a crash by
dropping the harness — runner, stores and connections — in the middle of a run and opening a
new one over the same SQLite file.

| Invariant | Mechanism | Test |
|---|---|---|
| A completed `bash`, `write_file`, `edit_file` or delegation never runs twice after a crash, retry or resume | The tools are `NonIdempotent`, so the runner's action ledger records each call; before a turn, `Harness::send` and `Harness::resume` answer the calls a stopped process left unanswered from the ledger (`already_succeeded`, `already_failed` or `outcome_unknown`), and nothing executes | `a_completed_bash_never_runs_twice_after_a_crash` |
| A held approval survives process loss and is answered from any process | The approval store is durable; another harness over the same store lists it, decides it and resumes the call | `a_held_approval_survives_process_loss_and_is_answered_from_another_harness` |
| A resumed run is governed by the current mode and rules | The policy reads the mode and rule stores at evaluation time; an approved call resumed after tightening to ReadOnly is refused | `a_resumed_run_is_governed_by_the_current_mode_not_the_one_it_parked_under` |
| Loosening cancels held calls | `Harness::set_mode` answers every held call "cancelled by mode change" before writing; a held `rm` runs zero times after switching to Auto | `a_held_rm_in_ask_runs_zero_times_after_switching_to_auto` |
| A plan step survives process loss | The plan graph is checkpointed; after a restart the gate is decided in the new process and only the remaining step runs | `a_plan_step_survives_process_loss` |
| Only one process works a run | `DurableWorker` claims runs under fenced leases; two workers over one store complete the run once | `a_durable_goal_survives_killing_the_process` |

## One backend per deployment

Sessions, approvals, the action ledger, modes, goals, durable runs and plan checkpoints can
share one SQLite file:

```rust,no_run
use std::sync::Arc;

use adk_core::{ActionLedger, ApprovalStore, DurableRunStore, Llm};
use adk_harness::{Harness, HarnessAgent, SqliteGoalStore, SqliteModeStore};
use adk_runner::Runner;
use adk_session::{SessionService, SqliteActionLedger, SqliteApprovalStore, SqliteRunStore, SqliteSessionService};

async fn open(model: Arc<dyn Llm>, url: &str) -> Result<Harness, Box<dyn std::error::Error>> {
    let pool = sqlx::SqlitePool::connect(url).await?;
    let sessions = SqliteSessionService::from_pool(pool.clone());
    sessions.migrate().await?;
    let approvals = SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let ledger = SqliteActionLedger::from_pool(pool.clone());
    ledger.migrate().await?;
    let modes = SqliteModeStore::new(pool.clone());
    modes.migrate().await?;
    let goals = SqliteGoalStore::new(pool.clone());
    goals.migrate().await?;
    let runs = SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await?;
    let checkpointer = adk_graph::SqliteCheckpointer::from_pool(pool).await?;

    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
            .mode_store(Arc::new(modes))
            .goal_store(Arc::new(goals))
            .approval_store(Arc::new(approvals) as Arc<dyn ApprovalStore>)
            .plan_checkpointer(Arc::new(checkpointer))
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(sessions);
    let runner = agent
        .configure_runner(Runner::builder().app_name("my-app").agent(agent.clone()).session_service(sessions))
        .action_ledger(Arc::new(ledger) as Arc<dyn ActionLedger>)
        .build()?;
    Ok(Harness::with_runner(agent, runner)?.with_run_store(Arc::new(runs) as Arc<dyn DurableRunStore>))
}
```

> **Note:** `adk-devtools` keeps its read-before-edit record in process memory, so an
> `edit_file` call approved after a restart fails with "must be read with read_file before
> it can be edited" until the model reads the file again in the new process.
