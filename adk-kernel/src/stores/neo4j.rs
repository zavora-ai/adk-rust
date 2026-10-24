//! The Neo4j [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `neo4j`), and the helpers the
//! kernel's other Neo4j stores share.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use neo4rs::{Graph, Neo4jErrorKind, Query, Row, Txn, query};

use super::{ProcessFilter, ProcessStore, SpendScope, day_number, stored_micro_usd, stored_total};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// The backend name in [`KernelError::Store`].
pub(super) const BACKEND: &str = "neo4j";

/// Attempts at a statement that fails with a transient error, such as a detected deadlock,
/// before the error is returned.
const ATTEMPTS: u32 = 30;

/// Schema codes that mean an `IF NOT EXISTS` statement raced another one and lost.
const SCHEMA_ALREADY_EXISTS: [&str; 4] = [
    "Neo.ClientError.Schema.EquivalentSchemaRuleAlreadyExists",
    "Neo.ClientError.Schema.ConstraintAlreadyExists",
    "Neo.ClientError.Schema.IndexAlreadyExists",
    "Neo.ClientError.Schema.ConstraintWithNameAlreadyExists",
];

/// Two `MERGE`s creating one node at once: the loser's retry finds the node.
const CONSTRAINT_VIOLATION: &str = "Neo.ClientError.Schema.ConstraintValidationFailed";

pub(super) fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: BACKEND, reason: format!("could not {action}: {error}") }
}

/// Connects to the server at `uri`.
pub(super) async fn connect(uri: &str, user: &str, password: &str) -> Result<Graph> {
    Graph::new(uri, user, password).await.map_err(|error| store_error("connect to Neo4j", &error))
}

/// The hex of `text`'s UTF-8 bytes, which orders as the bytes do whatever the database's
/// string collation, so `ORDER BY` on it follows byte order as on every backend.
pub(super) fn order_key(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// Runs one store call to completion on a task of its own.
///
/// neo4rs connections are not cancellation-safe: a call dropped between sending a bolt
/// message and reading its response returns the connection to the pool with that response
/// unread, and every later response on it is one behind. The kernel drops store calls when
/// a request or a worker attempt ends, so every call finishes its exchange on its own task
/// and a dropped caller only loses the result.
pub(super) async fn detached<T: Send + 'static>(
    call: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    match tokio::spawn(call).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(store_error("finish a Neo4j call", &error)),
    }
}

fn neo4j_code(error: &neo4rs::Error) -> Option<&str> {
    match error {
        neo4rs::Error::Neo4j(error) => Some(error.code()),
        _ => None,
    }
}

/// Whether running the statement again can succeed: a transient error, such as a detected
/// deadlock, or a `MERGE` that lost a race to create its node.
fn retryable(error: &neo4rs::Error) -> bool {
    match error {
        neo4rs::Error::Neo4j(neo4j) => {
            neo4j.kind() == Neo4jErrorKind::Transient || neo4j.code() == CONSTRAINT_VIOLATION
        }
        _ => false,
    }
}

/// Small randomized pause before retry `attempt`, growing with the attempt number.
fn backoff(attempt: u32) -> Duration {
    let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 10;
    Duration::from_millis(u64::from(attempt.min(20)) * 5 + jitter)
}

