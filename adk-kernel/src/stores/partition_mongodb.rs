//! The MongoDB [`PartitionStore`] (spec 3.2 §7.8, §13 Q8, feature `mongodb`).

use async_trait::async_trait;
use mongodb::bson::{Document, doc};
use mongodb::{Collection, Database};

use super::mongodb::{
    collect, connect, create_indexes, json, retry_upsert, sorted, store_error, text,
};
use crate::error::Result;
use crate::partition::{CrossingFilter, CrossingRecord, Partition, PartitionId, PartitionStore};

const PARTITIONS: &str = "adk_kernel_partitions";
const CROSSINGS: &str = "adk_kernel_crossings";
const GRANTS: &str = "adk_kernel_crossing_grants";

/// A [`PartitionStore`] in MongoDB, shared by every replica (spec 3.2 §7.1, §7.8, §13 Q8,
/// feature `mongodb`). **Alpha** until its live conformance suite passes in CI.
///
/// | Collection | `_id` | Contents |
/// |------------|-------|----------|
/// | `adk_kernel_partitions` | The partition id | `record`, the partition as JSON text |
/// | `adk_kernel_crossings` | The crossing id | `at_us` (microseconds since the Unix epoch), `run`, `from`, `to`, `fingerprint` and `outcome`, the fields `crossings` filters on, and `record`, the crossing as JSON text |
/// | `adk_kernel_crossing_grants` | `{run, fingerprint}` | `run` and `fingerprint`: one live grant |
///
/// Sorts use MongoDB's binary string comparison, so partitions order by their ids' UTF-8
/// bytes and crossings by time, then id, as on every backend. A crossing is inserted once
/// and never changed. A grant is an upsert, so granting twice keeps one grant;
/// `release_run` deletes a run's grants in one `deleteMany` and answers how many it
/// deleted. Every write is a single-document write or one `deleteMany`, so a standalone
/// `mongod` serves the store as a replica set does.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::MongoPartitionStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoPartitionStore {
    db: Database,
}

impl MongoPartitionStore {
    /// A store over `db`, such as the process store's.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Connects to `connection_string` and uses the database `database_name`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the connection string
    /// is invalid.
    pub async fn connect(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(connect(connection_string, database_name).await?))
    }

    /// The database the store writes to.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Creates the indexes `crossings` and `grants` read; an index that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the server cannot be
    /// reached or refuses an index.
    pub async fn migrate(&self) -> Result<()> {
        create_indexes(
            &self.crossing_rows(),
            &[
                ("adk_kernel_crossings_at", doc! { "at_us": 1, "_id": 1 }),
                ("adk_kernel_crossings_run", doc! { "run": 1, "at_us": 1, "_id": 1 }),
                ("adk_kernel_crossings_from", doc! { "from": 1, "at_us": 1, "_id": 1 }),
                ("adk_kernel_crossings_to", doc! { "to": 1, "at_us": 1, "_id": 1 }),
            ],
        )
        .await?;
        create_indexes(
            &self.grant_rows(),
            &[("adk_kernel_crossing_grants_run", doc! { "run": 1, "fingerprint": 1 })],
        )
        .await
    }

    fn partition_rows(&self) -> Collection<Document> {
        self.db.collection(PARTITIONS)
    }

    fn crossing_rows(&self) -> Collection<Document> {
        self.db.collection(CROSSINGS)
    }

    fn grant_rows(&self) -> Collection<Document> {
        self.db.collection(GRANTS)
    }
}

fn decode_partition(document: &Document) -> Result<Partition> {
    json("partition", text(document, "record")?)
}

