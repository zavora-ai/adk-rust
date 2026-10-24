//! The Neo4j [`AgendaStore`] (spec 3.2 §7.1, §7.5, §13 Q8, feature `neo4j`).

use async_trait::async_trait;
use neo4rs::{Graph, Query, Row, query};

use super::neo4j::{
    column, connect, count, detached, json, limit_clause, order_key, retry, rows, run_schema,
    txn_rows, where_clause,
};
use crate::agenda::{AgendaFilter, AgendaItem, AgendaStateKind, AgendaStore};
use crate::error::Result;

const PUT: &str = "\
MERGE (a:AdkKernelAgendaItem {id: $id}) \
SET a = {id: $id, order_key: $order_key, partition: $partition, state: $state, \
    created_us: $created_us, item: $item}";

/// Takes the item's write lock inside a transaction: Cypher locks a node before reading a
/// property a `SET` updates from its own value.
const LOCK: &str = "\
MATCH (a:AdkKernelAgendaItem {id: $id}) SET a.lock_seq = coalesce(a.lock_seq, 0) + 1 \
RETURN count(a) AS locked";

const STATE: &str = "MATCH (a:AdkKernelAgendaItem {id: $id}) RETURN a.state AS state";

const WRITE: &str = "\
MATCH (a:AdkKernelAgendaItem {id: $id}) \
SET a.partition = $partition, a.state = $state, a.created_us = $created_us, a.item = $item";

/// An [`AgendaStore`] in Neo4j, shared by every replica (spec 3.2 §7.1, §7.5, §13 Q8,
/// feature `neo4j`). **Alpha** until its live conformance suite passes in CI.
///
/// One `AdkKernelAgendaItem` node per item, unique by `id`: `partition`, `state` (its kind)
/// and `created_us` (microseconds since the Unix epoch), the properties `list` filters and
/// sorts on, `order_key` (the hex of the id, so ties order by the id's UTF-8 bytes whatever
/// the database's collation), and `item`, the item as JSON text. `transition` write-locks
/// the node inside a transaction before it reads the state, so of two replicas moving one
/// item from the same state the second sees the first's write and moves nothing. Every call
/// runs on a task of its own, as the [process store](crate::Neo4jProcessStore) explains.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::Neo4jAgendaStore::connect("bolt://localhost:7687", "neo4j", "password").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jAgendaStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jAgendaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jAgendaStore").finish_non_exhaustive()
    }
}

impl Neo4jAgendaStore {
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

    /// Creates the constraint and indexes the store relies on; one that exists is kept.
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
                    "CREATE CONSTRAINT adk_kernel_agenda_id IF NOT EXISTS \
                     FOR (a:AdkKernelAgendaItem) REQUIRE a.id IS UNIQUE",
                    "CREATE INDEX adk_kernel_agenda_partition IF NOT EXISTS \
                     FOR (a:AdkKernelAgendaItem) ON (a.partition)",
                    "CREATE INDEX adk_kernel_agenda_state IF NOT EXISTS \
                     FOR (a:AdkKernelAgendaItem) ON (a.state)",
                    "CREATE INDEX adk_kernel_agenda_created IF NOT EXISTS \
                     FOR (a:AdkKernelAgendaItem) ON (a.created_us)",
                ],
            )
            .await
        })
        .await
    }
}

/// `statement` with the item's stored properties bound.
fn bind(statement: Query, item: &AgendaItem) -> Result<Query> {
    Ok(statement
        .param("id", item.id.as_str())
        .param("partition", item.partition.as_str())
        .param("state", item.state.kind().as_str())
        .param("created_us", item.created_at.timestamp_micros())
        .param("item", serde_json::to_string(item)?))
}

fn decode(row: &Row) -> Result<AgendaItem> {
    json("agenda item", &column::<String>(row, "item")?)
}

/// Writes `write` over the item `id` when its state is `from`, in one transaction that
/// write-locks the node first.
async fn transition_once(
    graph: &Graph,
    id: &str,
    from: &str,
    write: Query,
) -> std::result::Result<bool, neo4rs::Error> {
    let mut txn = graph.start_txn().await?;
    txn.run(query(LOCK).param("id", id)).await?;
    let state = txn_rows(&mut txn, query(STATE).param("id", id))
        .await?
        .first()
        .and_then(|row| row.get::<Option<String>>("state").ok().flatten());
    if state.as_deref() != Some(from) {
        txn.rollback().await?;
        return Ok(false);
    }
    txn.run(write).await?;
    txn.commit().await?;
    Ok(true)
}

#[async_trait]
impl AgendaStore for Neo4jAgendaStore {
    async fn put(&self, item: AgendaItem) -> Result<()> {
        let statement = bind(query(PUT), &item)?.param("order_key", order_key(&item.id));
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write the agenda").await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &str) -> Result<Option<AgendaItem>> {
        let statement =
            query("MATCH (a:AdkKernelAgendaItem {id: $id}) RETURN a.item AS item").param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the agenda").await?.first().map(decode).transpose()
        })
        .await
    }

    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>> {
        let mut conditions = Vec::new();
        if filter.partition.is_some() {
            conditions.push("a.partition = $partition");
        }
        if !filter.states.is_empty() {
            conditions.push("a.state IN $states");
        }
        let states: Vec<String> =
            filter.states.iter().map(|state| state.as_str().to_string()).collect();
        let (limit, limit_value) = limit_clause(filter.limit);
        let statement = query(&format!(
            "MATCH (a:AdkKernelAgendaItem){} RETURN a.item AS item \
             ORDER BY a.created_us, a.order_key{limit}",
            where_clause(&conditions)
        ))
        .param("partition", filter.partition.as_ref().map(|partition| partition.as_str()))
        .param("states", states)
        .param("limit", limit_value);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "list the agenda").await?.iter().map(decode).collect()
        })
        .await
    }

    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool> {
        let write = bind(query(WRITE), &item)?;
        let (graph, id) = (self.graph.clone(), item.id.clone());
        detached(async move {
            retry("update the agenda", || {
                transition_once(&graph, &id, from.as_str(), write.clone())
            })
            .await
        })
        .await
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let statement =
            query("MATCH (a:AdkKernelAgendaItem {id: $id}) DELETE a RETURN count(*) AS deleted")
                .param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(&rows(&graph, statement, "delete from the agenda").await?, "deleted")? > 0)
        })
        .await
    }
}
