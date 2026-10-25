//! The Neo4j [`CronWatermarkStore`] (spec 3.2 §7.4, §13 Q8, feature `neo4j`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use neo4rs::{Graph, query};

use super::neo4j::{column, connect, count, detached, rows, run_schema, store_error};
use crate::error::Result;
use crate::watermark::CronWatermarkStore;

/// Keeps the later cursor in one statement. The `SET` reads the property it updates, so
/// Cypher write-locks the node before reading it, and concurrent advances apply one at a
/// time.
const ADVANCE: &str = "\
MERGE (w:AdkKernelCronWatermark {source: $source}) \
ON CREATE SET w.cursor_us = $cursor_us \
SET w.cursor_us = CASE WHEN w.cursor_us < $cursor_us THEN $cursor_us ELSE w.cursor_us END \
RETURN w.cursor_us AS cursor_us";

/// A [`CronWatermarkStore`] in Neo4j, shared by every replica (spec 3.2 §7.4, §13 Q8,
/// feature `neo4j`).
///
/// | Label | Key (unique) | Properties |
/// |-------|--------------|------------|
/// | `AdkKernelCronWatermark` | `source` | `cursor_us`, the cursor in microseconds since the Unix epoch |
///
/// An advance is one `MERGE` whose `SET` keeps the later of the stored cursor and the one
/// given, under the write lock Cypher takes on a property updated from its own value, so
/// replicas writing in any order never move a cursor back. Every call runs on a task of
/// its own, as the [process store](crate::Neo4jProcessStore) explains. Call
/// [`migrate`](Self::migrate) once per database to create the uniqueness constraint.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::Neo4jCronWatermarkStore::connect("bolt://localhost:7687", "neo4j", "password")
///         .await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jCronWatermarkStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jCronWatermarkStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jCronWatermarkStore").finish_non_exhaustive()
    }
}

impl Neo4jCronWatermarkStore {
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

    /// Creates the uniqueness constraint on the source's name; one that exists is kept.
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
                &["CREATE CONSTRAINT adk_kernel_cron_watermark_source IF NOT EXISTS \
                   FOR (w:AdkKernelCronWatermark) REQUIRE w.source IS UNIQUE"],
            )
            .await
        })
        .await
    }
}

/// The instant `micros` microseconds after the Unix epoch.
fn instant(micros: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros).ok_or_else(|| {
        store_error("read a cron watermark", &format!("the cursor {micros} is out of range"))
    })
}

#[async_trait]
impl CronWatermarkStore for Neo4jCronWatermarkStore {
    async fn read(&self, source: &str) -> Result<Option<DateTime<Utc>>> {
        let statement = query(
            "MATCH (w:AdkKernelCronWatermark {source: $source}) RETURN w.cursor_us AS cursor_us",
        )
        .param("source", source);
        let graph = self.graph.clone();
        detached(async move {
            let rows = rows(&graph, statement, "read a cron watermark").await?;
            // A node a concurrent delete removed after the match reads as null: no cursor.
            match rows.first() {
                Some(row) => column::<Option<i64>>(row, "cursor_us")?.map(instant).transpose(),
                None => Ok(None),
            }
        })
        .await
    }

    async fn advance(&self, source: &str, cursor: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let statement =
            query(ADVANCE).param("source", source).param("cursor_us", cursor.timestamp_micros());
        let graph = self.graph.clone();
        detached(async move {
            let rows = rows(&graph, statement, "advance a cron watermark").await?;
            let stored = match rows.first() {
                Some(row) => column::<Option<i64>>(row, "cursor_us")?,
                None => None,
            };
            stored.map(instant).transpose()?.ok_or_else(|| {
                store_error("advance a cron watermark", &"the statement returned no cursor")
            })
        })
        .await
    }

    async fn delete(&self, source: &str) -> Result<bool> {
        let statement = query(
            "MATCH (w:AdkKernelCronWatermark {source: $source}) DELETE w \
             RETURN count(*) AS deleted",
        )
        .param("source", source);
        let graph = self.graph.clone();
        detached(async move {
            Ok(count(&rows(&graph, statement, "delete a cron watermark").await?, "deleted")? > 0)
        })
        .await
    }
}
