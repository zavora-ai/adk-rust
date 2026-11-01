# Kernel Store Backends

The kernel keeps its own state in six stores beside the run, schedule and governance
stores of `adk-session`: the process side table, the agenda, the mounted drivers with their
parked calls, the [`/proc`](proc.md) activity ledger, the [partition](partitions.md)
table with its crossing ledger, and the cron watermarks, where each cron wake source left off
(spec 3.2 §7.1, §7.4, §7.8, §13 Q8). Every backend a store runs on
passes the store's conformance suite.

| Store | In-memory | SQLite (`sqlite`, default) | PostgreSQL (`postgres`) | Redis (`redis`) | MongoDB (`mongodb`) | Firestore (`firestore`) | Neo4j (`neo4j`) |
|---|---|---|---|---|---|---|---|
| `ProcessStore` | `InMemoryProcessStore` | `SqliteProcessStore` | `PostgresProcessStore` | `RedisProcessStore` | `MongoProcessStore` | `FirestoreProcessStore` | `Neo4jProcessStore` |
| `AgendaStore` | `InMemoryAgendaStore` | `SqliteAgendaStore` | `PostgresAgendaStore` | `RedisAgendaStore` | `MongoAgendaStore` | `FirestoreAgendaStore` | `Neo4jAgendaStore` |
| `DriverStore` (`drivers`) | `InMemoryDriverStore` | `SqliteDriverStore` | `PostgresDriverStore` | `RedisDriverStore` | `MongoDriverStore` | `FirestoreDriverStore` | `Neo4jDriverStore` |
| `LedgerStore` (`proc`) | `InMemoryLedgerStore` | `SqliteLedgerStore` | `PostgresLedgerStore` | `RedisLedgerStore` | `MongoLedgerStore` | `FirestoreLedgerStore` | `Neo4jLedgerStore` |
| `PartitionStore` | `InMemoryPartitionStore` | `SqlitePartitionStore` | `PostgresPartitionStore` | `RedisPartitionStore` | `MongoPartitionStore` | `FirestorePartitionStore` | `Neo4jPartitionStore` |
| `CronWatermarkStore` | `InMemoryCronWatermarkStore` | `SqliteCronWatermarkStore` | `PostgresCronWatermarkStore` | `RedisCronWatermarkStore` | `MongoCronWatermarkStore` | `FirestoreCronWatermarkStore` | `Neo4jCronWatermarkStore` |

## Every store at once: `KernelStores`

`adk_kernel::stores::KernelStores` holds every stateful store a kernel keeps, opened together
on one backend: `adk-session`'s run, schedule, approval, action-ledger, spend-ledger and
kill-switch stores; the kernel's process, agenda, partition, cron watermark and agent stores;
devices, control-plane settings, drivers (`drivers`), the activity ledger (`proc`), chat
pairings (`channels`) and the charter stores under `charter` (`charter`). `KernelBuilder::stores`
and `TenantKernelBuilder::stores` take it.

| Opened with | Backend |
|---|---|
| `KernelStores::in_memory()`, `open("memory:")` | Every store in memory |
| `open("sqlite://<path>?mode=rwc")`, `from_sqlite_pool(pool)` | SQLite, one pool, a migration table per store |
| `open("postgres://…")`, `from_postgres_pool(pool)` | PostgreSQL, one pool |
| `open("redis://…")`, `from_redis_client(client)` | Redis, one client |
| `open("mongodb://…/<database>")`, `from_mongo_database(db)` | MongoDB |
| `open("neo4j://<user>:<password>@<host>")`, `from_neo4j_graph(graph)` | Neo4j |
| `open("firestore://<project>")`, `from_firestore_db(db)` | Firestore |

Devices, control-plane settings, chat pairings and the charter stores have no MongoDB,
Firestore or Neo4j backend yet. A bundle on those backends keeps them in memory, logs a warning
for each, and names them in `in_memory_stores()`.

