//! The Firestore [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `firestore`), and the
//! helpers the kernel's other Firestore stores share.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use firestore::errors::{BackoffError, FirestoreError};
use firestore::{FirestoreDb, FirestoreTransactionOptions, FirestoreWritePrecondition};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::{ProcessFilter, ProcessStore, SpendScope, day_number, stored_micro_usd, stored_total};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

const PROCESSES: &str = "adk_kernel_processes";
const PROCESS_SPEND: &str = "adk_kernel_spend";
const PARTITION_SPEND: &str = "adk_kernel_partition_spend";

/// The backend name in [`KernelError::Store`].
pub(super) const BACKEND: &str = "firestore";

/// The longest identifier a document is named after: its hex must fit Firestore's 1,500-byte
/// document IDs.
const MAX_ID_BYTES: usize = 700;

/// How long one operation may spend retrying transactions Firestore aborted for contention.
const TRANSACTION_DEADLINE: chrono::TimeDelta = chrono::TimeDelta::seconds(120);

pub(super) type TxResult<T> = std::result::Result<T, BackoffError<KernelError>>;
pub(super) type TxFuture<'b, T> = BoxFuture<'b, TxResult<T>>;

pub(super) fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: BACKEND, reason: format!("could not {action}: {error}") }
}

/// A client of `project_id` with Application Default Credentials; the `firestore` crate
/// reaches the emulator when `FIRESTORE_EMULATOR_HOST` is set.
pub(super) async fn connect(project_id: &str) -> Result<FirestoreDb> {
    FirestoreDb::new(project_id).await.map_err(|error| store_error("connect to Firestore", &error))
}

/// The document ID of `id`: the hex of its UTF-8 bytes, which holds no `/` and orders as
/// the bytes do, so a query ordered by document name follows byte order as on every backend.
pub(super) fn doc_id(what: &str, id: &str) -> Result<String> {
    if id.len() > MAX_ID_BYTES {
        return Err(KernelError::Store {
            backend: BACKEND,
            reason: format!(
                "a Firestore {what} id is at most {MAX_ID_BYTES} bytes, and this one has {}",
                id.len()
            ),
        });
    }
    Ok(id.bytes().map(|byte| format!("{byte:02x}")).collect())
}

/// The document ID of a key of several parts: the SHA-256 of their JSON array, in hex. The
/// document stores the parts, and every read compares them.
pub(super) fn digest_id(parts: &[&str]) -> String {
    use sha2::Digest as _;

    let encoded = serde_json::to_vec(parts).unwrap_or_default();
    sha2::Sha256::digest(&encoded).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn transaction_options() -> FirestoreTransactionOptions {
    FirestoreTransactionOptions::new().with_max_elapsed_time(TRANSACTION_DEADLINE)
}

/// Maps a failure inside a transaction: retried when Firestore says it may succeed, such as
/// a contention abort.
pub(super) fn tx_fail(
    action: &'static str,
) -> impl Fn(FirestoreError) -> BackoffError<KernelError> {
    move |error| {
        let retryable = matches!(&error, FirestoreError::DatabaseError(db) if db.retry_possible);
        let error = store_error(action, &error);
        if retryable { BackoffError::transient(error) } else { BackoffError::permanent(error) }
    }
}

/// The [`KernelError`] a transaction body failed with, or the transaction's own failure.
pub(super) fn transaction_error(error: FirestoreError, action: &str) -> KernelError {
    match error {
        FirestoreError::ErrorInTransaction(inner) => match inner.source.downcast::<KernelError>() {
            Ok(error) => *error,
            Err(source) => store_error(action, &source),
        },
        other => store_error(action, &other),
    }
}

/// Deletes the document `doc_name` of `collection` when `matches` accepts what it holds,
/// reading and deleting in one transaction, so of two concurrent deletes one answers `true`.
pub(super) async fn delete_where<T>(
    db: &FirestoreDb,
    collection: &'static str,
    doc_name: String,
    matches: impl Fn(&T) -> bool + Send + Sync + 'static,
    action: &'static str,
) -> Result<bool>
where
    T: for<'de> Deserialize<'de> + Send + Sync + 'static,
{
    let (doc_name, matches) = (Arc::new(doc_name), Arc::new(matches));
    db.run_transaction_with_options(
        move |db, transaction| -> TxFuture<'_, bool> {
            let (doc_name, matches) = (doc_name.clone(), matches.clone());
            Box::pin(async move {
                let stored: Option<T> = db
                    .fluent()
                    .select()
                    .by_id_in(collection)
                    .obj()
                    .one(doc_name.as_str())
                    .await
                    .map_err(tx_fail(action))?;
                if !stored.as_ref().is_some_and(|stored| matches(stored)) {
                    return Ok(false);
                }
                db.fluent()
                    .delete()
                    .from(collection)
                    .document_id(doc_name.as_str())
                    .add_to_transaction(transaction)
                    .map_err(tx_fail(action))?;
                Ok(true)
            })
        },
        transaction_options(),
    )
    .await
    .map_err(|error| transaction_error(error, action))
}

