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

## The encrypted store

Feature `encrypted` seals every payload of the harness's stores at rest (spec 3.1 §7.12,
§9.2 I57). One `StoreKeyring { current, previous }` is resolved once at startup from a
`StoreKeySource`: `Secret { provider, name }` (an `adk-auth` secret provider), `Keychain {
service, account }` (the OS keychain) or `Env(var)`. Sessions wrap in
`adk_session::EncryptedSession`, held calls and decisions in
`adk_session::EncryptedApprovalStore`, and the SQLite mode, rule, shell-ledger, goal and
fact stores take the keyring through `with_keyring`; each payload column holds an
`adk-enc:v1:` AES-256-GCM envelope bound to its row, and identifiers, timestamps and status
columns stay plaintext for indexing.

| Invariant | Mechanism | Test |
|---|---|---|
| No payload reaches disk in plaintext | Every store seals before the write; the conformance suite scans the database file for planted sentinels | `encrypted_store_no_sentinel_reaches_the_file` |
| A wrong or missing key fails the open, never a plaintext fallback | `SqliteStoreMarker` records the key id; each store refuses an envelope it has no key for and a plaintext row while a key is configured | `encrypted_store_a_database_written_under_a_key_refuses_to_open_without_it` |
| Rotation retires the old key | `rewrap` on each store re-seals rows under the new key; `compact` vacuums the file so freed pages carry no plaintext; afterwards the old key opens nothing | `encrypted_store_rotation_reseals_every_row_and_the_old_key_opens_nothing` |
| A store can be encrypted in place | `rewrap` seals rows written before any key (sessions and approvals with legacy migration on) | `encrypted_store_rotation_seals_rows_written_before_the_key` |

`encrypted_store_conformance_tests!(keyring = …)` generates the four cases over a temporary
`harness.db`. `StoreRotation` describes a rotation and produces the
`harness.store.key_rotated` audit record (key ids, stores, rows, duration); the CLI's
`adk-rust store rotate-key` is the operator's entry point (see
[The harness in your terminal](cli.md#the-encrypted-store)).

```rust,no_run
use adk_harness::encrypted::{StoreKeySource, StoreKeyring};
use adk_harness::{SqliteFactStore, SqliteModeStore};
use adk_session::{EncryptedSession, SqliteSessionService};

async fn sealed(pool: sqlx::SqlitePool) -> adk_core::Result<()> {
    let keyring = StoreKeyring::resolve(&StoreKeySource::Env("ADK_STORE_KEY".into())).await?;
    let sessions = EncryptedSession::new(
        SqliteSessionService::from_pool(pool.clone()),
        keyring.current_key(),
        keyring.previous_keys(),
    );
    let modes = SqliteModeStore::new(pool.clone()).with_keyring(keyring.clone());
    let facts = SqliteFactStore::new(pool).with_keyring(keyring);
    let _ = (sessions, modes, facts);
    Ok(())
}
```