`KernelBuilder::stores` fills each store the builder was not given: a store set on the builder,
before or after, wins. It mirrors the kill switch over the bundle's governance store unless the
builder or its configuration already has one, and gives the control plane the bundle's
settings store unless the configuration names its own. The approval service, the driver
supervisor and the activity ledger stay the host's to build over `approvals`, `drivers` and
`ledger`, with its own routing, home and key.

```rust
use std::sync::Arc;

use adk_core::{ApprovalPolicy, ApproverSelector, StoreApprovalService};
use adk_kernel::stores::KernelStores;
use adk_kernel::{Kernel, KernelConfig};
use adk_runner::Runner;

async fn kernel(runner: Arc<Runner>) -> Result<Kernel, Box<dyn std::error::Error>> {
    let stores = KernelStores::open("sqlite://data/kernel.db?mode=rwc").await?;
    let approvals = StoreApprovalService::new(stores.approvals.clone())
        .with_run_store(stores.runs.clone())
        .with_router(ApprovalPolicy::new([ApproverSelector::group("operator")]));
    let kernel = Kernel::builder()
        .config(KernelConfig::default().with_app_name("ops"))
        .stores(&stores)
        .require_durable()
        .approval_service(Arc::new(approvals))
        .agent("ops", runner)
        .build()?;
    Ok(kernel)
}
```

### Requiring durable stores

A store the builder is given neither directly nor through `KernelBuilder::stores` is kept in
memory: the process table, agenda, partition table, cron watermarks, agent table, kill switch and
control-plane settings. The builder logs a warning for each such store (the settings store only
when the kernel serves a control plane), and `Kernel::store_uses()` reports every store with
where it came from (`set`, `bundle` or `defaulted`) and whether it is held in memory.

`KernelBuilder::require_durable()` turns the warnings into a refusal: `build` returns a
configuration error naming each store that would be kept in memory — one the builder
defaulted, one the bundle keeps in memory, or a kill switch that is not mirrored — with the
setter that fixes it.

| Host | Requires durable stores |
|---|---|
| `adk-rust up` | Yes, unless `--ephemeral`: on `harness.db` or the `--store` backend |
| `adk-website serve`, `adk-website agency` | Yes: every store in `data/kernel.db` |
| A kernel built in code | When the builder calls `require_durable()` |

