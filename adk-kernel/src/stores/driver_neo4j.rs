//! The Neo4j [`DriverStore`] (spec 3.2 §7.7, §13 Q8; features `drivers`, `neo4j`).

use adk_core::ApprovalScope;
use async_trait::async_trait;
use neo4rs::{Graph, Row, query};

use super::neo4j::{column, connect, count, detached, json, order_key, rows, run_schema};
use crate::drivers::store::park_key;
use crate::drivers::{DriverPark, DriverRecord, DriverStore};
use crate::error::Result;

/// A [`DriverStore`] in Neo4j, shared by every replica (spec 3.2 §7.7, §13 Q8; features
/// `drivers`, `neo4j`).
///
/// | Label | Key (unique) | Properties |
/// |-------|--------------|------------|
/// | `AdkKernelDriver` | `id` | `order_key` (the hex of the id) and `record`, the record as JSON text |
/// | `AdkKernelDriverPark` | `key`: the hex of the scope's app, user and session and of the call id, joined by `.` | `driver` and `park`, the park as JSON text |
///
/// `.` sorts below every hex digit, so ordering by `key` orders parks by scope, then call
/// id, and drivers order by `order_key`, each by UTF-8 bytes whatever the database's
/// collation. Every call runs on a task of its own, as the
/// [process store](crate::Neo4jProcessStore) explains.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::Neo4jDriverStore::connect(
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
pub struct Neo4jDriverStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jDriverStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jDriverStore").finish_non_exhaustive()
    }
}

impl Neo4jDriverStore {
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
                    "CREATE CONSTRAINT adk_kernel_driver_id IF NOT EXISTS \
                     FOR (d:AdkKernelDriver) REQUIRE d.id IS UNIQUE",
                    "CREATE INDEX adk_kernel_driver_order IF NOT EXISTS \
                     FOR (d:AdkKernelDriver) ON (d.order_key)",
                    "CREATE CONSTRAINT adk_kernel_driver_park_key IF NOT EXISTS \
                     FOR (k:AdkKernelDriverPark) REQUIRE k.key IS UNIQUE",
                    "CREATE INDEX adk_kernel_driver_park_driver IF NOT EXISTS \
                     FOR (k:AdkKernelDriverPark) ON (k.driver)",
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

/// The key of the park of `call_id` in `scope`.
fn park_node_key(scope: &ApprovalScope, call_id: &str) -> String {
    let (app, user, session, call) = park_key(scope, call_id);
    [app, user, session, call].iter().map(|part| order_key(part)).collect::<Vec<_>>().join(".")
}

#[async_trait]
impl DriverStore for Neo4jDriverStore {
    async fn put(&self, record: DriverRecord) -> Result<()> {
        let id = record.id().to_string();
        let statement = query(
            "MERGE (d:AdkKernelDriver {id: $id}) SET d.order_key = $order_key, d.record = $record",
        )
        .param("order_key", order_key(&id))
        .param("record", serde_json::to_string(&record)?)
        .param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write the driver table").await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &str) -> Result<Option<DriverRecord>> {
        let statement =
            query("MATCH (d:AdkKernelDriver {id: $id}) RETURN d.record AS record").param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the driver table")
                .await?
                .first()
                .map_or(Ok(None), |row| decode(row, "record", "driver"))
        })
        .await
    }

    async fn list(&self) -> Result<Vec<DriverRecord>> {
        let statement =
            query("MATCH (d:AdkKernelDriver) RETURN d.record AS record ORDER BY d.order_key");
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "list the driver table")
                .await?
                .iter()
                .filter_map(|row| decode(row, "record", "driver").transpose())
                .collect()
        })
        .await
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let statement =
            query("MATCH (d:AdkKernelDriver {id: $id}) DELETE d RETURN count(*) AS deleted")
                .param("id", id);
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(&rows(&graph, statement, "delete from the driver table").await?, "deleted")?
                > 0)
        })
        .await
    }

    async fn put_park(&self, park: DriverPark) -> Result<()> {
        let statement = query(
            "MERGE (k:AdkKernelDriverPark {key: $key}) SET k.driver = $driver, k.park = $park",
        )
        .param("key", park_node_key(&park.scope, &park.call_id))
        .param("driver", park.driver.as_str())
        .param("park", serde_json::to_string(&park)?);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "write the driver table").await?;
            Ok(())
        })
        .await
    }

    async fn get_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<Option<DriverPark>> {
        let statement = query("MATCH (k:AdkKernelDriverPark {key: $key}) RETURN k.park AS park")
            .param("key", park_node_key(scope, call_id));
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "read the driver table")
                .await?
                .first()
                .map_or(Ok(None), |row| decode(row, "park", "parked driver call"))
        })
        .await
    }

    async fn list_parks(&self, driver: Option<&str>) -> Result<Vec<DriverPark>> {
        let filter = if driver.is_some() { " WHERE k.driver = $driver" } else { "" };
        let statement = query(&format!(
            "MATCH (k:AdkKernelDriverPark){filter} RETURN k.park AS park ORDER BY k.key"
        ))
        .param("driver", driver);
        let graph = self.graph.clone();
        detached(async move {
            rows(&graph, statement, "list the driver table")
                .await?
                .iter()
                .filter_map(|row| decode(row, "park", "parked driver call").transpose())
                .collect()
        })
        .await
    }

    async fn delete_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<bool> {
        let statement =
            query("MATCH (k:AdkKernelDriverPark {key: $key}) DELETE k RETURN count(*) AS deleted")
                .param("key", park_node_key(scope, call_id));
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(&rows(&graph, statement, "delete from the driver table").await?, "deleted")?
                > 0)
        })
        .await
    }
}
