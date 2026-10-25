//! The Firestore [`CronWatermarkStore`] (spec 3.2 §7.4, §13 Q8, feature `firestore`).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firestore::FirestoreDb;
use serde::{Deserialize, Serialize};

use super::firestore::{
    TxFuture, connect, delete_where, doc_id, store_error, transaction_error, transaction_options,
    tx_fail,
};
use crate::error::Result;
use crate::watermark::CronWatermarkStore;

const WATERMARKS: &str = "adk_kernel_cron_watermarks";

/// One source's cursor, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WatermarkDoc {
    source: String,
    cursor_us: i64,
}

/// A [`CronWatermarkStore`] in Firestore, shared by every replica (spec 3.2 §7.4, §13 Q8,
/// feature `firestore`). **Alpha** until its live conformance suite passes in CI.
///
/// | Collection (document ID) | Contents |
/// |--------------------------|----------|
/// | `adk_kernel_cron_watermarks` (hex of the source's name) | `source` and `cursor_us`, the cursor in microseconds since the Unix epoch |
///
/// An advance reads the cursor and writes the later one in one serializable transaction,
/// so of two replicas advancing one source at once the second retries, sees the first's
/// write and never moves the cursor back. A source name longer than 700 bytes is refused.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::FirestoreCronWatermarkStore::connect("my-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreCronWatermarkStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreCronWatermarkStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreCronWatermarkStore").finish_non_exhaustive()
    }
}

impl FirestoreCronWatermarkStore {
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
}

/// The instant `micros` microseconds after the Unix epoch.
fn instant(micros: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros).ok_or_else(|| {
        store_error("read a cron watermark", &format!("the cursor {micros} is out of range"))
    })
}

#[async_trait]
impl CronWatermarkStore for FirestoreCronWatermarkStore {
    async fn read(&self, source: &str) -> Result<Option<DateTime<Utc>>> {
        let Ok(doc_name) = doc_id("cron source", source) else {
            return Ok(None);
        };
        let doc: Option<WatermarkDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(WATERMARKS)
            .obj()
            .one(doc_name)
            .await
            .map_err(|error| store_error("read a cron watermark", &error))?;
        doc.filter(|doc| doc.source == source).map(|doc| instant(doc.cursor_us)).transpose()
    }

    async fn advance(&self, source: &str, cursor: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let doc_name = Arc::new(doc_id("cron source", source)?);
        let doc = Arc::new(WatermarkDoc {
            source: source.to_string(),
            cursor_us: cursor.timestamp_micros(),
        });
        let stored = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, i64> {
                    let (doc_name, doc) = (doc_name.clone(), doc.clone());
                    Box::pin(async move {
                        let stored: Option<WatermarkDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(WATERMARKS)
                            .obj()
                            .one(doc_name.as_str())
                            .await
                            .map_err(tx_fail("read a cron watermark"))?;
                        // A document holding another name reads as no cursor, as in `read`.
                        if let Some(stored) = stored.filter(|stored| {
                            stored.source == doc.source && stored.cursor_us >= doc.cursor_us
                        }) {
                            return Ok(stored.cursor_us);
                        }
                        db.fluent()
                            .update()
                            .in_col(WATERMARKS)
                            .document_id(doc_name.as_str())
                            .object(doc.as_ref())
                            .add_to_transaction(transaction)
                            .map_err(tx_fail("advance a cron watermark"))?;
                        Ok(doc.cursor_us)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|error| transaction_error(error, "advance a cron watermark"))?;
        instant(stored)
    }

    async fn delete(&self, source: &str) -> Result<bool> {
        let Ok(doc_name) = doc_id("cron source", source) else {
            return Ok(false);
        };
        let source = source.to_string();
        let matches = move |doc: &WatermarkDoc| doc.source == source;
        delete_where(&self.db, WATERMARKS, doc_name, matches, "delete a cron watermark").await
    }
}