/// JSON text a store wrote, read back.
pub(super) fn json<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| KernelError::Store {
        backend: BACKEND,
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// One process, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcessDoc {
    pid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    partition: String,
    queue: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wake: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    suspended_by: Option<String>,
    #[serde(default)]
    suspend_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agenda_item: Option<String>,
    labels: String,
}

impl ProcessDoc {
    fn new(meta: &ProcessMeta) -> Result<Self> {
        Ok(Self {
            pid: meta.pid.clone(),
            parent: meta.parent.clone(),
            partition: meta.partition.as_str().to_string(),
            queue: meta.queue.as_str().to_string(),
            kind: serde_json::to_string(&meta.kind)?,
            wake: meta.wake.as_ref().map(serde_json::to_string).transpose()?,
            suspended_by: meta.suspended_by.as_ref().map(serde_json::to_string).transpose()?,
            suspend_requested: meta.suspend_requested,
            agenda_item: meta.agenda_item.clone(),
            labels: serde_json::to_string(&meta.labels)?,
        })
    }

    fn decode(self) -> Result<ProcessMeta> {
        Ok(ProcessMeta {
            kind: json("process kind", &self.kind)?,
            wake: self.wake.map(|wake| json("wake set", &wake)).transpose()?,
            suspended_by: self
                .suspended_by
                .map(|suspension| json("suspension", &suspension))
                .transpose()?,
            labels: json("label set", &self.labels)?,
            partition: PartitionId::new(self.partition),
            queue: QueueClass::from_name(&self.queue),
            suspend_requested: self.suspend_requested,
            agenda_item: self.agenda_item,
            parent: self.parent,
            pid: self.pid,
        })
    }
}

/// The fields one update of a process sets; `None` deletes the field.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct ProcessFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    wake: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suspend_requested: Option<bool>,
}

/// One day's spend of a process, or of a partition when `pid` is absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SpendDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<String>,
    partition: String,
    day: i64,
    #[serde(default, skip_serializing)]
    micro_usd: i64,
}

/// A [`ProcessStore`] in Firestore, shared by every replica (spec 3.2 §7.1, §13 Q8, feature
/// `firestore`). **Alpha** until its live conformance suite passes in CI.
///
/// | Collection (document ID) | Contents |
/// |--------------------------|----------|
/// | `adk_kernel_processes` (hex of the pid) | `pid`, `partition`, `queue`, `suspend_requested` and, when set, `parent` and `agenda_item`; `kind`, `labels` and, when set, `wake` and `suspended_by` as JSON text |
/// | `adk_kernel_spend` (SHA-256 of pid, partition and day) | One process's spend in one partition on one UTC day: `pid`, `partition`, `day` (days from the common era), `micro_usd` |
/// | `adk_kernel_partition_spend` (SHA-256 of partition and day) | One partition's spend on one UTC day: `partition`, `day`, `micro_usd` |
///
/// `list` queries with equality filters only and Firestore orders their results by
/// document name, the hex of the pid, which orders as the pid's UTF-8 bytes do; Firestore
/// serves such queries from its automatic single-field indexes, so no composite index is
/// needed. A pid longer than 700 bytes is refused. `set_wake`, `set_queue` and
/// `set_suspend_requested` write one field under a field mask with an exists precondition,
/// so writers changing different fields of one row never lose each other's update.
/// `add_spend` adds to the process's and the partition's day totals with Firestore's
/// server-side increment in one commit, so concurrent additions all count; `spend` reads
/// the scope's day totals with one equality query and sums the days in range. Deleting a
/// process keeps its spend.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::FirestoreProcessStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreProcessStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreProcessStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreProcessStore").finish_non_exhaustive()
    }
}

impl FirestoreProcessStore {
    /// A store over `db`, such as the client `adk_session::FirestoreRunStore::from_db` uses,
    /// so the kernel's collections sit beside the run, schedule and governance stores.
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

    /// Prepares the store's storage. Firestore creates collections on first write and
    /// serves every query from its automatic single-field indexes, so there is nothing to
    /// create; the method exists so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    /// Writes `fields` under the mask `mask` to the process `pid`, failing when there is
    /// none.
    async fn update(&self, pid: &str, mask: &str, fields: ProcessFields) -> Result<()> {
        let result = self
            .db
            .fluent()
            .update()
            .fields([mask])
            .in_col(PROCESSES)
            .precondition(FirestoreWritePrecondition::Exists(true))
            .document_id(doc_id("process", pid)?)
            .object(&fields)
            .execute::<ProcessFields>()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(FirestoreError::DataNotFoundError(_)) => {
                Err(KernelError::NotFound { pid: pid.into() })
            }
            Err(error) => Err(store_error("update the process table", &error)),
        }
    }

    /// The day totals of `field` = `key` in `collection`.
    async fn day_totals(&self, collection: &str, field: &str, key: &str) -> Result<Vec<SpendDoc>> {
        self.db
            .fluent()
            .select()
            .from(collection)
            .filter(|q| q.for_all([q.field(field).eq(key)]))
            .obj::<SpendDoc>()
            .query()
            .await
            .map_err(|error| store_error("read the spend totals", &error))
    }
}

