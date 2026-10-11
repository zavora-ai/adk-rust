# Durable Stores

Three stores keep safe actions safe across restarts and across processes, and a fourth
keeps the signed audit log:

| Store | Trait | Keeps | Set on |
|-------|-------|-------|--------|
| Spend ledger | `SpendLedger` | Budget holds and committed spend | `RunConfig::spend_ledger`, `Runner::builder().spend_ledger(...)` |
| Action ledger | `ActionLedger` | One record per non-idempotent tool call | `RunConfig::action_ledger`, `Runner::builder().action_ledger(...)` |
| Approval store | `ApprovalStore` | Calls held for confirmation and the decisions for them; the requests and delegations of the [approval service](approval-service.md) | `RunConfig::approval_store`, `Runner::builder().approval_store(...)`, `RunConfig::approval_service` |
| Audit log store | `adk_core::audit_log::AuditLogStore` | Audit records, Merkle subtree hashes, and signed checkpoints | `adk_auth::signed_audit::AuditLog::builder(...)` — see [Audit Log](audit-log.md) |

Every backend implements the same trait and passes the same conformance suite, so choosing
a backend changes one constructor and nothing else.

## Backend matrix

| Backend | Feature | `SpendLedger` | `ActionLedger` | `ApprovalStore` | Conformance run |
|---------|---------|---------------|----------------|-----------------|-----------------|
| In memory | `adk-core` | `InMemorySpendLedger` | `InMemoryActionLedger` | `InMemoryApprovalStore` | Every PR (`adk-core`); restart cases are not applicable |
| SQLite | `sqlite` | `SqliteSpendLedger` | `SqliteActionLedger` | `SqliteApprovalStore` | Every PR (`adk-session`) |
| PostgreSQL | `postgres` | `PostgresSpendLedger` | `PostgresActionLedger` | `PostgresApprovalStore` | Nightly, live server |
| Redis | `redis` | `RedisSpendLedger` | `RedisActionLedger` | `RedisApprovalStore` | Nightly, live server |
| MongoDB | `mongodb` | `MongoSpendLedger` | `MongoActionLedger` | `MongoApprovalStore` | Nightly, live server |
| Neo4j | `neo4j` | `Neo4jSpendLedger` | `Neo4jActionLedger` | `Neo4jApprovalStore` | Nightly, live server |
| Firestore | `firestore` | `FirestoreSpendLedger` | `FirestoreActionLedger` | `FirestoreApprovalStore` | Nightly, emulator |

| Backend | Feature | `AuditLogStore` | Conformance run |
|---------|---------|-----------------|-----------------|
| In memory | `adk-core` | `InMemoryAuditLogStore` | Every PR (`adk-core`) |
| SQLite | `sqlite` | `SqliteAuditLogStore` | Every PR (`adk-session`) |
| PostgreSQL | `postgres` | `PostgresAuditLogStore` | Nightly, live server |

Every durable type in `adk-session` follows one setup sequence:

1. Construct it with `new(...)`, or from the handle a session service already holds:
   `from_pool` (SQLite, PostgreSQL), `from_client` (Redis), `from_database` (MongoDB),
   `from_graph` (Neo4j), `from_db` (Firestore).
2. Call `migrate()` once at startup. It creates tables, indexes, constraints, or loads
   scripts, and is safe to call from several processes at once.
3. Spend ledgers only: set `with_limits`, `with_reservation_ttl`, and `with_clock`.
4. Install it on the runner builder or the run config.

