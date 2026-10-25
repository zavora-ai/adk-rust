//! The Firestore [`LedgerStore`] (spec 3.2 §7.10, §13 Q8, features `proc` and `firestore`).

use std::sync::Arc;

use async_trait::async_trait;
use firestore::{FirestoreDb, FirestoreQueryDirection};
use serde::{Deserialize, Serialize};

use super::day_number;
use super::firestore::{
    BACKEND, TxFuture, connect, doc_id, json, store_error, transaction_error, transaction_options,
    tx_fail,
};
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::proc::{
    LedgerEntry, LedgerQuery, LedgerStore, Observation, ObservationQuery, PassState,
    order_observations,
};

const LEDGER: &str = "adk_kernel_ledger";
const COUNTERS: &str = "adk_kernel_ledger_counters";
const OBSERVATIONS: &str = "adk_kernel_observations";
const PASS_STATES: &str = "adk_kernel_proc_state";

/// The document of the ledger's sequence counter in `adk_kernel_ledger_counters`.
const SEQUENCE: &str = "ledger";

/// How many rows one read fetches.
const CHUNK: u32 = 512;

/// The ledger's sequence counter, as stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CounterDoc {
    seq: i64,
}

/// One ledger row, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RowDoc {
    seq: i64,
    ts_ms: i64,
    partition: String,
    agent: String,
    kind: String,
    row: String,
}

impl RowDoc {
    fn decode(&self) -> Result<LedgerEntry> {
        let mut entry: LedgerEntry = json("ledger row", &self.row)?;
        entry.seq = u64::try_from(self.seq).map_err(|_| KernelError::Store {
            backend: BACKEND,
            reason: format!("a ledger row has the negative sequence number {}", self.seq),
        })?;
        Ok(entry)
    }
}

/// One observation, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservationDoc {
    id: String,
    partition: String,
    as_of: i64,
    body: String,
}

/// One partition's pass state, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PassStateDoc {
    partition: String,
    body: String,
}

/// The document ID of row `seq`: fixed-width decimal, so document names order as the
/// numbers do.
fn row_id(seq: i64) -> String {
    format!("{seq:020}")
}

/// A [`LedgerStore`] in Firestore, shared by every replica (spec 3.2 §7.1, §7.10, §13 Q8,
/// features `proc` and `firestore`).
///
/// | Collection (document ID) | Contents |
/// |--------------------------|----------|
/// | `adk_kernel_ledger` (the sequence number, fixed-width) | `seq`, `ts_ms` (the instant in UTC milliseconds), `partition`, `agent`, `kind`, and `row`, the row as JSON text |
/// | `adk_kernel_ledger_counters` (`ledger`) | `seq`, the last sequence number taken |
/// | `adk_kernel_observations` (hex of the id) | `id`, `partition`, `as_of` (days from the common era) and `body`, the observation as JSON text |
/// | `adk_kernel_proc_state` (hex of the partition) | `partition` and `body`, the pass state as JSON text |
///
/// An append reads and raises the counter and creates the row in one serializable
/// transaction, so concurrent appends conflict on the counter, the loser retries, and rows
/// commit in number order. Every query is a range over one field, which Firestore serves
/// from its automatic single-field indexes, so no composite index is needed: `list` walks
/// the rows after its cursor in number order, or the rows of its `[from, to)` window in time
/// order, and applies the other filters in memory, as the Redis backend does. Observations
/// and pass states are read with at most one equality filter and ordered in memory.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::proc::FirestoreLedgerStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreLedgerStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreLedgerStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreLedgerStore").finish_non_exhaustive()
    }
}

impl FirestoreLedgerStore {
    /// A store over `db`, such as the process store's.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Connects to `project_id` with Application Default Credentials.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the client cannot be created.
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

    /// Up to [`CHUNK`] rows after sequence number `cursor`, in number order.
    async fn page(&self, cursor: i64) -> Result<Vec<RowDoc>> {
        self.db
            .fluent()
            .select()
            .from(LEDGER)
            .filter(|q| q.for_all([q.field("seq").greater_than(cursor)]))
            .order_by([("seq", FirestoreQueryDirection::Ascending)])
            .limit(CHUNK)
            .obj()
            .query()
            .await
            .map_err(|error| store_error("read the ledger", &error))
    }

    /// The rows of the `[from, to)` window `query` names, in time order.
    async fn window(&self, query: &LedgerQuery) -> Result<Vec<RowDoc>> {
        let from = query.from.map(|from| from.timestamp_millis());
        let to = query.to.map(|to| to.timestamp_millis());
        self.db
            .fluent()
            .select()
            .from(LEDGER)
            .filter(|q| {
                q.for_all([
                    from.and_then(|from| q.field("ts_ms").greater_than_or_equal(from)),
                    to.and_then(|to| q.field("ts_ms").less_than(to)),
                ])
            })
            .order_by([("ts_ms", FirestoreQueryDirection::Ascending)])
            .obj()
            .query()
            .await
            .map_err(|error| store_error("read the ledger", &error))
    }
}

