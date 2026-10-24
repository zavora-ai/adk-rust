//! The in-memory [`CronWatermarkStore`] (spec 3.2 §7.4, §13 Q8).

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::error::Result;
use crate::watermark::CronWatermarkStore;

/// A [`CronWatermarkStore`] in process memory, for tests and a kernel with no durable store
/// (spec 3.2 §7.4). Its cursors end with the process, so a restarted kernel over it sees no
/// tick it missed.
///
/// # Example
///
/// ```rust
/// use adk_kernel::CronWatermarkStore;
/// use adk_kernel::stores::InMemoryCronWatermarkStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryCronWatermarkStore::new();
/// let now = chrono::Utc::now();
/// store.advance("nightly", now).await?;
/// assert!(store.read("nightly").await?.is_some());
/// assert!(store.delete("nightly").await?);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryCronWatermarkStore {
    cursors: RwLock<BTreeMap<String, DateTime<Utc>>>,
}

impl InMemoryCronWatermarkStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// `cursor` truncated to the microseconds every backend keeps.
pub(crate) fn micros(cursor: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(cursor.timestamp_micros()).unwrap_or(cursor)
}

#[async_trait]
impl CronWatermarkStore for InMemoryCronWatermarkStore {
    async fn read(&self, source: &str) -> Result<Option<DateTime<Utc>>> {
        Ok(self.cursors.read().await.get(source).copied())
    }

    async fn advance(&self, source: &str, cursor: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let cursor = micros(cursor);
        let mut cursors = self.cursors.write().await;
        let stored = cursors.entry(source.to_string()).or_insert(cursor);
        if *stored < cursor {
            *stored = cursor;
        }
        Ok(*stored)
    }

    async fn delete(&self, source: &str) -> Result<bool> {
        Ok(self.cursors.write().await.remove(source).is_some())
    }
}
