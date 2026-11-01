//! The MongoDB [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `mongodb`), and the helpers
//! the kernel's other MongoDB stores share.

use async_trait::async_trait;
use chrono::NaiveDate;
use mongodb::bson::{Bson, Document, doc};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, FindOptions, IndexOptions};
use mongodb::{Client, Collection, Cursor, Database, IndexModel};

use super::{
    ProcessFilter, ProcessStore, SpendScope, UsageTotals, counters, day_number, stored_amount,
    stored_micro_usd, stored_total,
};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

const PROCESSES: &str = "adk_kernel_processes";
const SPEND: &str = "adk_kernel_spend";

/// The backend name in [`KernelError::Store`].
pub(super) const BACKEND: &str = "mongodb";

/// MongoDB's duplicate key error code.
const DUPLICATE_KEY: i32 = 11000;

/// Attempts at an upsert that collided with a concurrent one before the error is returned.
const UPSERT_ATTEMPTS: u32 = 5;

pub(super) fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: BACKEND, reason: format!("could not {action}: {error}") }
}

/// The database `database_name` on the deployment `connection_string` names.
pub(super) async fn connect(connection_string: &str, database_name: &str) -> Result<Database> {
    let options = ClientOptions::parse(connection_string)
        .await
        .map_err(|error| store_error("parse the MongoDB connection string", &error))?;
    let client = Client::with_options(options)
        .map_err(|error| store_error("build the MongoDB client", &error))?;
    Ok(client.database(database_name))
}

/// Creates each named index of `collection`; an index that exists is left as it is.
pub(super) async fn create_indexes(
    collection: &Collection<Document>,
    indexes: &[(&str, Document)],
) -> Result<()> {
    for (name, keys) in indexes {
        let model = IndexModel::builder()
            .keys(keys.clone())
            .options(IndexOptions::builder().name((*name).to_string()).build())
            .build();
        collection
            .create_index(model)
            .await
            .map_err(|error| store_error(&format!("create the index {name}"), &error))?;
    }
    Ok(())
}

/// Every document `cursor` yields.
pub(super) async fn collect(mut cursor: Cursor<Document>, action: &str) -> Result<Vec<Document>> {
    let mut documents = Vec::new();
    while cursor.advance().await.map_err(|error| store_error(action, &error))? {
        documents.push(cursor.deserialize_current().map_err(|error| store_error(action, &error))?);
    }
    Ok(documents)
}

/// A text field every stored document of the kind has.
pub(super) fn text<'a>(document: &'a Document, field: &str) -> Result<&'a str> {
    document.get_str(field).map_err(|_| KernelError::Store {
        backend: BACKEND,
        reason: format!("a stored document has no text field `{field}`"),
    })
}

/// A text field a stored document may lack.
pub(super) fn optional_text(document: &Document, field: &str) -> Option<String> {
    document.get_str(field).ok().map(str::to_string)
}

/// An integer field the server may return as `Int32`, `Int64` or `Double`.
pub(super) fn read_i64(document: &Document, field: &str) -> Option<i64> {
    match document.get(field)? {
        Bson::Int32(value) => Some(i64::from(*value)),
        Bson::Int64(value) => Some(*value),
        Bson::Double(value) => Some(*value as i64),
        _ => None,
    }
}

/// JSON text a store wrote, read back.
pub(super) fn json<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| KernelError::Store {
        backend: BACKEND,
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// A `limit` as `find` takes it. `None` for no limit; callers answer a limit of 0 themselves,
/// because MongoDB reads a limit of 0 as no limit.
pub(super) fn find_limit(limit: Option<usize>) -> Option<i64> {
    limit.map(|limit| i64::try_from(limit).unwrap_or(i64::MAX))
}

/// `find` options sorting by `sort` and returning at most `limit` documents.
pub(super) fn sorted(sort: Document, limit: Option<usize>) -> FindOptions {
    FindOptions::builder().sort(sort).limit(find_limit(limit)).build()
}

pub(super) fn is_duplicate_key(error: &mongodb::error::Error) -> bool {
    match error.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(write)) => write.code == DUPLICATE_KEY,
        ErrorKind::Command(command) => command.code == DUPLICATE_KEY,
        _ => false,
    }
}

