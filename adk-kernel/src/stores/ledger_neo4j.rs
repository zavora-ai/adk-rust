//! The Neo4j [`LedgerStore`] (spec 3.2 §7.10, §13 Q8, features `proc` and `neo4j`).

use async_trait::async_trait;
use neo4rs::{Graph, Row, query};

use super::day_number;
use super::neo4j::{
    BACKEND, column, connect, detached, json, limit_clause, order_key, rows, run_schema,
    where_clause,
};
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::proc::{
    LedgerEntry, LedgerQuery, LedgerStore, Observation, ObservationQuery, PassState,
};

/// Takes the next sequence number and creates the row under it in one statement: the
/// counter's `SET` from its own value write-locks it, so appends commit in number order.
const APPEND: &str = "\
MERGE (c:AdkKernelLedgerCounter {name: 'ledger'}) \
SET c.seq = coalesce(c.seq, 0) + 1 \
CREATE (r:AdkKernelLedgerRow {seq: c.seq, ts_ms: $ts_ms, partition: $partition, agent: $agent, \
    kind: $kind, row: $row}) \
RETURN r.seq AS seq";

const PUT_OBSERVATION: &str = "\
MERGE (o:AdkKernelObservation {id: $id}) \
SET o.order_key = $order_key, o.partition = $partition, o.as_of = $as_of, o.body = $body";

const PUT_PASS_STATE: &str = "\
MERGE (s:AdkKernelPassState {partition: $partition}) \
SET s.order_key = $order_key, s.body = $body";

/// A [`LedgerStore`] in Neo4j, shared by every replica (spec 3.2 §7.1, §7.10, §13 Q8,
/// features `proc` and `neo4j`).
///
/// | Label | Key (unique) | Properties |
/// |-------|--------------|------------|
/// | `AdkKernelLedgerRow` | `seq` | `ts_ms` (the instant in UTC milliseconds), `partition`, `agent` and `kind`, the properties `list` filters on, and `row`, the row as JSON text |
/// | `AdkKernelLedgerCounter` | `name` (`ledger`) | `seq`, the last sequence number taken |
/// | `AdkKernelObservation` | `id` | `order_key` (the hex of the id), `partition`, `as_of` (days from the common era) and `body`, the observation as JSON text |
/// | `AdkKernelPassState` | `partition` | `order_key` (the hex of the partition) and `body`, the pass state as JSON text |
///
/// An append is one statement: it raises the counter, which write-locks it until the
/// statement commits, and creates the row under the new number, so rows commit in number
/// order and a reader paging with `after` never passes a row that lands later. Every call
/// runs on a task of its own, as the [process store](crate::Neo4jProcessStore) explains.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::proc::Neo4jLedgerStore::connect(
///     "bolt://localhost:7687",
///     "neo4j",
///     "password",
/// )
/// .await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jLedgerStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jLedgerStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jLedgerStore").finish_non_exhaustive()
    }
}

impl Neo4jLedgerStore {
    /// A store over `graph`, such as the process store's.
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
                    "CREATE CONSTRAINT adk_kernel_ledger_seq IF NOT EXISTS \
                     FOR (r:AdkKernelLedgerRow) REQUIRE r.seq IS UNIQUE",
                    "CREATE INDEX adk_kernel_ledger_ts IF NOT EXISTS \
                     FOR (r:AdkKernelLedgerRow) ON (r.ts_ms)",
                    "CREATE INDEX adk_kernel_ledger_partition IF NOT EXISTS \
                     FOR (r:AdkKernelLedgerRow) ON (r.partition)",
                    "CREATE INDEX adk_kernel_ledger_kind IF NOT EXISTS \
                     FOR (r:AdkKernelLedgerRow) ON (r.kind)",
                    "CREATE CONSTRAINT adk_kernel_ledger_counter IF NOT EXISTS \
                     FOR (c:AdkKernelLedgerCounter) REQUIRE c.name IS UNIQUE",
                    "CREATE CONSTRAINT adk_kernel_observation_id IF NOT EXISTS \
                     FOR (o:AdkKernelObservation) REQUIRE o.id IS UNIQUE",
                    "CREATE INDEX adk_kernel_observation_partition IF NOT EXISTS \
                     FOR (o:AdkKernelObservation) ON (o.partition)",
                    "CREATE CONSTRAINT adk_kernel_pass_state_partition IF NOT EXISTS \
                     FOR (s:AdkKernelPassState) REQUIRE s.partition IS UNIQUE",
                ],
            )
            .await
        })
        .await
    }
}

fn decode_row(row: &Row) -> Result<LedgerEntry> {
    let seq = u64::try_from(column::<i64>(row, "seq")?).map_err(|_| KernelError::Store {
        backend: BACKEND,
        reason: "a ledger row has a negative sequence number".into(),
    })?;
    let mut entry: LedgerEntry = json("ledger row", &column::<String>(row, "row")?)?;
    entry.seq = seq;
    Ok(entry)
}

