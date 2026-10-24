//! The Firestore [`PartitionStore`] (spec 3.2 §7.8, §13 Q8, feature `firestore`).

use async_trait::async_trait;
use firestore::{FirestoreDb, FirestoreQueryDirection};
use serde::{Deserialize, Serialize};

use super::firestore::{connect, delete_where, digest_id, doc_id, json, store_error};
use crate::error::Result;
use crate::partition::{CrossingFilter, CrossingRecord, Partition, PartitionId, PartitionStore};

const PARTITIONS: &str = "adk_kernel_partitions";
const CROSSINGS: &str = "adk_kernel_crossings";
const GRANTS: &str = "adk_kernel_crossing_grants";

/// One partition, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PartitionDoc {
    id: String,
    record: String,
}

/// One crossing, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CrossingDoc {
    id: String,
    at_us: i64,
    run: String,
    from: String,
    to: String,
    fingerprint: String,
    outcome: String,
    record: String,
}

/// One live grant, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrantDoc {
    run: String,
    fingerprint: String,
}

/// A [`PartitionStore`] in Firestore, shared by every replica (spec 3.2 §7.1, §7.8, §13 Q8,
/// feature `firestore`). **Alpha** until its live conformance suite passes in CI.
///
/// | Collection (document ID) | Contents |
/// |--------------------------|----------|
/// | `adk_kernel_partitions` (hex of the id) | `id` and `record`, the partition as JSON text |
/// | `adk_kernel_crossings` (hex of the id) | `id`, `at_us` (microseconds since the Unix epoch), `run`, `from`, `to`, `fingerprint`, `outcome`, and `record`, the crossing as JSON text |
/// | `adk_kernel_crossing_grants` (SHA-256 of the run and the fingerprint) | `run` and `fingerprint`: one live grant |
///
/// `crossings` queries with the equality filters the filter names — source, target, run,
/// fingerprint — or, with none of those, with its `since` range on `at_us`, which
/// Firestore serves from its automatic single-field indexes; it applies the rest, orders by
/// time, then id, and applies the limit in memory, so no composite index is needed. `list`
/// and `grants` order in memory too. A crossing is written with Firestore's create, which
/// fails when the document exists, so a second record with the same id never replaces it. A grant is written
/// under its digest, so granting twice keeps one grant; `release_run` deletes each of the
/// run's grants in a transaction of its own and answers how many it deleted. An id longer
/// than 700 bytes is refused.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::FirestorePartitionStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestorePartitionStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestorePartitionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestorePartitionStore").finish_non_exhaustive()
    }
}

impl FirestorePartitionStore {
    /// A store over `db`, such as the process store's.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Connects to `project_id` with Application Default Credentials.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the client cannot be
    /// created.
    pub async fn connect(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await?))
    }

    /// The client the store uses.
    pub fn db(&self) -> &FirestoreDb {
        &self.db
    }

    /// Prepares the store's storage; Firestore needs nothing created, so this does nothing.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    /// The grants of `run`.
    async fn grant_docs(&self, run: &str) -> Result<Vec<GrantDoc>> {
        self.db
            .fluent()
            .select()
            .from(GRANTS)
            .filter(|q| q.for_all([q.field("run").eq(run)]))
            .obj()
            .query()
            .await
            .map_err(|error| store_error("read the crossing grants", &error))
    }
}

/// The document ID of the grant of `fingerprint` in `run`.
fn grant_id(run: &str, fingerprint: &str) -> String {
    digest_id(&[run, fingerprint])
}

#[async_trait]
impl PartitionStore for FirestorePartitionStore {
    async fn put(&self, partition: Partition) -> Result<()> {
        let doc = PartitionDoc {
            id: partition.id.as_str().to_string(),
            record: serde_json::to_string(&partition)?,
        };
        self.db
            .fluent()
            .update()
            .in_col(PARTITIONS)
            .document_id(doc_id("partition", &doc.id)?)
            .object(&doc)
            .execute::<PartitionDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write the partition table", &error))
    }

    async fn get(&self, id: &PartitionId) -> Result<Option<Partition>> {
        let Ok(doc_name) = doc_id("partition", id.as_str()) else {
            return Ok(None);
        };
        let doc: Option<PartitionDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(PARTITIONS)
            .obj()
            .one(doc_name)
            .await
            .map_err(|error| store_error("read the partition table", &error))?;
        doc.filter(|doc| doc.id == id.as_str())
            .map(|doc| json("partition", &doc.record))
            .transpose()
    }