/// Runs `upsert` again while it collides with a concurrent upsert of the same `_id`, which
/// MongoDB reports as a duplicate key error on servers that do not retry it themselves.
pub(super) async fn retry_upsert<T, F, Fut>(action: &str, mut upsert: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::IntoFuture<Output = mongodb::error::Result<T>>,
{
    let mut attempt = 1;
    loop {
        match upsert().await {
            Err(error) if is_duplicate_key(&error) && attempt < UPSERT_ATTEMPTS => attempt += 1,
            result => return result.map_err(|error| store_error(action, &error)),
        }
    }
}

/// A [`ProcessStore`] in MongoDB, shared by every replica (spec 3.2 §7.1, §13 Q8, feature
/// `mongodb`).
///
/// It works on a standalone `mongod`, a replica set or a sharded cluster: every change is a
/// single-document write, so it needs no multi-document transaction.
///
/// | Collection | `_id` | Contents |
/// |------------|-------|----------|
/// | `adk_kernel_processes` | The pid | `partition`, `queue`, `suspend_requested` and, when set, `parent` and `agenda_item`, the fields `list` filters on; `kind`, `labels` and, when set, `wake` and `suspended_by` as JSON text |
/// | `adk_kernel_spend` | `{pid, partition, day}` | `micro_usd`, with `pid`, `partition` and `day` (days from the common era) as the fields `spend` sums over, and `counters`: model calls, total tokens and units by name |
///
/// `list` sorts by `_id` under MongoDB's binary string comparison, so pids order by their
/// UTF-8 bytes, as on every backend. `set_wake`, `set_queue` and `set_suspend_requested` each
/// change one field, so writers changing different fields of one row never lose each other's
/// update. `add_spend` is one `$inc` upsert, so concurrent additions all count; deleting a
/// process keeps its spend, so a partition's totals for past days do not change when the run
/// store prunes a finished run.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::MongoProcessStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoProcessStore {
    db: Database,
}

impl MongoProcessStore {
    /// A store over `db`, such as the database `adk_session::MongoRunStore::from_database`
    /// uses, so the kernel's tables sit beside the run, schedule and governance stores.
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

    /// Creates the indexes `list` and `spend` read; an index that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached or refuses an index.
    pub async fn migrate(&self) -> Result<()> {
        create_indexes(
            &self.processes(),
            &[
                ("adk_kernel_processes_partition", doc! { "partition": 1, "_id": 1 }),
                ("adk_kernel_processes_queue", doc! { "queue": 1, "_id": 1 }),
                ("adk_kernel_processes_parent", doc! { "parent": 1, "_id": 1 }),
                ("adk_kernel_processes_suspend", doc! { "suspend_requested": 1, "_id": 1 }),
            ],
        )
        .await?;
        create_indexes(
            &self.spend_totals(),
            &[
                ("adk_kernel_spend_pid", doc! { "pid": 1, "day": 1 }),
                ("adk_kernel_spend_partition", doc! { "partition": 1, "day": 1 }),
            ],
        )
        .await
    }

    fn processes(&self) -> Collection<Document> {
        self.db.collection(PROCESSES)
    }

    fn spend_totals(&self) -> Collection<Document> {
        self.db.collection(SPEND)
    }

    /// Sets the fields `update` names on the row of `pid`, failing when there is none.
    async fn update(&self, pid: &str, update: Document) -> Result<()> {
        let result = self
            .processes()
            .update_one(doc! { "_id": pid }, update)
            .await
            .map_err(|error| store_error("update the process table", &error))?;
        if result.matched_count == 0 {
            return Err(KernelError::NotFound { pid: pid.into() });
        }
        Ok(())
    }
}

/// The stored document of `meta`.
fn encode(meta: &ProcessMeta) -> Result<Document> {
    let mut document = doc! {
        "_id": &meta.pid,
        "partition": meta.partition.as_str(),
        "queue": meta.queue.as_str(),
        "kind": serde_json::to_string(&meta.kind)?,
        "labels": serde_json::to_string(&meta.labels)?,
        "suspend_requested": meta.suspend_requested,
    };
    if let Some(parent) = &meta.parent {
        document.insert("parent", parent);
    }
    if let Some(wake) = &meta.wake {
        document.insert("wake", serde_json::to_string(wake)?);
    }
    if let Some(suspension) = &meta.suspended_by {
        document.insert("suspended_by", serde_json::to_string(suspension)?);
    }
    if let Some(item) = &meta.agenda_item {
        document.insert("agenda_item", item);
    }
    Ok(document)
}

/// Reads a stored document back into the [`ProcessMeta`] that was put.
fn decode(document: &Document) -> Result<ProcessMeta> {
    let pid = text(document, "_id")?.to_string();
    let structured = |field: &str| optional_text(document, field);
    Ok(ProcessMeta {
        parent: optional_text(document, "parent"),
        partition: PartitionId::new(text(document, "partition")?),
        queue: QueueClass::from_name(text(document, "queue")?),
        kind: json("process kind", text(document, "kind")?)?,
        wake: structured("wake").map(|wake| json("wake set", &wake)).transpose()?,
        suspended_by: structured("suspended_by")
            .map(|suspension| json("suspension", &suspension))
            .transpose()?,
        suspend_requested: document.get_bool("suspend_requested").unwrap_or(false),
        agenda_item: optional_text(document, "agenda_item"),
        labels: json("label set", text(document, "labels")?)?,
        pid,
    })
}