A [tenant kernel](managed.md#cron-wakes-missed-while-a-tenant-was-down) keeps its cron
watermarks in the tenant's own store (`TenantKernelBuilder::watermark_store`).

### Cron sleepers

The kernel fires a sleeper's cron wake itself and keeps each expression's cursor in its
`CronWatermarkStore` under `kernel:<expression>` (`KernelBuilder::watermark_store`; in memory by
default). `adk-rust up` opens it on `harness.db`, or in the `--store` backend every replica shares
(spec 3.2 §7.4):

| Occurrences | What fires |
|---|---|
| Due while a kernel watched the expression | The latest since the last scheduler tick, to every sleeper on it |
| After the cursor and before the kernel's first tick since it started | The missed ones, one at a time, ranked from the latest as the widest `missed` policy of the sleepers plans them; each sleeper wakes for those its own policy picks, and a recurring series takes the next one when its successor sleeps |
| Before a series began, or due while a kernel ran and nothing slept on the expression | None |

The cursor moves past each occurrence after its wake is submitted, and every occurrence keeps its
key (`cron:kernel:<expression>:<time>`), so a restart or a second replica never fires one twice.

### Cron triggers

A standing trigger on a cron expression (`KernelBuilder::trigger` with `Wake::Cron`) fires
through a watermarked `CronWakeSource` the kernel registers itself: one per expression, named
`trigger-cron:<expression>`, planned with the widest `missed` policy of the expression's
triggers, its cursor in the same `CronWatermarkStore`. Every trigger on the expression matches
each tick once. A registered source that ticks on the expression — a
`CronWakeSource::watermarked` one reports it through `WakeSource::cron_expression` — takes the
kernel's place (spec 3.5 §7.11).

There is no file backend: no durable store is file-backed, and the CLI keeps its state in
`harness.db`. Vertex AI holds sessions only, so it has no kernel backend.

## Choosing a backend

| Where the kernel runs | Backend | How it is opened |
|---|---|---|
| One machine, `adk-rust up` | SQLite | `harness.db` in the kernel home, on the harness's pool with a migration table per store |
| Replicas, `adk-rust up --store` | PostgreSQL or Redis | The `--store` URL, which also holds sessions, approvals, runs, modes, standing rules and plan checkpoints; on PostgreSQL, long-term memory too (`PostgresFactStore`, spec 3.3 §12 phase 3), which Redis does not keep |
| A kernel built in code | Any | `KernelStores::open(url)` and `KernelBuilder::stores`, or each store on the builder as below |

`--store` takes PostgreSQL and Redis URLs only: it also keeps the harness's modes and
standing rules, which have no MongoDB, Firestore or Neo4j backend.

## MongoDB, Firestore and Neo4j

MongoDB, Firestore and Neo4j are **Beta**, as the rest of `adk-kernel`: each implements the whole
contract of every kernel store — process, agenda, driver, ledger, partition and cron watermark —
and passes their conformance suites against a live server in CI. CI's nightly `durable-stores` job
runs them against a MongoDB replica set and a standalone server, Neo4j 5 and the Firestore
emulator. Their storage layout follows the Beta contract of [STABILITY.md](../../../STABILITY.md).

Each feature also turns on the same backend of `adk-session`'s run, schedule and governance
stores, and each of those kernel stores takes the client handle those stores take, so one
client serves them all.

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
| A crossing is recorded once and never changed | `insertOne` on its id | Firestore's create, which fails when the document exists | `CREATE` under a uniqueness constraint on the id |
| Granting twice keeps one grant | An upsert on `{run, fingerprint}` | One document named by the digest of run and fingerprint | `MERGE` on a key of run and fingerprint |
| A cron watermark never moves back | One `findOneAndUpdate` upsert with `$max`, returning the document after the update | A read and a write of the later cursor in one serializable transaction | One `MERGE` whose `SET` keeps the later cursor, under Cypher's write lock |
| Transactions needed | None: a standalone `mongod` serves every store | Transactions for compare-and-set, deletes and appends | Explicit transactions for agenda transitions |

| Limit | Backend |
|---|---|
| An id over 700 bytes is refused, because a document ID holds at most 1,500 bytes and the stores name documents by its hex | Firestore |
| Queries use equality filters or one range filter, which the automatic single-field indexes serve, so no composite index is needed; `list` reads every row its equality filters match and orders in memory, and the ledger walks rows by number or by time and filters in memory, as Redis does | Firestore |
| The ledger's counter and its row are two writes, so a reader paging with `after` while appends land can pass a row whose number was taken before the last row it read, as with a PostgreSQL sequence | MongoDB |
| Every call runs on a task of its own, because a neo4rs connection dropped mid-exchange is left out of step | Neo4j |
| A node a concurrent `delete` removes while a list reads it is left out of the list: Neo4j reads committed data, so its properties read as null, as if the list had come after the delete | Neo4j |

## Checking a backend

The conformance suites — `process_store_conformance_tests!`, `agenda_store_conformance_tests!`,
`driver_store_conformance_tests!`, `ledger_store_conformance_tests!`,
`partition_store_conformance_tests!` and `cron_watermark_store_conformance_tests!` (feature
`conformance`) — check a backend against the contract the kernel relies on. The MongoDB,
Firestore and Neo4j suites are `#[ignore]`d and need a server:

```bash
MONGODB_URL=mongodb://localhost:27017 \
  cargo nextest run -p adk-kernel --features mongodb --run-ignored only \
  --test integration -E 'test(/^store_conformance_mongodb::/)'

NEO4J_URI=bolt://localhost:7687 NEO4J_PASSWORD=secret \
  cargo nextest run -p adk-kernel --features neo4j --run-ignored only \
  --test integration -E 'test(/^store_conformance_neo4j::/)'

FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 \
  cargo nextest run -p adk-kernel --features firestore --run-ignored only \
  --test integration -E 'test(/^store_conformance_firestore::/)'
```
