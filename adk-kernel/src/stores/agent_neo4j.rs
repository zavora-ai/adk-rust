//! The Neo4j [`AgentStore`] (spec 3.3 §5.7, §10 K7, feature `neo4j`).

use async_trait::async_trait;
use neo4rs::{Graph, Query, Row, query};

use super::neo4j::{
    column, connect, count, detached, json, order_key, retry, rows, run_schema, txn_rows,
};
use crate::agents::{AgentRecord, AgentStore};
use crate::error::{KernelError, Result};

/// Finds or creates the agent's node at version 0 and takes its write lock inside the
/// transaction: Cypher locks a node before reading a property a `SET` updates from its own
/// value. A transaction that rolls back removes a node it created.
const LOCK: &str = "\
MERGE (a:AdkKernelAgent {id: $id}) ON CREATE SET a.version = 0, a.order_key = $order_key \
SET a.lock_seq = coalesce(a.lock_seq, 0) + 1";

const VERSION: &str = "MATCH (a:AdkKernelAgent {id: $id}) RETURN a.version AS version";

const WRITE: &str = "\
MATCH (a:AdkKernelAgent {id: $id}) \
SET a.version = $version, a.state = $state, a.record = $record";

/// An [`AgentStore`] in Neo4j, shared by every replica (spec 3.3 §5.7, §10 K7, feature
/// `neo4j`).
///
/// One `AdkKernelAgent` node per agent, unique by `id`: `version` and `state`, the
/// properties a write compares and a delete reads, `order_key` (the hex of the id, so
/// `list` orders by the id's UTF-8 bytes whatever the database's collation), and `record`,
/// the record as JSON text. `put_if_version` write-locks the node inside a transaction
/// before it reads the version, so of two replicas changing one agent from the same version
/// the second sees the first's write and changes nothing. Every call runs on a task of its
/// own, as the [process store](crate::Neo4jProcessStore) explains.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::stores::Neo4jAgentStore::connect("bolt://localhost:7687", "neo4j", "password").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jAgentStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jAgentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jAgentStore").finish_non_exhaustive()
    }
}

impl Neo4jAgentStore {
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

    /// Creates the constraint the store relies on; one that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        let graph = self.graph.clone();
        detached(async move {
            run_schema(
                &graph,
                &["CREATE CONSTRAINT adk_kernel_agent_id IF NOT EXISTS \
                   FOR (a:AdkKernelAgent) REQUIRE a.id IS UNIQUE"],
            )
            .await
        })
        .await
    }
}

fn stored_version(version: u64) -> Result<i64> {
    i64::try_from(version).map_err(|_| KernelError::Store {
        backend: super::neo4j::BACKEND,
        reason: format!("agent version {version} exceeds a signed 64-bit integer"),
    })
}

/// Reads a row back into its record, or `None` for a node without one: a concurrent
/// transaction's node not written yet, or one a `delete` removed after the statement
/// matched it.
fn decode(row: &Row) -> Result<Option<AgentRecord>> {
    column::<Option<String>>(row, "record")?.map(|record| json("agent", &record)).transpose()
}

/// Writes `write` over the agent `id` when its stored version is `expected`, in one
/// transaction that write-locks the node first.
async fn put_once(
    graph: &Graph,
    id: &str,
    expected: i64,
    write: Query,
) -> std::result::Result<bool, neo4rs::Error> {
    let mut txn = graph.start_txn().await?;
    txn.run(query(LOCK).param("id", id).param("order_key", order_key(id))).await?;
    let version = txn_rows(&mut txn, query(VERSION).param("id", id))
        .await?
        .first()
        .and_then(|row| row.get::<Option<i64>>("version").ok().flatten())
        .unwrap_or(0);
    if version != expected {
        txn.rollback().await?;
        return Ok(false);
    }
    txn.run(write).await?;
    txn.commit().await?;
    Ok(true)
}

#[async_trait]
impl AgentStore for Neo4jAgentStore {
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> Result<bool> {
        let write = query(WRITE)
            .param("id", record.id.as_str())
            .param("version", stored_version(record.version)?)
            .param("state", record.state.as_str())
            .param("record", serde_json::to_string(&record)?);
        let expected = stored_version(expected)?;
        let (graph, id) = (self.graph.clone(), record.id.clone());
        detached(async move {
            retry("write the agent table", || put_once(&graph, &id, expected, write.clone())).await
        })
        .await
    }

    async fn get(&self, id: &str) -> Result<Option<AgentRecord>> {
        let statement =
            query("MATCH (a:AdkKernelAgent {id: $id}) RETURN a.record AS record").param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            Ok(match rows(&graph, statement, "read the agent table").await?.first() {
                Some(row) => decode(row)?,
                None => None,
            })
        })
        .await
    }

    async fn list(&self) -> Result<Vec<AgentRecord>> {
        let statement = query(
            "MATCH (a:AdkKernelAgent) WHERE a.record IS NOT NULL RETURN a.record AS record \
             ORDER BY a.order_key",
        );
        let graph = self.graph.clone();
        detached(async move {
            let rows = rows(&graph, statement, "list the agent table").await?;
            rows.iter().filter_map(|row| decode(row).transpose()).collect()
        })
        .await
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let statement = query(
            "MATCH (a:AdkKernelAgent {id: $id}) WHERE a.state = 'proposed' DELETE a \
             RETURN count(*) AS deleted",
        )
        .param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(&rows(&graph, statement, "delete from the agent table").await?, "deleted")?
                > 0)
        })
        .await
    }
}
