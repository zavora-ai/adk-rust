# Kernel Store Backends

The kernel keeps its own state in four stores beside the run, schedule and governance
stores of `adk-session`: the process side table, the agenda, the mounted drivers with their
parked calls, and the [`/proc`](proc.md) activity ledger (spec 3.2 §7.1, §13 Q8). Every
store runs on every backend that holds the run, schedule and governance stores, and every
backend passes the store's conformance suite.

| Store | In-memory | SQLite (`sqlite`, default) | PostgreSQL (`postgres`) | Redis (`redis`) | MongoDB (`mongodb`, Alpha) | Firestore (`firestore`, Alpha) | Neo4j (`neo4j`, Alpha) |
|---|---|---|---|---|---|---|---|
| `ProcessStore` | `InMemoryProcessStore` | `SqliteProcessStore` | `PostgresProcessStore` | `RedisProcessStore` | `MongoProcessStore` | `FirestoreProcessStore` | `Neo4jProcessStore` |
| `AgendaStore` | `InMemoryAgendaStore` | `SqliteAgendaStore` | `PostgresAgendaStore` | `RedisAgendaStore` | `MongoAgendaStore` | `FirestoreAgendaStore` | `Neo4jAgendaStore` |
| `DriverStore` (`drivers`) | `InMemoryDriverStore` | `SqliteDriverStore` | `PostgresDriverStore` | `RedisDriverStore` | `MongoDriverStore` | `FirestoreDriverStore` | `Neo4jDriverStore` |
| `LedgerStore` (`proc`) | `InMemoryLedgerStore` | `SqliteLedgerStore` | `PostgresLedgerStore` | `RedisLedgerStore` | `MongoLedgerStore` | `FirestoreLedgerStore` | `Neo4jLedgerStore` |

There is no file backend: no durable store is file-backed, and the CLI keeps its state in
`harness.db`. Vertex AI holds sessions only, so it has no kernel backend.

## Choosing a backend

| Where the kernel runs | Backend | How it is opened |
|---|---|---|
| One machine, `adk-rust up` | SQLite | `harness.db` in the kernel home, on the harness's pool with a migration table per store |
| Replicas, `adk-rust up --store` | PostgreSQL or Redis | The `--store` URL, which also holds sessions, approvals, runs, modes, standing rules and plan checkpoints |
| A kernel built in code on MongoDB, Firestore or Neo4j | The Alpha backends | `KernelBuilder` with the stores below |

`--store` takes PostgreSQL and Redis URLs only: it also keeps the harness's modes and
standing rules, which have no MongoDB, Firestore or Neo4j backend.

## Alpha backends

MongoDB, Firestore and Neo4j are **Alpha**: each implements the whole contract of every
kernel store and carries its conformance suites, but stays Alpha until those suites pass
against a live server in CI, so its storage layout may still change in a minor release
without a migration ([STABILITY.md](../../../STABILITY.md)). CI's nightly `durable-stores`
job runs them against a MongoDB replica set and a standalone server, Neo4j 5 and the
Firestore emulator.

Each feature also turns on the same backend of `adk-session`'s run, schedule and governance
stores, and every kernel store takes the client handle those stores take, so one client
serves them all.

```toml
[dependencies]
adk-kernel = { version = "3.0.0", features = ["mongodb"] }
adk-runner = "3.0.0"
adk-session = { version = "3.0.0", features = ["mongodb"] }
```

```rust
use std::sync::Arc;

use adk_kernel::proc::{Ledger, LedgerKey, MongoLedgerStore};
use adk_kernel::{Kernel, KernelConfig, MongoAgendaStore, MongoProcessStore};
use adk_runner::Runner;
use adk_session::{MongoGovernanceStateStore, MongoRunStore, MongoScheduleStore};

async fn kernel(runner: Arc<Runner>) -> Result<Kernel, Box<dyn std::error::Error>> {
    let processes = MongoProcessStore::connect("mongodb://localhost:27017", "adk").await?;
    let db = processes.database().clone();
    let runs = MongoRunStore::from_database(db.clone());
    let schedules = MongoScheduleStore::from_database(db.clone());
    let governance = MongoGovernanceStateStore::from_database(db.clone());
    let agenda = MongoAgendaStore::from_database(db.clone());
    let ledger = MongoLedgerStore::from_database(db);
    runs.migrate().await?;
    schedules.migrate().await?;
    governance.migrate().await?;
    processes.migrate().await?;
    agenda.migrate().await?;
    ledger.migrate().await?;
    let kernel = Kernel::builder()
        .config(KernelConfig::default().with_app_name("ops"))
        .governance(KernelConfig::mirrored_governance(Arc::new(governance)))
        .run_store(Arc::new(runs))
        .schedule_store(Arc::new(schedules))
        .process_store(Arc::new(processes))
        .agenda_store(Arc::new(agenda))
        .ledger(Ledger::new(Arc::new(ledger), LedgerKey::from_bytes([7; 32])))
        .agent("ops", runner)
        .build()?;
    Ok(kernel)
}
```

