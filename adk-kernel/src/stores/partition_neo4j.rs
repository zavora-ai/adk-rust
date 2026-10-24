//! The Neo4j [`PartitionStore`] (spec 3.2 §7.8, §13 Q8, feature `neo4j`).

use async_trait::async_trait;
use neo4rs::{Graph, Row, query};

use super::neo4j::{
    column, connect, count, detached, json, limit_clause, order_key, rows, run_schema, where_clause,
};
use crate::error::Result;
use crate::partition::{CrossingFilter, CrossingRecord, Partition, PartitionId, PartitionStore};

const RECORD_CROSSING: &str = "\
CREATE (c:AdkKernelCrossing {id: $id, order_key: $order_key, at_us: $at_us, run: $run, \
    source: $source, target: $target, fingerprint: $fingerprint, outcome: $outcome, record: $record})";

const GRANT: &str = "\
MERGE (g:AdkKernelCrossingGrant {key: $key}) \
ON CREATE SET g.run = $run, g.fingerprint = $fingerprint, g.order_key = $order_key";

/// A [`PartitionStore`] in Neo4j, shared by every replica (spec 3.2 §7.1, §7.8, §13 Q8,
/// feature `neo4j`). **Alpha** until its live conformance suite passes in CI.
///
/// | Label | Key (unique) | Properties |
/// |-------|--------------|------------|
/// | `AdkKernelPartition` | `id` | `order_key` (the hex of the id) and `record`, the partition as JSON text |
/// | `AdkKernelCrossing` | `id` | `order_key` (the hex of the id), `at_us` (microseconds since the Unix epoch), `run`, `source`, `target`, `fingerprint` and `outcome`, the properties `crossings` filters on, and `record`, the crossing as JSON text |
/// | `AdkKernelCrossingGrant` | `key`: the hex of the run and of the fingerprint, joined by `.` | `run`, `fingerprint` and `order_key` (the hex of the fingerprint) |
///
/// Ordering by `order_key` follows the ids' UTF-8 bytes whatever the database's collation,
/// so partitions order by id, crossings by time, then id, and a run's grants by
/// fingerprint, as on every backend. A crossing is created once and never changed; a grant
/// is a `MERGE` on its key, so granting twice keeps one grant. Every call runs on a task of
/// its own, as the [process store](crate::Neo4jProcessStore) explains.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::Neo4jPartitionStore::connect("bolt://localhost:7687", "neo4j", "password")
///         .await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jPartitionStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jPartitionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jPartitionStore").finish_non_exhaustive()
    }
}

impl Neo4jPartitionStore {
    /// A store over `graph`, such as the process store's.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Connects to the server at `uri`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the server cannot be
    /// reached.
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
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when a schema statement
    /// fails.
    pub async fn migrate(&self) -> Result<()> {
        let graph = self.graph.clone();
        detached(async move {
            run_schema(
                &graph,
                &[
                    "CREATE CONSTRAINT adk_kernel_partition_id IF NOT EXISTS \
                     FOR (p:AdkKernelPartition) REQUIRE p.id IS UNIQUE",
                    "CREATE INDEX adk_kernel_partition_order IF NOT EXISTS \
                     FOR (p:AdkKernelPartition) ON (p.order_key)",
                    "CREATE CONSTRAINT adk_kernel_crossing_id IF NOT EXISTS \
                     FOR (c:AdkKernelCrossing) REQUIRE c.id IS UNIQUE",
                    "CREATE INDEX adk_kernel_crossing_at IF NOT EXISTS \
                     FOR (c:AdkKernelCrossing) ON (c.at_us)",
                    "CREATE INDEX adk_kernel_crossing_run IF NOT EXISTS \
                     FOR (c:AdkKernelCrossing) ON (c.run)",
                    "CREATE INDEX adk_kernel_crossing_source IF NOT EXISTS \
                     FOR (c:AdkKernelCrossing) ON (c.source)",
                    "CREATE INDEX adk_kernel_crossing_target IF NOT EXISTS \
                     FOR (c:AdkKernelCrossing) ON (c.target)",
                    "CREATE CONSTRAINT adk_kernel_crossing_grant_key IF NOT EXISTS \
                     FOR (g:AdkKernelCrossingGrant) REQUIRE g.key IS UNIQUE",
                    "CREATE INDEX adk_kernel_crossing_grant_run IF NOT EXISTS \
                     FOR (g:AdkKernelCrossingGrant) ON (g.run)",
                ],
            )
            .await
        })
        .await
    }
}

/// Column `name` of `row` as `what`, or `None` for a node a concurrent delete removed after
/// the statement matched it, whose properties then read as null.
fn decode<T: serde::de::DeserializeOwned>(row: &Row, name: &str, what: &str) -> Result<Option<T>> {
    column::<Option<String>>(row, name)?.map(|text| json(what, &text)).transpose()
}