```rust
use std::sync::Arc;

use adk_core::{SpendKey, SpendLimits, SpendPeriod};
use adk_runner::Runner;
use adk_session::{
    PostgresActionLedger, PostgresApprovalStore, PostgresSessionService, PostgresSpendLedger,
};
use sqlx::postgres::PgPool;

let pool = PgPool::connect("postgres://localhost/app").await?;
let sessions = PostgresSessionService::from_pool(pool.clone());
sessions.migrate().await?;

let actions = PostgresActionLedger::from_pool(pool.clone());
let approvals = PostgresApprovalStore::from_pool(pool.clone());
let spend = PostgresSpendLedger::from_pool(pool).with_limits(
    SpendLimits::new().limit(SpendKey::org("payments").per(SpendPeriod::Day), 50_000_000),
);
actions.migrate().await?;
approvals.migrate().await?;
spend.migrate().await?;

let runner = Runner::builder()
    .app_name("payments")
    .agent(agent)
    .session_service(Arc::new(sessions))
    .action_ledger(Arc::new(actions))
    .approval_store(Arc::new(approvals))
    .spend_ledger(Arc::new(spend))
    .build()?;
```

Switching to Redis replaces the three constructors with `RedisActionLedger::new(url)`,
`RedisApprovalStore::new(url)`, and `RedisSpendLedger::new(url)`; the runner code does not
change. A run that carries its own store in `RunConfig` keeps it; the runner fills only the
stores the run leaves unset.

## How each backend stays atomic

| Backend | Reservation never overspends | Begin admits one caller | A decision is taken once |
|---------|------------------------------|-------------------------|--------------------------|
| In memory | One mutex | One write lock | One write lock |
| SQLite | `BEGIN IMMEDIATE` before the limit check | `INSERT ... ON CONFLICT DO NOTHING` | `DELETE ... RETURNING` |
| PostgreSQL | `SELECT ... FOR UPDATE` on the organization row | `INSERT ... ON CONFLICT DO NOTHING` | `DELETE ... RETURNING` |
| Redis | One Lua script per reservation, keys hash-tagged by organization | Lua create-if-absent | Lua get-and-delete |
| MongoDB | Compare-and-set on the organization document's sequence; no transactions, so standalone servers work | `insert_one` on the key as `_id` | `find_one_and_delete` |
| Neo4j | A transaction writes the organization node first, taking its write lock | Unique constraint on the key | A transaction writes the scope node first, then deletes the decision |
| Firestore | A transaction reads the organization document and its live holds; a conflicting reservation is retried | Create-only write (`exists = false`) | Transactional read and delete |

Backend-specific limits:

| Backend | Limit |
|---------|-------|
| Redis | Day buckets of committed spend expire 70 days after their last write, so only day and month windows read them; lifetime totals are kept forever. Sums are exact up to 2^53 micro-USD per window. |
| MongoDB | A reservation retries its compare-and-set up to 200 times under contention, then fails with `spend.ledger_unavailable`. |
| PostgreSQL | Timestamps are `TIMESTAMPTZ`, so action-record times round-trip at microsecond precision. |

## The no-duplicate guarantee

An `ActionLedger` makes a non-idempotent call execute at most once per idempotency key. A
replay after a crash, a timeout, or a cancelled run is answered from the record, or as
`outcome_unknown` when the call began and never completed. The guarantee holds only when
every process that can replay a session reads the same durable ledger.

| Session store | Action ledger | Duplicate execution after a restart |
|---------------|---------------|--------------------------------------|
| Any | `InMemoryActionLedger` | Possible: records die with the process |
| Any | A durable ledger on a store each process reaches separately (for example, one SQLite file per host) | Possible when another host replays the session |
| SQLite (one host) | `SqliteActionLedger` on the same database file | Prevented |
| PostgreSQL | `PostgresActionLedger` on the same database | Prevented |
| Redis | `RedisActionLedger` on the same server or cluster | Prevented; requires persistence (AOF or RDB) on the server |
| MongoDB | `MongoActionLedger` on the same deployment | Prevented |
| Neo4j | `Neo4jActionLedger` on the same database | Prevented |
| Firestore | `FirestoreActionLedger` on the same project | Prevented |

Keep the session store and the action ledger in the same database: one backup then restores
both to the same point, and a restored session never replays calls its ledger has forgotten.
The session event append and the ledger record are separate writes, so a crash between them
is reported as `outcome_unknown`, never as a second execution.

## Conformance suite

The suites live in `adk-core` behind the `conformance` feature. A third-party backend runs
them by implementing a harness and invoking one macro per store:

