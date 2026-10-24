//! The MongoDB [`LedgerStore`] (spec 3.2 §7.10, §13 Q8, features `proc` and `mongodb`).

use async_trait::async_trait;
use mongodb::bson::{Document, doc};
use mongodb::options::ReturnDocument;
use mongodb::{Collection, Database};

use super::day_number;
use super::mongodb::{
    collect, connect, create_indexes, json, read_i64, retry_upsert, sorted, store_error, text,
};
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::proc::{
    LedgerEntry, LedgerQuery, LedgerStore, Observation, ObservationQuery, PassState,
};

const LEDGER: &str = "adk_kernel_ledger";
const COUNTERS: &str = "adk_kernel_ledger_counters";
const OBSERVATIONS: &str = "adk_kernel_observations";
const PASS_STATES: &str = "adk_kernel_proc_state";

/// The `_id` of the ledger's sequence counter in `adk_kernel_ledger_counters`.
const SEQUENCE: &str = "ledger";

/// A [`LedgerStore`] in MongoDB, shared by every replica (spec 3.2 §7.1, §7.10, §13 Q8,
/// features `proc` and `mongodb`). **Alpha** until its live conformance suite passes in CI.
///
/// | Collection | `_id` | Contents |
/// |------------|-------|----------|
/// | `adk_kernel_ledger` | The sequence number | `ts_ms` (the instant in UTC milliseconds), `partition`, `agent` and `kind`, the fields `list` filters on, and `row`, the row as JSON text |
/// | `adk_kernel_ledger_counters` | `ledger` | `seq`, the last sequence number taken |
/// | `adk_kernel_observations` | The observation id | `partition`, `as_of` (days from the common era) and `body`, the observation as JSON text |
/// | `adk_kernel_proc_state` | The partition | `body`, the pass state as JSON text |
///
/// An append takes the next number with one `$inc` on the counter, then inserts the row
/// under it, so every row has its own increasing number. The two writes are separate, so a
/// reader paging with `after` while appends land can pass a row whose number was taken
/// before the last row it read but which was inserted after it, as with PostgreSQL's
/// sequence. Neither write needs a multi-document transaction, so a standalone `mongod`
/// serves the store as a replica set does.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::proc::MongoLedgerStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoLedgerStore {
    db: Database,
}

impl MongoLedgerStore {
    /// A store over `db`, such as the process store's.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Connects to `connection_string` and uses the database `database_name`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the connection string is invalid.
    pub async fn connect(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(connect(connection_string, database_name).await?))
    }

    /// The database the store writes to.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Creates the indexes `list` and `observations` read; an index that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached or refuses an index.
    pub async fn migrate(&self) -> Result<()> {
        create_indexes(
            &self.rows(),
            &[
                ("adk_kernel_ledger_ts", doc! { "ts_ms": 1 }),
                ("adk_kernel_ledger_partition", doc! { "partition": 1, "_id": 1 }),
                ("adk_kernel_ledger_kind", doc! { "kind": 1, "_id": 1 }),
            ],
        )
        .await?;
        create_indexes(
            &self.observation_rows(),
            &[("adk_kernel_observations_as_of", doc! { "partition": 1, "as_of": -1, "_id": 1 })],
        )
        .await
    }

    fn rows(&self) -> Collection<Document> {
        self.db.collection(LEDGER)
    }

    fn observation_rows(&self) -> Collection<Document> {
        self.db.collection(OBSERVATIONS)
    }

    fn pass_state_rows(&self) -> Collection<Document> {
        self.db.collection(PASS_STATES)
    }

    /// Takes the next sequence number.
    async fn next_seq(&self) -> Result<i64> {
        let counters: Collection<Document> = self.db.collection(COUNTERS);
        let counter = retry_upsert("take a ledger sequence number", || {
            counters
                .find_one_and_update(doc! { "_id": SEQUENCE }, doc! { "$inc": { "seq": 1_i64 } })
                .upsert(true)
                .return_document(ReturnDocument::After)
        })
        .await?;
        counter.as_ref().and_then(|counter| read_i64(counter, "seq")).ok_or_else(|| {
            store_error("take a ledger sequence number", &"the counter has no `seq` field")
        })
    }
}

fn decode_row(document: &Document) -> Result<LedgerEntry> {
    let seq = read_i64(document, "_id")
        .and_then(|seq| u64::try_from(seq).ok())
        .ok_or_else(|| store_error("read the ledger", &"a row has no sequence number"))?;
    let mut entry: LedgerEntry = json("ledger row", text(document, "row")?)?;
    entry.seq = seq;
    Ok(entry)
}