#[async_trait]
impl PartitionStore for MongoPartitionStore {
    async fn put(&self, partition: Partition) -> Result<()> {
        let id = partition.id.as_str().to_string();
        let document = doc! { "_id": &id, "record": serde_json::to_string(&partition)? };
        let rows = self.partition_rows();
        retry_upsert("write the partition table", || {
            rows.replace_one(doc! { "_id": &id }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn get(&self, id: &PartitionId) -> Result<Option<Partition>> {
        let document = self
            .partition_rows()
            .find_one(doc! { "_id": id.as_str() })
            .await
            .map_err(|error| store_error("read the partition table", &error))?;
        document.as_ref().map(decode_partition).transpose()
    }

    async fn list(&self) -> Result<Vec<Partition>> {
        let cursor = self
            .partition_rows()
            .find(Document::new())
            .with_options(sorted(doc! { "_id": 1 }, None))
            .await
            .map_err(|error| store_error("list the partition table", &error))?;
        collect(cursor, "list the partition table").await?.iter().map(decode_partition).collect()
    }

    async fn delete(&self, id: &PartitionId) -> Result<bool> {
        let result = self
            .partition_rows()
            .delete_one(doc! { "_id": id.as_str() })
            .await
            .map_err(|error| store_error("delete from the partition table", &error))?;
        Ok(result.deleted_count == 1)
    }

    async fn record_crossing(&self, record: CrossingRecord) -> Result<()> {
        self.crossing_rows()
            .insert_one(doc! {
                "_id": &record.id,
                "at_us": record.at.timestamp_micros(),
                "run": &record.run,
                "from": record.from.as_str(),
                "to": record.to.as_str(),
                "fingerprint": &record.fingerprint,
                "outcome": record.outcome.as_str(),
                "record": serde_json::to_string(&record)?,
            })
            .await
            .map_err(|error| store_error("record a crossing", &error))?;
        Ok(())
    }

    async fn crossings(&self, filter: &CrossingFilter) -> Result<Vec<CrossingRecord>> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut query = Document::new();
        if let Some(from) = &filter.from {
            query.insert("from", from.as_str());
        }
        if let Some(to) = &filter.to {
            query.insert("to", to.as_str());
        }
        if let Some(run) = &filter.run {
            query.insert("run", run);
        }
        if let Some(fingerprint) = &filter.fingerprint {
            query.insert("fingerprint", fingerprint);
        }
        if !filter.outcomes.is_empty() {
            let outcomes: Vec<&str> =
                filter.outcomes.iter().map(|outcome| outcome.as_str()).collect();
            query.insert("outcome", doc! { "$in": outcomes });
        }
        if let Some(since) = filter.since {
            query.insert("at_us", doc! { "$gte": since.timestamp_micros() });
        }
        let cursor = self
            .crossing_rows()
            .find(query)
            .with_options(sorted(doc! { "at_us": 1, "_id": 1 }, filter.limit))
            .await
            .map_err(|error| store_error("read the crossings", &error))?;
        collect(cursor, "read the crossings")
            .await?
            .iter()
            .map(|document| json("crossing record", text(document, "record")?))
            .collect()
    }

    async fn grant(&self, run: &str, fingerprint: &str) -> Result<()> {
        let id = doc! { "run": run, "fingerprint": fingerprint };
        let update = doc! { "$setOnInsert": { "run": run, "fingerprint": fingerprint } };
        let rows = self.grant_rows();
        retry_upsert("record a crossing grant", || {
            rows.update_one(doc! { "_id": id.clone() }, update.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn grants(&self, run: &str) -> Result<Vec<String>> {
        let cursor = self
            .grant_rows()
            .find(doc! { "run": run })
            .with_options(sorted(doc! { "fingerprint": 1 }, None))
            .await
            .map_err(|error| store_error("read the crossing grants", &error))?;
        collect(cursor, "read the crossing grants")
            .await?
            .iter()
            .map(|document| text(document, "fingerprint").map(str::to_string))
            .collect()
    }

    async fn release_run(&self, run: &str) -> Result<u64> {
        let result = self
            .grant_rows()
            .delete_many(doc! { "run": run })
            .await
            .map_err(|error| store_error("release the crossing grants", &error))?;
        Ok(result.deleted_count)
    }
}