/// Runs `attempt` until it succeeds, fails with an error running it again cannot fix, or
/// has failed [`ATTEMPTS`] times.
pub(super) async fn retry<T, F, Fut>(action: &str, mut attempt: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, neo4rs::Error>>,
{
    let mut tries = 1;
    loop {
        match attempt().await {
            Err(error) if retryable(&error) && tries < ATTEMPTS => {
                tokio::time::sleep(backoff(tries)).await;
                tries += 1;
            }
            result => return result.map_err(|error| store_error(action, &error)),
        }
    }
}

/// Every row of an auto-commit `query`.
async fn fetch(graph: &Graph, query: Query) -> std::result::Result<Vec<Row>, neo4rs::Error> {
    let mut stream = graph.execute(query).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next().await? {
        rows.push(row);
    }
    Ok(rows)
}

/// Every row of `query`, run as an auto-commit statement and retried as [`retry`] does.
pub(super) async fn rows(graph: &Graph, query: Query, action: &str) -> Result<Vec<Row>> {
    retry(action, || fetch(graph, query.clone())).await
}

/// Every row of `query`, run inside `txn`.
pub(super) async fn txn_rows(
    txn: &mut Txn,
    query: Query,
) -> std::result::Result<Vec<Row>, neo4rs::Error> {
    let mut stream = txn.execute(query).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next(&mut *txn).await? {
        rows.push(row);
    }
    Ok(rows)
}

/// Column `name` of `row`.
pub(super) fn column<T: serde::de::DeserializeOwned>(row: &Row, name: &str) -> Result<T> {
    row.get::<T>(name).map_err(|error| KernelError::Store {
        backend: BACKEND,
        reason: format!("a stored row's column `{name}` is unreadable: {error}"),
    })
}

/// The integer column `name` of the first row, 0 when there is none.
pub(super) fn count(rows: &[Row], name: &str) -> Result<i64> {
    rows.first().map_or(Ok(0), |row| column::<i64>(row, name))
}

/// JSON text a store wrote, read back.
pub(super) fn json<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| KernelError::Store {
        backend: BACKEND,
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// Runs idempotent schema statements, tolerating a concurrent `migrate` that created the
/// same rule first.
pub(super) async fn run_schema(graph: &Graph, statements: &[&str]) -> Result<()> {
    for statement in statements {
        let mut tries = 1;
        loop {
            match graph.run(query(statement)).await {
                Ok(()) => break,
                Err(error)
                    if neo4j_code(&error)
                        .is_some_and(|code| SCHEMA_ALREADY_EXISTS.contains(&code)) =>
                {
                    break;
                }
                Err(error) if tries < ATTEMPTS && retryable(&error) => {
                    tokio::time::sleep(backoff(tries)).await;
                    tries += 1;
                }
                Err(error) => return Err(store_error("create the Neo4j schema", &error)),
            }
        }
    }
    Ok(())
}

/// The `LIMIT` clause of a list, with its parameter.
pub(super) fn limit_clause(limit: Option<usize>) -> (String, Option<i64>) {
    match limit {
        Some(limit) => {
            (" LIMIT $limit".to_string(), Some(i64::try_from(limit).unwrap_or(i64::MAX)))
        }
        None => (String::new(), None),
    }
}

/// The `WHERE` clause joining `conditions`, empty when there are none.
pub(super) fn where_clause(conditions: &[&str]) -> String {
    if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    }
}

const PUT: &str = "\
MERGE (p:AdkKernelProcess {pid: $pid}) \
SET p = {pid: $pid, order_key: $order_key, partition: $partition, queue: $queue, kind: $kind, \
    labels: $labels, suspend_requested: $suspend_requested, parent: $parent, wake: $wake, \
    suspended_by: $suspended_by, agenda_item: $agenda_item}";

const COLUMNS: &str = "\
RETURN p.pid AS pid, p.parent AS parent, p.partition AS partition, p.queue AS queue, \
    p.kind AS kind, p.wake AS wake, p.suspended_by AS suspended_by, \
    p.suspend_requested AS suspend_requested, p.agenda_item AS agenda_item, p.labels AS labels";

const ADD_SPEND: &str = "\
MERGE (s:AdkKernelSpend {key: $key}) \
ON CREATE SET s.pid = $pid, s.partition = $partition, s.day = $day, s.micro_usd = 0 \
SET s.micro_usd = s.micro_usd + $amount";

/// A [`ProcessStore`] in Neo4j, shared by every replica (spec 3.2 §7.1, §13 Q8, feature
/// `neo4j`). **Alpha** until its live conformance suite passes in CI.
///
/// | Label | Key (unique) | Properties |
/// |-------|--------------|------------|
/// | `AdkKernelProcess` | `pid` | `order_key` (the hex of the pid), `partition`, `queue`, `suspend_requested` and, when set, `parent` and `agenda_item`, the properties `list` filters on; `kind`, `labels` and, when set, `wake` and `suspended_by` as JSON text |
/// | `AdkKernelSpend` | `key`: the hex of the pid and the partition with the day | `pid`, `partition`, `day` (days from the common era) and `micro_usd` |
///
/// `list` orders by `order_key`, so pids order by their UTF-8 bytes whatever the database's
/// collation. `set_wake`, `set_queue` and `set_suspend_requested` each set one property, so
/// writers changing different fields of one row never lose each other's update.
/// `add_spend` adds to the stored total in one statement, under the write lock Cypher takes
/// on a property updated from its own value, so concurrent additions all count; deleting a
/// process keeps its spend. Every call runs on a task of its own, because a neo4rs
/// connection dropped mid-exchange is left out of step. Call [`migrate`](Self::migrate)
/// once per database to create the constraints and indexes.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::Neo4jProcessStore::connect("bolt://localhost:7687", "neo4j", "password").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jProcessStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jProcessStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jProcessStore").finish_non_exhaustive()
    }
}