#[async_trait]
impl LedgerStore for MongoLedgerStore {
    async fn append(&self, mut entry: LedgerEntry) -> Result<LedgerEntry> {
        entry.seq = 0;
        let row = serde_json::to_string(&entry)?;
        let seq = self.next_seq().await?;
        self.rows()
            .insert_one(doc! {
                "_id": seq,
                "ts_ms": entry.ts.timestamp_millis(),
                "partition": entry.partition.as_str(),
                "agent": &entry.agent,
                "kind": &entry.kind,
                "row": row,
            })
            .await
            .map_err(|error| store_error("append to the ledger", &error))?;
        entry.seq = u64::try_from(seq).map_err(|_| KernelError::Store {
            backend: super::mongodb::BACKEND,
            reason: format!("the ledger counter holds a negative sequence number {seq}"),
        })?;
        Ok(entry)
    }

    async fn list(&self, query: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        if query.limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut filter = Document::new();
        if let Some(partition) = &query.partition {
            filter.insert("partition", partition.as_str());
        }
        if !query.kinds.is_empty() {
            filter.insert("kind", doc! { "$in": query.kinds.clone() });
        }
        if let Some(agent) = &query.agent {
            filter.insert("agent", agent);
        }
        let mut window = Document::new();
        if let Some(from) = query.from {
            window.insert("$gte", from.timestamp_millis());
        }
        if let Some(to) = query.to {
            window.insert("$lt", to.timestamp_millis());
        }
        if !window.is_empty() {
            filter.insert("ts_ms", window);
        }
        if let Some(after) = query.after {
            filter.insert("_id", doc! { "$gt": i64::try_from(after).unwrap_or(i64::MAX) });
        }
        let cursor = self
            .rows()
            .find(filter)
            .with_options(sorted(doc! { "_id": 1 }, query.limit))
            .await
            .map_err(|error| store_error("read the ledger", &error))?;
        collect(cursor, "read the ledger").await?.iter().map(decode_row).collect()
    }

    async fn put_observation(&self, observation: Observation) -> Result<()> {
        let document = doc! {
            "_id": &observation.id,
            "partition": observation.partition.as_str(),
            "as_of": day_number(observation.as_of),
            "body": serde_json::to_string(&observation)?,
        };
        let rows = self.observation_rows();
        retry_upsert("write an observation", || {
            rows.replace_one(doc! { "_id": &observation.id }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn observations(&self, query: &ObservationQuery) -> Result<Vec<Observation>> {
        if query.limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut filter = Document::new();
        if let Some(partition) = &query.partition {
            filter.insert("partition", partition.as_str());
        }
        if let Some(since) = query.since {
            filter.insert("as_of", doc! { "$gte": day_number(since) });
        }
        let cursor = self
            .observation_rows()
            .find(filter)
            .with_options(sorted(doc! { "as_of": -1, "_id": 1 }, query.limit))
            .await
            .map_err(|error| store_error("read the observations", &error))?;
        collect(cursor, "read the observations")
            .await?
            .iter()
            .map(|document| json("observation", text(document, "body")?))
            .collect()
    }

    async fn pass_state(&self, partition: &PartitionId) -> Result<Option<PassState>> {
        let document = self
            .pass_state_rows()
            .find_one(doc! { "_id": partition.as_str() })
            .await
            .map_err(|error| store_error("read the pass state", &error))?;
        document.map(|document| json("pass state", text(&document, "body")?)).transpose()
    }

    async fn pass_states(&self) -> Result<Vec<PassState>> {
        let cursor = self
            .pass_state_rows()
            .find(Document::new())
            .with_options(sorted(doc! { "_id": 1 }, None))
            .await
            .map_err(|error| store_error("read the pass states", &error))?;
        collect(cursor, "read the pass states")
            .await?
            .iter()
            .map(|document| json("pass state", text(document, "body")?))
            .collect()
    }

    async fn put_pass_state(&self, state: PassState) -> Result<()> {
        let partition = state.partition.as_str().to_string();
        let document = doc! { "_id": &partition, "body": serde_json::to_string(&state)? };
        let rows = self.pass_state_rows();
        retry_upsert("write the pass state", || {
            rows.replace_one(doc! { "_id": &partition }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }
}