#[async_trait]
impl PartitionStore for Neo4jPartitionStore {
    async fn put(&self, partition: Partition) -> Result<()> {
        let statement = query(
            "MERGE (p:AdkKernelPartition {id: $id}) SET p.order_key = $order_key, \
             p.record = $record",
        )
        .param("id", partition.id.as_str())
        .param("order_key", order_key(partition.id.as_str()))
        .param("record", serde_json::to_string(&partition)?);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write the partition table").await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &PartitionId) -> Result<Option<Partition>> {
        let statement = query("MATCH (p:AdkKernelPartition {id: $id}) RETURN p.record AS record")
            .param("id", id.as_str());
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the partition table")
                .await?
                .first()
                .map_or(Ok(None), |row| decode(row, "record", "partition"))
        })
        .await
    }

    async fn list(&self) -> Result<Vec<Partition>> {
        let statement =
            query("MATCH (p:AdkKernelPartition) RETURN p.record AS record ORDER BY p.order_key");
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "list the partition table")
                .await?
                .iter()
                .filter_map(|row| decode(row, "record", "partition").transpose())
                .collect()
        })
        .await
    }

    async fn delete(&self, id: &PartitionId) -> Result<bool> {
        let statement =
            query("MATCH (p:AdkKernelPartition {id: $id}) DELETE p RETURN count(*) AS deleted")
                .param("id", id.as_str());
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(
                &rows(&graph, statement, "delete from the partition table").await?,
                "deleted",
            )? > 0)
        })
        .await
    }

    async fn record_crossing(&self, record: CrossingRecord) -> Result<()> {
        let statement = query(RECORD_CROSSING)
            .param("id", record.id.as_str())
            .param("order_key", order_key(&record.id))
            .param("at_us", record.at.timestamp_micros())
            .param("run", record.run.as_str())
            .param("source", record.from.as_str())
            .param("target", record.to.as_str())
            .param("fingerprint", record.fingerprint.as_str())
            .param("outcome", record.outcome.as_str())
            .param("record", serde_json::to_string(&record)?);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "record a crossing").await?;
            Ok(())
        })
        .await
    }

    async fn crossings(&self, filter: &CrossingFilter) -> Result<Vec<CrossingRecord>> {
        let mut conditions = Vec::new();
        if filter.from.is_some() {
            conditions.push("c.source = $source");
        }
        if filter.to.is_some() {
            conditions.push("c.target = $target");
        }
        if filter.run.is_some() {
            conditions.push("c.run = $run");
        }
        if filter.fingerprint.is_some() {
            conditions.push("c.fingerprint = $fingerprint");
        }
        if !filter.outcomes.is_empty() {
            conditions.push("c.outcome IN $outcomes");
        }
        if filter.since.is_some() {
            conditions.push("c.at_us >= $since");
        }
        let outcomes: Vec<String> =
            filter.outcomes.iter().map(|outcome| outcome.as_str().to_string()).collect();
        let (limit, limit_value) = limit_clause(filter.limit);
        let statement = query(&format!(
            "MATCH (c:AdkKernelCrossing){} RETURN c.record AS record \
             ORDER BY c.at_us, c.order_key{limit}",
            where_clause(&conditions)
        ))
        .param("source", filter.from.as_ref().map(|from| from.as_str()))
        .param("target", filter.to.as_ref().map(|to| to.as_str()))
        .param("run", filter.run.clone())
        .param("fingerprint", filter.fingerprint.clone())
        .param("outcomes", outcomes)
        .param("since", filter.since.map(|since| since.timestamp_micros()))
        .param("limit", limit_value);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the crossings")
                .await?
                .iter()
                .filter_map(|row| decode(row, "record", "crossing record").transpose())
                .collect()
        })
        .await
    }

    async fn grant(&self, run: &str, fingerprint: &str) -> Result<()> {
        let statement = query(GRANT)
            .param("key", format!("{}.{}", order_key(run), order_key(fingerprint)))
            .param("run", run)
            .param("fingerprint", fingerprint)
            .param("order_key", order_key(fingerprint));
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "record a crossing grant").await?;
            Ok(())
        })
        .await
    }

    async fn grants(&self, run: &str) -> Result<Vec<String>> {
        let statement = query(
            "MATCH (g:AdkKernelCrossingGrant {run: $run}) RETURN g.fingerprint AS fingerprint \
             ORDER BY g.order_key",
        )
        .param("run", run);
        let graph = self.graph.clone();
        detached(async move {
            let rows = rows(&graph, statement, "read the crossing grants").await?;
            let mut grants = Vec::with_capacity(rows.len());
            for row in &rows {
                if let Some(fingerprint) = column::<Option<String>>(row, "fingerprint")? {
                    grants.push(fingerprint);
                }
            }
            Ok(grants)
        })
        .await
    }

    async fn release_run(&self, run: &str) -> Result<u64> {
        let statement = query(
            "MATCH (g:AdkKernelCrossingGrant {run: $run}) DELETE g RETURN count(*) AS released",
        )
        .param("run", run);
        let graph = self.graph.clone();
        let released = detached(async move {
            count(&rows(&graph, statement, "release the crossing grants").await?, "released")
        })
        .await?;
        Ok(u64::try_from(released).unwrap_or(0))
    }
}
