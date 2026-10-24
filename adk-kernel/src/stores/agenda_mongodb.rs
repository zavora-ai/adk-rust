//! The MongoDB [`AgendaStore`] (spec 3.2 §7.1, §7.5, §13 Q8, feature `mongodb`).

use async_trait::async_trait;
use mongodb::bson::{Document, doc};
use mongodb::{Collection, Database};

use super::mongodb::{
    collect, connect, create_indexes, json, retry_upsert, sorted, store_error, text,
};
use crate::agenda::{AgendaFilter, AgendaItem, AgendaStateKind, AgendaStore};
use crate::error::Result;

const AGENDA: &str = "adk_kernel_agenda";

/// An [`AgendaStore`] in MongoDB, shared by every replica (spec 3.2 §7.1, §7.5, §13 Q8,
/// feature `mongodb`). **Alpha** until its live conformance suite passes in CI.
///
/// One `adk_kernel_agenda` document per item, `_id` its id: `partition`, `state` (its kind)
/// and `created_us` (microseconds since the Unix epoch), the fields `list` filters and sorts
/// on, beside `item`, the item as JSON text, so it reads back exactly as written. `list`
/// sorts by `created_us`, then `_id` under MongoDB's binary string comparison, so ties order
/// by the ids' UTF-8 bytes. `transition` is one update filtered on `_id` and `state`, so two
/// replicas moving one item from the same state move it once. Every write is a
/// single-document write, so a standalone `mongod` serves it as a replica set does.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::MongoAgendaStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoAgendaStore {
    db: Database,
}

impl MongoAgendaStore {
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

    /// Creates the indexes `list` reads; an index that exists is kept.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the server cannot be
    /// reached or refuses an index.
    pub async fn migrate(&self) -> Result<()> {
        create_indexes(
            &self.items(),
            &[
                (
                    "adk_kernel_agenda_partition",
                    doc! { "partition": 1, "state": 1, "created_us": 1, "_id": 1 },
                ),
                ("adk_kernel_agenda_state", doc! { "state": 1, "created_us": 1, "_id": 1 }),
                ("adk_kernel_agenda_created", doc! { "created_us": 1, "_id": 1 }),
            ],
        )
        .await
    }

    fn items(&self) -> Collection<Document> {
        self.db.collection(AGENDA)
    }
}

/// The fields of `item` other than `_id`.
fn fields(item: &AgendaItem) -> Result<Document> {
    Ok(doc! {
        "partition": item.partition.as_str(),
        "state": item.state.kind().as_str(),
        "created_us": item.created_at.timestamp_micros(),
        "item": serde_json::to_string(item)?,
    })
}

fn decode(document: &Document) -> Result<AgendaItem> {
    json("agenda item", text(document, "item")?)
}

#[async_trait]
impl AgendaStore for MongoAgendaStore {
    async fn put(&self, item: AgendaItem) -> Result<()> {
        let mut document = doc! { "_id": &item.id };
        document.extend(fields(&item)?);
        let items = self.items();
        retry_upsert("write the agenda", || {
            items.replace_one(doc! { "_id": &item.id }, document.clone()).upsert(true)
        })
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<AgendaItem>> {
        let document = self
            .items()
            .find_one(doc! { "_id": id })
            .await
            .map_err(|error| store_error("read the agenda", &error))?;
        document.as_ref().map(decode).transpose()
    }

    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut query = Document::new();
        if let Some(partition) = &filter.partition {
            query.insert("partition", partition.as_str());
        }
        if !filter.states.is_empty() {
            let states: Vec<&str> = filter.states.iter().map(|state| state.as_str()).collect();
            query.insert("state", doc! { "$in": states });
        }
        let cursor = self
            .items()
            .find(query)
            .with_options(sorted(doc! { "created_us": 1, "_id": 1 }, filter.limit))
            .await
            .map_err(|error| store_error("list the agenda", &error))?;
        collect(cursor, "list the agenda").await?.iter().map(decode).collect()
    }

    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool> {
        let result = self
            .items()
            .update_one(
                doc! { "_id": &item.id, "state": from.as_str() },
                doc! { "$set": fields(&item)? },
            )
            .await
            .map_err(|error| store_error("update the agenda", &error))?;
        Ok(result.matched_count == 1)
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let result = self
            .items()
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|error| store_error("delete from the agenda", &error))?;
        Ok(result.deleted_count == 1)
    }
}