impl Neo4jProcessStore {
    /// A store over `graph`, such as the connection `adk_session::Neo4jRunStore::from_graph`
    /// uses, so the kernel's nodes sit beside the run, schedule and governance stores.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Connects to the server at `uri`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached.
    pub async fn connect(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password).await?))
    }

    /// The connection the store uses.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Creates the constraints and indexes the store relies on; one that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        let graph = self.graph.clone();
        detached(async move {
            run_schema(
                &graph,
                &[
                    "CREATE CONSTRAINT adk_kernel_process_pid IF NOT EXISTS \
                     FOR (p:AdkKernelProcess) REQUIRE p.pid IS UNIQUE",
                    "CREATE INDEX adk_kernel_process_order IF NOT EXISTS \
                     FOR (p:AdkKernelProcess) ON (p.order_key)",
                    "CREATE INDEX adk_kernel_process_partition IF NOT EXISTS \
                     FOR (p:AdkKernelProcess) ON (p.partition)",
                    "CREATE INDEX adk_kernel_process_queue IF NOT EXISTS \
                     FOR (p:AdkKernelProcess) ON (p.queue)",
                    "CREATE INDEX adk_kernel_process_parent IF NOT EXISTS \
                     FOR (p:AdkKernelProcess) ON (p.parent)",
                    "CREATE CONSTRAINT adk_kernel_spend_key IF NOT EXISTS \
                     FOR (s:AdkKernelSpend) REQUIRE s.key IS UNIQUE",
                    "CREATE INDEX adk_kernel_spend_pid IF NOT EXISTS \
                     FOR (s:AdkKernelSpend) ON (s.pid)",
                    "CREATE INDEX adk_kernel_spend_partition IF NOT EXISTS \
                     FOR (s:AdkKernelSpend) ON (s.partition)",
                ],
            )
            .await
        })
        .await
    }

    /// Runs `update`, which returns `matched`, on the row of `pid`, failing when there is
    /// none.
    async fn update(&self, pid: &str, update: Query) -> Result<()> {
        let graph = self.graph.clone();
        let matched = detached(async move {
            count(&rows(&graph, update, "update the process table").await?, "matched")
        })
        .await?;
        if matched == 0 {
            return Err(KernelError::NotFound { pid: pid.into() });
        }
        Ok(())
    }
}

/// Reads a row back into the [`ProcessMeta`] that was put.
fn decode(row: &Row) -> Result<ProcessMeta> {
    let optional = |name: &str| column::<Option<String>>(row, name);
    let kind: String = column(row, "kind")?;
    let labels: String = column(row, "labels")?;
    Ok(ProcessMeta {
        pid: column(row, "pid")?,
        parent: optional("parent")?,
        partition: PartitionId::new(column::<String>(row, "partition")?),
        queue: QueueClass::from_name(&column::<String>(row, "queue")?),
        kind: json("process kind", &kind)?,
        wake: optional("wake")?.map(|wake| json("wake set", &wake)).transpose()?,
        suspended_by: optional("suspended_by")?
            .map(|suspension| json("suspension", &suspension))
            .transpose()?,
        suspend_requested: column::<Option<bool>>(row, "suspend_requested")?.unwrap_or(false),
        agenda_item: optional("agenda_item")?,
        labels: json("label set", &labels)?,
    })
}