#[async_trait]
impl ProcessStore for MongoProcessStore {
    async fn put(&self, meta: ProcessMeta) -> Result<()> {
        let document = encode(&meta)?;
        let processes = self.processes();
        retry_upsert("write the process table", || {
            processes.replace_one(doc! { "_id": &meta.pid }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>> {
        let document = self
            .processes()
            .find_one(doc! { "_id": pid })
            .await
            .map_err(|error| store_error("read the process table", &error))?;
        document.as_ref().map(decode).transpose()
    }

    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut query = Document::new();
        if let Some(partition) = &filter.partition {
            query.insert("partition", partition.as_str());
        }
        if let Some(queue) = &filter.queue {
            query.insert("queue", queue.as_str());
        }
        if let Some(parent) = &filter.parent {
            query.insert("parent", parent);
        }
        if let Some(requested) = filter.suspend_requested {
            query.insert("suspend_requested", requested);
        }
        let cursor = self
            .processes()
            .find(query)
            .with_options(sorted(doc! { "_id": 1 }, filter.limit))
            .await
            .map_err(|error| store_error("list processes", &error))?;
        collect(cursor, "list processes").await?.iter().map(decode).collect()
    }

    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()> {
        let update = match &wake {
            Some(wake) => doc! { "$set": { "wake": serde_json::to_string(wake)? } },
            None => doc! { "$unset": { "wake": "" } },
        };
        self.update(pid, update).await
    }

    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()> {
        self.update(pid, doc! { "$set": { "queue": queue.as_str() } }).await
    }

    async fn delete(&self, pid: &str) -> Result<bool> {
        let result = self
            .processes()
            .delete_one(doc! { "_id": pid })
            .await
            .map_err(|error| store_error("delete from the process table", &error))?;
        Ok(result.deleted_count == 1)
    }

    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> Result<()> {
        self.update(pid, doc! { "$set": { "suspend_requested": requested } }).await
    }

    async fn add_spend(
        &self,
        pid: &str,
        partition: &PartitionId,
        day: NaiveDate,
        delta: SpendDelta,
    ) -> Result<()> {
        let mut increments = doc! { "micro_usd": stored_micro_usd(BACKEND, &delta)? };
        for (name, amount) in counters(&delta) {
            increments.insert(format!("counters.{name}"), stored_amount(BACKEND, name, amount)?);
        }
        let day = day_number(day);
        let key = doc! { "pid": pid, "partition": partition.as_str(), "day": day };
        let update = doc! {
            "$inc": increments,
            "$setOnInsert": { "pid": pid, "partition": partition.as_str(), "day": day },
        };
        let totals = self.spend_totals();
        retry_upsert("add to the spend totals", || {
            totals.update_one(doc! { "_id": key.clone() }, update.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn spend(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<u64> {
        let mut filter = match scope {
            SpendScope::Process(pid) => doc! { "pid": pid },
            SpendScope::Partition(partition) => doc! { "partition": partition.as_str() },
        };
        filter.insert("day", doc! { "$gte": day_number(from), "$lte": day_number(to_inclusive) });
        let pipeline = [
            doc! { "$match": filter },
            doc! { "$group": { "_id": Bson::Null, "total": { "$sum": "$micro_usd" } } },
        ];
        let cursor = self
            .spend_totals()
            .aggregate(pipeline)
            .await
            .map_err(|error| store_error("sum the spend totals", &error))?;
        let rows = collect(cursor, "sum the spend totals").await?;
        let sum = rows.first().and_then(|row| read_i64(row, "total")).unwrap_or(0);
        stored_total(BACKEND, sum)
    }

    async fn usage(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<UsageTotals> {
        let mut filter = match scope {
            SpendScope::Process(pid) => doc! { "pid": pid },
            SpendScope::Partition(partition) => doc! { "partition": partition.as_str() },
        };
        filter.insert("day", doc! { "$gte": day_number(from), "$lte": day_number(to_inclusive) });
        let cursor = self
            .spend_totals()
            .find(filter)
            .await
            .map_err(|error| store_error("read the usage totals", &error))?;
        let mut totals = UsageTotals::default();
        for row in collect(cursor, "read the usage totals").await? {
            let micro_usd = stored_total(BACKEND, read_i64(&row, "micro_usd").unwrap_or(0))?;
            totals.micro_usd = totals.micro_usd.saturating_add(micro_usd);
            let Ok(counters) = row.get_document("counters") else { continue };
            for name in counters.keys() {
                let amount = stored_total(BACKEND, read_i64(counters, name).unwrap_or(0))?;
                totals.add_counter(name, amount);
            }
        }
        Ok(totals)
    }
}