#[async_trait]
impl LedgerStore for Neo4jLedgerStore {
    async fn append(&self, mut entry: LedgerEntry) -> Result<LedgerEntry> {
        entry.seq = 0;
        let statement = query(APPEND)
            .param("ts_ms", entry.ts.timestamp_millis())
            .param("partition", entry.partition.as_str())
            .param("agent", entry.agent.as_str())
            .param("kind", entry.kind.as_str())
            .param("row", serde_json::to_string(&entry)?);
        let graph = self.graph.clone();
        let seq = detached(async move {
            let rows = rows(&graph, statement, "append to the ledger").await?;
            let row = rows.first().ok_or_else(|| KernelError::Store {
                backend: BACKEND,
                reason: "an append returned no sequence number".into(),
            })?;
            column::<i64>(row, "seq")
        })
        .await?;
        entry.seq = u64::try_from(seq).map_err(|_| KernelError::Store {
            backend: BACKEND,
            reason: format!("the ledger counter holds a negative sequence number {seq}"),
        })?;
        Ok(entry)
    }

    async fn list(&self, filter: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        let mut conditions = Vec::new();
        if filter.partition.is_some() {
            conditions.push("r.partition = $partition");
        }
        if !filter.kinds.is_empty() {
            conditions.push("r.kind IN $kinds");
        }
        if filter.agent.is_some() {
            conditions.push("r.agent = $agent");
        }
        if filter.from.is_some() {
            conditions.push("r.ts_ms >= $from");
        }
        if filter.to.is_some() {
            conditions.push("r.ts_ms < $to");
        }
        if filter.after.is_some() {
            conditions.push("r.seq > $after");
        }
        let (limit, limit_value) = limit_clause(filter.limit);
        let statement = query(&format!(
            "MATCH (r:AdkKernelLedgerRow){} RETURN r.seq AS seq, r.row AS row ORDER BY r.seq{limit}",
            where_clause(&conditions)
        ))
        .param("partition", filter.partition.as_ref().map(|partition| partition.as_str()))
        .param("kinds", filter.kinds.clone())
        .param("agent", filter.agent.clone())
        .param("from", filter.from.map(|from| from.timestamp_millis()))
        .param("to", filter.to.map(|to| to.timestamp_millis()))
        .param("after", filter.after.map(|after| i64::try_from(after).unwrap_or(i64::MAX)))
        .param("limit", limit_value);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the ledger").await?.iter().map(decode_row).collect()
        })
        .await
    }

    async fn put_observation(&self, observation: Observation) -> Result<()> {
        let statement = query(PUT_OBSERVATION)
            .param("id", observation.id.as_str())
            .param("order_key", order_key(&observation.id))
            .param("partition", observation.partition.as_str())
            .param("as_of", day_number(observation.as_of))
            .param("body", serde_json::to_string(&observation)?);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write an observation").await?;
            Ok(())
        })
        .await
    }

    async fn observations(&self, filter: &ObservationQuery) -> Result<Vec<Observation>> {
        let mut conditions = Vec::new();
        if filter.partition.is_some() {
            conditions.push("o.partition = $partition");
        }
        if filter.since.is_some() {
            conditions.push("o.as_of >= $since");
        }
        let (limit, limit_value) = limit_clause(filter.limit);
        let statement = query(&format!(
            "MATCH (o:AdkKernelObservation){} RETURN o.body AS body \
             ORDER BY o.as_of DESC, o.order_key{limit}",
            where_clause(&conditions)
        ))
        .param("partition", filter.partition.as_ref().map(|partition| partition.as_str()))
        .param("since", filter.since.map(day_number))
        .param("limit", limit_value);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the observations")
                .await?
                .iter()
                .map(|row| json("observation", &column::<String>(row, "body")?))
                .collect()
        })
        .await
    }

    async fn pass_state(&self, partition: &PartitionId) -> Result<Option<PassState>> {
        let statement =
            query("MATCH (s:AdkKernelPassState {partition: $partition}) RETURN s.body AS body")
                .param("partition", partition.as_str());
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the pass state")
                .await?
                .first()
                .map(|row| json("pass state", &column::<String>(row, "body")?))
                .transpose()
        })
        .await
    }

    async fn pass_states(&self) -> Result<Vec<PassState>> {
        let statement =
            query("MATCH (s:AdkKernelPassState) RETURN s.body AS body ORDER BY s.order_key");
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the pass states")
                .await?
                .iter()
                .map(|row| json("pass state", &column::<String>(row, "body")?))
                .collect()
        })
        .await
    }

    async fn put_pass_state(&self, state: PassState) -> Result<()> {
        let statement = query(PUT_PASS_STATE)
            .param("partition", state.partition.as_str())
            .param("order_key", order_key(state.partition.as_str()))
            .param("body", serde_json::to_string(&state)?);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write the pass state").await?;
            Ok(())
        })
        .await
    }
}