Keep the `LedgerKey` in the kernel home, as `adk-rust up` does with `ledger.key`; the fixed
bytes above only keep the example short.

| Backend | Opened with | Shares | `migrate` |
|---|---|---|---|
| MongoDB | `connect(connection_string, database)`, `from_database(db)` | The `mongodb::Database` of `MongoRunStore::from_database` | Creates the indexes `list` and the sums read |
| Firestore | `connect(project_id)` with Application Default Credentials, `from_db(db)` | The `FirestoreDb` of `FirestoreRunStore::from_db`; `adk_session::firestore_stores::connect_firestore_emulator` for the emulator | Nothing to create |
| Neo4j | `connect(uri, user, password)`, `from_graph(graph)` | The `neo4rs::Graph` of `Neo4jRunStore::from_graph` | Creates the uniqueness constraints and indexes |

### How each backend keeps the contract

| Contract | MongoDB | Firestore | Neo4j |
|---|---|---|---|
| A row reads back exactly as written | Structured fields as JSON text beside the fields a filter reads | The same, one document per row | The same, one node per row |
| Byte order of ids | `_id` under MongoDB's binary string comparison | Documents named by the hex of the id, so name order is byte order | An `order_key` property, the hex of the id |
| Concurrent field updates keep both | One `$set` per field | One field-masked write with an exists precondition | One `SET` per property |
| Concurrent spend additions all count | One `$inc` upsert | Firestore's server-side increment | `SET s.micro_usd = s.micro_usd + $amount` under Cypher's write lock |
| An agenda item moves from a state once | An update filtered on `_id` and `state` | A read and a write in one serializable transaction | A write lock on the node before the state is read |
| Ledger rows take increasing numbers | A `$inc` counter, then the insert | A counter read and raised in the row's transaction | The counter raised and the row created in one statement |
| Transactions needed | None: a standalone `mongod` serves every store | Transactions for compare-and-set, deletes and appends | Explicit transactions for agenda transitions |

| Limit | Backend |
|---|---|
| An id over 700 bytes is refused, because a document ID holds at most 1,500 bytes and the stores name documents by its hex | Firestore |
| Queries use equality filters or one range filter, which the automatic single-field indexes serve, so no composite index is needed; `list` reads every row its equality filters match and orders in memory, and the ledger walks rows by number or by time and filters in memory, as Redis does | Firestore |
| The ledger's counter and its row are two writes, so a reader paging with `after` while appends land can pass a row whose number was taken before the last row it read, as with a PostgreSQL sequence | MongoDB |
| Every call runs on a task of its own, because a neo4rs connection dropped mid-exchange is left out of step | Neo4j |

## Checking a backend

The conformance suites — `process_store_conformance_tests!`, `agenda_store_conformance_tests!`,
`driver_store_conformance_tests!` and `ledger_store_conformance_tests!` (feature
`conformance`) — check a backend against the contract the kernel relies on. The Alpha suites
are `#[ignore]`d and need a server:

```bash
MONGODB_URL=mongodb://localhost:27017 \
  cargo nextest run -p adk-kernel --features mongodb --run-ignored only \
  --test store_conformance_mongodb

NEO4J_URI=bolt://localhost:7687 NEO4J_PASSWORD=secret \
  cargo nextest run -p adk-kernel --features neo4j --run-ignored only \
  --test store_conformance_neo4j

FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 \
  cargo nextest run -p adk-kernel --features firestore --run-ignored only \
  --test store_conformance_firestore
```