#[async_trait]
impl ProcessStore for Neo4jProcessStore {
    async fn put(&self, meta: ProcessMeta) -> Result<()> {
        let statement = query(PUT)
            .param("pid", meta.pid.as_str())
            .param("order_key", order_key(&meta.pid))
            .param("partition", meta.partition.as_str())
            .param("queue", meta.queue.as_str())
            .param("kind", serde_json::to_string(&meta.kind)?)
            .param("labels", serde_json::to_string(&meta.labels)?)
            .param("suspend_requested", meta.suspend_requested)
            .param("parent", meta.parent.clone())
            .param("wake", meta.wake.as_ref().map(serde_json::to_string).transpose()?)
            .param(
                "suspended_by",
                meta.suspended_by.as_ref().map(serde_json::to_string).transpose()?,
            )
            .param("agenda_item", meta.agenda_item.clone());
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write the process table").await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>> {
        let statement =
            query(&format!("MATCH (p:AdkKernelProcess {{pid: $pid}}) {COLUMNS}")).param("pid", pid);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the process table").await?.first().map(decode).transpose()
        })
        .await
    }

    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        let mut conditions = Vec::new();
        if filter.partition.is_some() {
            conditions.push("p.partition = $partition");
        }
        if filter.queue.is_some() {
            conditions.push("p.queue = $queue");
        }
        if filter.parent.is_some() {
            conditions.push("p.parent = $parent");
        }
        if filter.suspend_requested.is_some() {
            conditions.push("p.suspend_requested = $suspend_requested");
        }
        let (limit, limit_value) = limit_clause(filter.limit);
        let statement = query(&format!(
            "MATCH (p:AdkKernelProcess){} {COLUMNS} ORDER BY p.order_key{limit}",
            where_clause(&conditions)
        ))
        .param("partition", filter.partition.as_ref().map(|partition| partition.as_str()))
        .param("queue", filter.queue.as_ref().map(|queue| queue.as_str()))
        .param("parent", filter.parent.clone())
        .param("suspend_requested", filter.suspend_requested)
        .param("limit", limit_value);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "list processes").await?.iter().map(decode).collect()
        })
        .await
    }

    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()> {
        let statement = query(
            "MATCH (p:AdkKernelProcess {pid: $pid}) SET p.wake = $wake \
             RETURN count(p) AS matched",
        )
        .param("pid", pid)
        .param("wake", wake.as_ref().map(serde_json::to_string).transpose()?);
        self.update(pid, statement).await
    }

    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()> {
        let statement = query(
            "MATCH (p:AdkKernelProcess {pid: $pid}) SET p.queue = $queue \
             RETURN count(p) AS matched",
        )
        .param("pid", pid)
        .param("queue", queue.as_str());
        self.update(pid, statement).await
    }

    async fn delete(&self, pid: &str) -> Result<bool> {
        let statement =
            query("MATCH (p:AdkKernelProcess {pid: $pid}) DELETE p RETURN count(*) AS deleted")
                .param("pid", pid);
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(&rows(&graph, statement, "delete from the process table").await?, "deleted")?
                > 0)
        })
        .await
    }

    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> Result<()> {
        let statement = query(
            "MATCH (p:AdkKernelProcess {pid: $pid}) SET p.suspend_requested = $requested \
             RETURN count(p) AS matched",
        )
        .param("pid", pid)
        .param("requested", requested);
        self.update(pid, statement).await
    }

    async fn add_spend(
        &self,
        pid: &str,
        partition: &PartitionId,
        day: NaiveDate,
        delta: SpendDelta,
    ) -> Result<()> {
        let day = day_number(day);
        let statement = query(ADD_SPEND)
            .param("key", format!("{}:{}:{day}", order_key(pid), order_key(partition.as_str())))
            .param("pid", pid)
            .param("partition", partition.as_str())
            .param("day", day)
            .param("amount", stored_micro_usd(BACKEND, delta)?);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "add to the spend totals").await?;
            Ok(())
        })
        .await
    }

    async fn spend(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<u64> {
        let (field, key) = match scope {
            SpendScope::Process(pid) => ("pid", pid.as_str()),
            SpendScope::Partition(partition) => ("partition", partition.as_str()),
        };
        let statement = query(&format!(
            "MATCH (s:AdkKernelSpend) WHERE s.{field} = $key AND s.day >= $from AND s.day <= $to \
             RETURN coalesce(sum(s.micro_usd), 0) AS total"
        ))
        .param("key", key)
        .param("from", day_number(from))
        .param("to", day_number(to_inclusive));
        let graph = self.graph.clone();
        let total = detached(async move {
            count(&rows(&graph, statement, "sum the spend totals").await?, "total")
        })
        .await?;
        stored_total(BACKEND, total)
    }
}