    async fn list(&self) -> Result<Vec<Partition>> {
        let mut docs: Vec<PartitionDoc> = self
            .db
            .fluent()
            .select()
            .from(PARTITIONS)
            .obj()
            .query()
            .await
            .map_err(|error| store_error("list the partition table", &error))?;
        docs.sort_by(|a, b| a.id.cmp(&b.id));
        docs.iter().map(|doc| json("partition", &doc.record)).collect()
    }

    async fn delete(&self, id: &PartitionId) -> Result<bool> {
        let Ok(doc_name) = doc_id("partition", id.as_str()) else {
            return Ok(false);
        };
        let id = id.as_str().to_string();
        let matches = move |doc: &PartitionDoc| doc.id == id;
        delete_where(&self.db, PARTITIONS, doc_name, matches, "delete from the partition table")
            .await
    }

    async fn record_crossing(&self, record: CrossingRecord) -> Result<()> {
        let doc = CrossingDoc {
            id: record.id.clone(),
            at_us: record.at.timestamp_micros(),
            run: record.run.clone(),
            from: record.from.as_str().to_string(),
            to: record.to.as_str().to_string(),
            fingerprint: record.fingerprint.clone(),
            outcome: record.outcome.as_str().to_string(),
            record: serde_json::to_string(&record)?,
        };
        self.db
            .fluent()
            .insert()
            .into(CROSSINGS)
            .document_id(doc_id("crossing", &record.id)?)
            .object(&doc)
            .execute::<CrossingDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("record a crossing", &error))
    }

    async fn crossings(&self, filter: &CrossingFilter) -> Result<Vec<CrossingRecord>> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let by_equality = filter.from.is_some()
            || filter.to.is_some()
            || filter.run.is_some()
            || filter.fingerprint.is_some();
        let since = filter.since.map(|since| since.timestamp_micros());
        let mut select = self.db.fluent().select().from(CROSSINGS).filter(|q| {
            q.for_all([
                filter.from.as_ref().and_then(|from| q.field("from").eq(from.as_str())),
                filter.to.as_ref().and_then(|to| q.field("to").eq(to.as_str())),
                filter.run.as_ref().and_then(|run| q.field("run").eq(run.as_str())),
                filter
                    .fingerprint
                    .as_ref()
                    .and_then(|fingerprint| q.field("fingerprint").eq(fingerprint.as_str())),
                since
                    .filter(|_| !by_equality)
                    .and_then(|since| q.field("at_us").greater_than_or_equal(since)),
            ])
        });
        if !by_equality && since.is_some() {
            select = select.order_by([("at_us", FirestoreQueryDirection::Ascending)]);
        }
        let mut docs: Vec<CrossingDoc> = select
            .obj()
            .query()
            .await
            .map_err(|error| store_error("read the crossings", &error))?;
        docs.sort_by(|a, b| (a.at_us, &a.id).cmp(&(b.at_us, &b.id)));
        let mut out = Vec::new();
        for doc in &docs {
            let record: CrossingRecord = json("crossing record", &doc.record)?;
            if filter.matches(&record) {
                out.push(record);
                if filter.limit.is_some_and(|limit| out.len() >= limit) {
                    break;
                }
            }
        }
        Ok(out)
    }

    async fn grant(&self, run: &str, fingerprint: &str) -> Result<()> {
        let doc = GrantDoc { run: run.to_string(), fingerprint: fingerprint.to_string() };
        self.db
            .fluent()
            .update()
            .in_col(GRANTS)
            .document_id(grant_id(run, fingerprint))
            .object(&doc)
            .execute::<GrantDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("record a crossing grant", &error))
    }

    async fn grants(&self, run: &str) -> Result<Vec<String>> {
        let mut grants: Vec<String> =
            self.grant_docs(run).await?.into_iter().map(|doc| doc.fingerprint).collect();
        grants.sort();
        Ok(grants)
    }

    async fn release_run(&self, run: &str) -> Result<u64> {
        let mut released = 0;
        for doc in self.grant_docs(run).await? {
            let (run, fingerprint) = (doc.run.clone(), doc.fingerprint.clone());
            let matches =
                move |stored: &GrantDoc| stored.run == run && stored.fingerprint == fingerprint;
            let doc_name = grant_id(&doc.run, &doc.fingerprint);
            if delete_where(&self.db, GRANTS, doc_name, matches, "release the crossing grants")
                .await?
            {
                released += 1;
            }
        }
        Ok(released)
    }
}