#[async_trait]
impl LedgerStore for FirestoreLedgerStore {
    async fn append(&self, mut entry: LedgerEntry) -> Result<LedgerEntry> {
        entry.seq = 0;
        let row = Arc::new(RowDoc {
            seq: 0,
            ts_ms: entry.ts.timestamp_millis(),
            partition: entry.partition.as_str().to_string(),
            agent: entry.agent.clone(),
            kind: entry.kind.clone(),
            row: serde_json::to_string(&entry)?,
        });
        let seq = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, i64> {
                    let row = row.clone();
                    Box::pin(async move {
                        let counter: Option<CounterDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(COUNTERS)
                            .obj()
                            .one(SEQUENCE)
                            .await
                            .map_err(tx_fail("take a ledger sequence number"))?;
                        let seq = counter.unwrap_or_default().seq + 1;
                        db.fluent()
                            .update()
                            .in_col(COUNTERS)
                            .document_id(SEQUENCE)
                            .object(&CounterDoc { seq })
                            .add_to_transaction(transaction)
                            .map_err(tx_fail("take a ledger sequence number"))?;
                        db.fluent()
                            .update()
                            .in_col(LEDGER)
                            .document_id(row_id(seq))
                            .object(&RowDoc { seq, ..(*row).clone() })
                            .add_to_transaction(transaction)
                            .map_err(tx_fail("append to the ledger"))?;
                        Ok(seq)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|error| transaction_error(error, "append to the ledger"))?;
        entry.seq = u64::try_from(seq).map_err(|_| KernelError::Store {
            backend: BACKEND,
            reason: format!("the ledger counter holds a negative sequence number {seq}"),
        })?;
        Ok(entry)
    }

    async fn list(&self, query: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut found = Vec::new();
        if limit == 0 {
            return Ok(found);
        }
        // The window is applied by the query, in milliseconds as the SQL backends compare
        // it; the remaining fields are checked here.
        let mut rest = query.clone();
        rest.from = None;
        rest.to = None;
        if query.from.is_some() || query.to.is_some() {
            let mut rows = self.window(query).await?;
            rows.sort_by_key(|row| row.seq);
            for row in &rows {
                let entry = row.decode()?;
                if rest.matches(&entry) {
                    found.push(entry);
                    if found.len() == limit {
                        break;
                    }
                }
            }
            return Ok(found);
        }
        let mut cursor = query.after.map_or(0, |after| i64::try_from(after).unwrap_or(i64::MAX));
        loop {
            let rows = self.page(cursor).await?;
            for row in &rows {
                cursor = row.seq;
                let entry = row.decode()?;
                if rest.matches(&entry) {
                    found.push(entry);
                    if found.len() == limit {
                        return Ok(found);
                    }
                }
            }
            if rows.len() < CHUNK as usize {
                return Ok(found);
            }
        }
    }

    async fn put_observation(&self, observation: Observation) -> Result<()> {
        let doc = ObservationDoc {
            id: observation.id.clone(),
            partition: observation.partition.as_str().to_string(),
            as_of: day_number(observation.as_of),
            body: serde_json::to_string(&observation)?,
        };
        self.db
            .fluent()
            .update()
            .in_col(OBSERVATIONS)
            .document_id(doc_id("observation", &observation.id)?)
            .object(&doc)
            .execute::<ObservationDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write an observation", &error))
    }

    async fn observations(&self, query: &ObservationQuery) -> Result<Vec<Observation>> {
        let docs: Vec<ObservationDoc> = self
            .db
            .fluent()
            .select()
            .from(OBSERVATIONS)
            .filter(|q| {
                q.for_all([query
                    .partition
                    .as_ref()
                    .and_then(|partition| q.field("partition").eq(partition.as_str()))])
            })
            .obj()
            .query()
            .await
            .map_err(|error| store_error("read the observations", &error))?;
        let mut observations = docs
            .iter()
            .map(|doc| json::<Observation>("observation", &doc.body))
            .collect::<Result<Vec<_>>>()?;
        observations.retain(|observation| query.matches(observation));
        order_observations(&mut observations);
        observations.truncate(query.limit.unwrap_or(usize::MAX));
        Ok(observations)
    }

    async fn pass_state(&self, partition: &PartitionId) -> Result<Option<PassState>> {
        let Ok(doc_name) = doc_id("partition", partition.as_str()) else {
            return Ok(None);
        };
        let doc: Option<PassStateDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(PASS_STATES)
            .obj()
            .one(doc_name)
            .await
            .map_err(|error| store_error("read the pass state", &error))?;
        doc.filter(|doc| doc.partition == partition.as_str())
            .map(|doc| json("pass state", &doc.body))
            .transpose()
    }

    async fn pass_states(&self) -> Result<Vec<PassState>> {
        let docs: Vec<PassStateDoc> = self
            .db
            .fluent()
            .select()
            .from(PASS_STATES)
            .obj()
            .query()
            .await
            .map_err(|error| store_error("read the pass states", &error))?;
        let mut states = docs
            .iter()
            .map(|doc| json::<PassState>("pass state", &doc.body))
            .collect::<Result<Vec<_>>>()?;
        states.sort_by(|a, b| a.partition.cmp(&b.partition));
        Ok(states)
    }

    async fn put_pass_state(&self, state: PassState) -> Result<()> {
        let doc = PassStateDoc {
            partition: state.partition.as_str().to_string(),
            body: serde_json::to_string(&state)?,
        };
        self.db
            .fluent()
            .update()
            .in_col(PASS_STATES)
            .document_id(doc_id("partition", state.partition.as_str())?)
            .object(&doc)
            .execute::<PassStateDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write the pass state", &error))
    }
}
