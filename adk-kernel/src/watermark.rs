//! Cron watermarks: where each cron wake source left off, so the ticks it missed while no
//! kernel watched replay once, per the schedule's catch-up policy (spec 3.2 §7.4, §7.13).
//!
//! | Item | What it is |
//! |---|---|
//! | [`CronWatermarkStore`] | The cursor of each cron source by name, which only moves forward |
//! | [`SourceWatermark`] | One source's cursor as an `adk_agent::ambient::TickWatermark` |
//!
//! The store runs on in-memory, SQLite, PostgreSQL and Redis ([`InMemoryCronWatermarkStore`],
//! `SqliteCronWatermarkStore`, `PostgresCronWatermarkStore`, `RedisCronWatermarkStore`), and,
//! Alpha, on MongoDB, Firestore and Neo4j (`MongoCronWatermarkStore`,
//! `FirestoreCronWatermarkStore`, `Neo4jCronWatermarkStore`), with the conformance suite
//! `cron_watermark_store_conformance_tests!` (feature `conformance`).
//! A tenant kernel (`managed::TenantKernel`, feature `managed`) keeps its watermarks in the
//! tenant's own store, so a tick missed while no replica of the tenant ran is seen by the next
//! replica that starts.
//!
//! [`InMemoryCronWatermarkStore`]: crate::stores::InMemoryCronWatermarkStore

use std::sync::Arc;

use adk_agent::ambient::TickWatermark;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;

/// Where each cron wake source left off (spec 3.2 §7.4, §7.13): the instant through which
/// its ticks are accounted for, by source name.
///
/// A cursor only moves forward: [`advance`](Self::advance) keeps the later of the stored
/// cursor and the one given, so replicas that watch one schedule and write in any order
/// never move it back, and a tick already accounted for is never planned again.
///
/// # Example
///
/// ```rust
/// use adk_kernel::CronWatermarkStore;
/// use adk_kernel::stores::InMemoryCronWatermarkStore;
/// use chrono::{TimeZone, Utc};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryCronWatermarkStore::new();
/// let nine = Utc.with_ymd_and_hms(2026, 10, 24, 9, 0, 0).unwrap();
/// let eight = Utc.with_ymd_and_hms(2026, 10, 24, 8, 0, 0).unwrap();
/// assert_eq!(store.advance("nightly", nine).await?, nine);
/// // An earlier cursor, from a replica behind this one, leaves it where it is.
/// assert_eq!(store.advance("nightly", eight).await?, nine);
/// assert_eq!(store.read("nightly").await?, Some(nine));
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait CronWatermarkStore: Send + Sync {
    /// The cursor of `source`, or `None` when it never ran.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn read(&self, source: &str) -> Result<Option<DateTime<Utc>>>;

    /// Moves the cursor of `source` to `cursor` unless it is already later, and returns the
    /// cursor as stored. Cursors keep microseconds.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn advance(&self, source: &str, cursor: DateTime<Utc>) -> Result<DateTime<Utc>>;

    /// Deletes the cursor of `source`; `false` when there was none.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn delete(&self, source: &str) -> Result<bool>;
}

/// The cursor of one cron source in a [`CronWatermarkStore`], as the
/// `adk_agent::ambient::TickWatermark` a [`CronWakeSource`](crate::CronWakeSource) or a
/// `CronTrigger` keeps (spec 3.2 §7.4).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_agent::ambient::TickWatermark;
/// use adk_kernel::SourceWatermark;
/// use adk_kernel::stores::InMemoryCronWatermarkStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let watermark = SourceWatermark::new(Arc::new(InMemoryCronWatermarkStore::new()), "nightly");
/// assert_eq!(watermark.read().await?, None);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct SourceWatermark {
    store: Arc<dyn CronWatermarkStore>,
    source: String,
}

impl std::fmt::Debug for SourceWatermark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceWatermark").field("source", &self.source).finish_non_exhaustive()
    }
}

impl SourceWatermark {
    /// The cursor of `source` in `store`.
    pub fn new(store: Arc<dyn CronWatermarkStore>, source: impl Into<String>) -> Self {
        Self { store, source: source.into() }
    }

    /// The source whose cursor this is.
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[async_trait]
impl TickWatermark for SourceWatermark {
    async fn read(&self) -> adk_core::Result<Option<DateTime<Utc>>> {
        Ok(self.store.read(&self.source).await?)
    }

    async fn write(&self, cursor: DateTime<Utc>) -> adk_core::Result<()> {
        self.store.advance(&self.source, cursor).await?;
        Ok(())
    }
}
