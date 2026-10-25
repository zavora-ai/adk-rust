//! The MongoDB [`CronWatermarkStore`] (spec 3.2 §7.4, §13 Q8, feature `mongodb`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mongodb::bson::{Document, doc};
use mongodb::options::ReturnDocument;
use mongodb::{Collection, Database};

use super::mongodb::{connect, read_i64, retry_upsert, store_error};
use crate::error::Result;
use crate::watermark::CronWatermarkStore;

const WATERMARKS: &str = "adk_kernel_cron_watermarks";

/// A [`CronWatermarkStore`] in MongoDB, shared by every replica (spec 3.2 §7.4, §13 Q8,
/// feature `mongodb`).
///
/// | Collection | `_id` | Contents |
/// |------------|-------|----------|
/// | `adk_kernel_cron_watermarks` | The source's name | `cursor_us`, the cursor in microseconds since the Unix epoch |
///
/// An advance is one `findOneAndUpdate` upsert with `$max`, which keeps the later of the
/// stored cursor and the one given and returns the document as stored, so replicas writing
/// in any order never move a cursor back. Every read and write is by `_id`, and every write
/// is a single-document write, so a standalone `mongod` serves the store as a replica set
/// does.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store =
///     adk_kernel::MongoCronWatermarkStore::connect("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoCronWatermarkStore {
    db: Database,
}

impl MongoCronWatermarkStore {
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

    /// Prepares the store's storage. MongoDB creates the collection on first write and the
    /// `_id` index serves every read, so there is nothing to create; the method exists so
    /// every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    fn cursors(&self) -> Collection<Document> {
        self.db.collection(WATERMARKS)
    }
}

/// The instant a stored document's cursor names.
fn instant(document: &Document) -> Result<DateTime<Utc>> {
    let micros = read_i64(document, "cursor_us")
        .ok_or_else(|| store_error("read a cron watermark", &"the cursor is not an integer"))?;
    DateTime::from_timestamp_micros(micros).ok_or_else(|| {
        store_error("read a cron watermark", &format!("the cursor {micros} is out of range"))
    })
}

#[async_trait]
impl CronWatermarkStore for MongoCronWatermarkStore {
    async fn read(&self, source: &str) -> Result<Option<DateTime<Utc>>> {
        let document = self
            .cursors()
            .find_one(doc! { "_id": source })
            .await
            .map_err(|error| store_error("read a cron watermark", &error))?;
        document.as_ref().map(instant).transpose()
    }

    async fn advance(&self, source: &str, cursor: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let update = doc! { "$max": { "cursor_us": cursor.timestamp_micros() } };
        let cursors = self.cursors();
        let stored = retry_upsert("advance a cron watermark", || {
            cursors
                .find_one_and_update(doc! { "_id": source }, update.clone())
                .upsert(true)
                .return_document(ReturnDocument::After)
        })
        .await?;
        stored.as_ref().map(instant).transpose()?.ok_or_else(|| {
            store_error("advance a cron watermark", &"the upsert returned no document")
        })
    }

    async fn delete(&self, source: &str) -> Result<bool> {
        let result = self
            .cursors()
            .delete_one(doc! { "_id": source })
            .await
            .map_err(|error| store_error("delete a cron watermark", &error))?;
        Ok(result.deleted_count == 1)
    }
}