#[async_trait]
impl ProcessStore for FirestoreProcessStore {
    async fn put(&self, meta: ProcessMeta) -> Result<()> {
        let doc = ProcessDoc::new(&meta)?;
        self.db
            .fluent()
            .update()
            .in_col(PROCESSES)
            .document_id(doc_id("process", &meta.pid)?)
            .object(&doc)
            .execute::<ProcessDoc>()
            .await
            .map(|_| ())
            .map_err(|error| store_error("write the process table", &error))
    }

    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>> {
        let Ok(id) = doc_id("process", pid) else {
            return Ok(None);
        };
        let doc: Option<ProcessDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(PROCESSES)
            .obj()
            .one(id)
            .await
            .map_err(|error| store_error("read the process table", &error))?;
        doc.filter(|doc| doc.pid == pid).map(ProcessDoc::decode).transpose()
    }

    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut select = self.db.fluent().select().from(PROCESSES).filter(|q| {
            q.for_all([
                filter.partition.as_ref().and_then(|p| q.field("partition").eq(p.as_str())),
                filter.queue.as_ref().and_then(|queue| q.field("queue").eq(queue.as_str())),
                filter.parent.as_ref().and_then(|parent| q.field("parent").eq(parent.as_str())),
                filter
                    .suspend_requested
                    .and_then(|requested| q.field("suspend_requested").eq(requested)),
            ])
        });
        // Firestore takes a limit up to `i32::MAX`; a larger one limits nothing.
        if let Some(limit) = filter.limit.and_then(|limit| i32::try_from(limit).ok()) {
            select = select.limit(limit.unsigned_abs());
        }
        let docs: Vec<ProcessDoc> =
            select.obj().query().await.map_err(|error| store_error("list processes", &error))?;
        docs.into_iter().map(ProcessDoc::decode).collect()
    }

    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()> {
        let wake = wake.as_ref().map(serde_json::to_string).transpose()?;
        self.update(pid, "wake", ProcessFields { wake, ..ProcessFields::default() }).await
    }

    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()> {
        let queue = Some(queue.as_str().to_string());
        self.update(pid, "queue", ProcessFields { queue, ..ProcessFields::default() }).await
    }

    async fn delete(&self, pid: &str) -> Result<bool> {
        let Ok(id) = doc_id("process", pid) else {
            return Ok(false);
        };
        let pid = pid.to_string();
        let matches = move |doc: &ProcessDoc| doc.pid == pid;
        delete_where(&self.db, PROCESSES, id, matches, "delete from the process table").await
    }

    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> Result<()> {
        let fields =
            ProcessFields { suspend_requested: Some(requested), ..ProcessFields::default() };
        self.update(pid, "suspend_requested", fields).await
    }

    async fn add_spend(
        &self,
        pid: &str,
        partition: &PartitionId,
        day: NaiveDate,
        delta: SpendDelta,
    ) -> Result<()> {
        let amount = stored_micro_usd(BACKEND, delta)?;
        let day = day_number(day);
        let day_text = day.to_string();
        let process = SpendDoc {
            pid: Some(pid.to_string()),
            partition: partition.as_str().to_string(),
            day,
            micro_usd: 0,
        };
        let shared = SpendDoc { pid: None, ..process.clone() };
        let writes = Arc::new([
            (
                PROCESS_SPEND,
                digest_id(&[pid, partition.as_str(), &day_text]),
                vec!["pid", "partition", "day"],
                process,
            ),
            (
                PARTITION_SPEND,
                digest_id(&[partition.as_str(), &day_text]),
                vec!["partition", "day"],
                shared,
            ),
        ]);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, ()> {
                    let writes = writes.clone();
                    Box::pin(async move {
                        for (collection, id, mask, doc) in writes.iter() {
                            db.fluent()
                                .update()
                                .fields(mask)
                                .in_col(collection)
                                .document_id(id)
                                .object(doc)
                                .transforms(|t| t.fields([t.field("micro_usd").increment(amount)]))
                                .add_to_transaction(transaction)
                                .map_err(tx_fail("add to the spend totals"))?;
                        }
                        Ok(())
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|error| transaction_error(error, "add to the spend totals"))
    }

    async fn spend(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<u64> {
        let docs = match scope {
            SpendScope::Process(pid) => self.day_totals(PROCESS_SPEND, "pid", pid).await?,
            SpendScope::Partition(partition) => {
                self.day_totals(PARTITION_SPEND, "partition", partition.as_str()).await?
            }
        };
        let days = day_number(from)..=day_number(to_inclusive);
        let total = docs
            .iter()
            .filter(|doc| days.contains(&doc.day))
            .fold(0_i64, |total, doc| total.saturating_add(doc.micro_usd));
        stored_total(BACKEND, total)
    }
}