```toml
[dev-dependencies]
adk-core = { version = "3.0.0", features = ["conformance"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use std::sync::Arc;

use adk_core::conformance::ActionLedgerHarness;
use adk_core::{ActionLedger, async_trait};

struct MyHarness;

#[async_trait]
impl ActionLedgerHarness for MyHarness {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        // A new handle over the same data on every call, as a restart would create.
        let ledger = MyActionLedger::connect(&std::env::var("MY_URL").unwrap()).await.unwrap();
        Arc::new(ledger)
    }
}

adk_core::action_ledger_conformance_tests! {
    #[ignore = "requires a server at MY_URL"]
    harness = MyHarness,
}
```

| Suite | Cases |
|-------|-------|
| `spend_ledger_conformance_tests!` | reserve/commit/release accounting; each reservation settles once; per-key limits; day and month rollover at UTC boundaries (driven by a `ManualClock`); expired holds; 40 concurrent reservations across two handles; holds and settlements across a restart |
| `action_ledger_conformance_tests!` | begin/complete round trip; a recorded call answered without executing; a begun call read as `outcome_unknown` after a restart; 32 concurrent begins with one winner; write-once outcomes with one concurrent winner |
| `approval_store_conformance_tests!` | pending kept once, oldest first, per scope; `has_decision` does not consume and `take_decision` consumes once; a later decision replaces an earlier one; expiry; scope isolation; 16 concurrent takes with one winner; records across a restart; decisions carry edited arguments and messages; requests created once; 16 concurrent `replace_request` calls across two handles with one winner; listing by state, openness, application, and cursor; delegations stored, replaced, listed, and deleted; requests and delegations across a restart |
| `approval_service_conformance_tests!` | the `StoreApprovalService` over the store: expiry denies once; quorum of two stays pending after one approval; self-approval refused, also through a delegation; delegation only within its window; 12 concurrent decisions across two handles with one winner; edits restart the quorum and carry their arguments; escalation fires once; inbox by name, group, and delegation with paging; cancel and reject deny; a partly approved request across a restart |
| `audit_log_store_conformance_tests!` | contiguous appends read back unchanged; a gap or overlap is `audit_log.seq_conflict`; malformed batches are `audit_log.invalid_batch`; concurrent appends at one position across two handles have one winner; logs are independent; leaf and subtree hashes; reads past the end fail; checkpoints never exceed the log or fork, and a re-signed note replaces the old one; pruning keeps hashes; data across a restart |

Run the suites for the workspace backends:

| Backend | Command |
|---------|---------|
| In memory | `cargo nextest run -p adk-core --test store_conformance` |
| SQLite | `cargo nextest run -p adk-session --features sqlite --test store_conformance_sqlite` |
| PostgreSQL | `DATABASE_URL=postgres://postgres@localhost:5432/postgres cargo nextest run -p adk-session --features postgres --run-ignored only --test store_conformance_postgres` |
| Redis | `REDIS_URL=redis://localhost:6379 cargo nextest run -p adk-session --features redis --run-ignored only --test store_conformance_redis` |
| MongoDB | `MONGODB_URL=mongodb://localhost:27017 cargo nextest run -p adk-session --features mongodb --run-ignored only --test store_conformance_mongodb` |
| Neo4j | `NEO4J_URI=bolt://localhost:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=<password> cargo nextest run -p adk-session --features neo4j --run-ignored only --test store_conformance_neo4j` |
| Firestore | `FIRESTORE_EMULATOR_HOST=localhost:8080 cargo nextest run -p adk-session --features firestore --run-ignored only --test store_conformance_firestore` |

The `durable-stores` job in the nightly workflow starts PostgreSQL, Redis, MongoDB, Neo4j,
and the Firestore emulator and runs every live suite.

## Related

- [Spend Ledger](spend-ledger.md)
- [Audit Log](audit-log.md)
- [Tool Authorization](tool-authorization.md)
- [Tool Effects](../tools/tool-effects.md)
