//! What the control plane keeps between restarts (spec 3.3 §10 K3, K4): the settings a
//! person changed while the kernel ran, by area, and how far each device has read "While
//! you were away".
//!
//! | Record | Key | Written by |
//! |---|---|---|
//! | [`AreaSettings`] | partition | `POST /kernel/settings/{partition}/{quiet-time,spending-limit,mode}` |
//! | [`AwayCursor`] | user and viewer | `POST /kernel/away/seen` |
//!
//! [`ControlStore`] runs on the backends the device store runs on, with one conformance
//! suite (`control_store_conformance_tests!`, feature `conformance`):
//!
//! | Backend | Type | Feature |
//! |---|---|---|
//! | In-memory | [`InMemoryControlStore`] | — |
//! | SQLite | `SqliteControlStore` | `sqlite` (default) |
//! | PostgreSQL | `PostgresControlStore` | `postgres` |
//! | Redis | `RedisControlStore` | `redis` |

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::Result;
use crate::partition::{PartitionId, PartitionMode};
use crate::sched::QuietHours;

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "redis")]
mod redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;
#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(feature = "postgres")]
pub use postgres::PostgresControlStore;
#[cfg(feature = "redis")]
pub use redis::RedisControlStore;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteControlStore;

/// One setting a person changed while the kernel ran (spec 3.3 §10 K3): the value, and when
/// and by whom it was set.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::Change;
///
/// let change = Change::new(Some(5_000_000_u64), "ana", chrono::Utc::now());
/// assert_eq!(serde_json::to_value(&change).unwrap()["value"], 5_000_000);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Change<T> {
    /// The value set; for quiet time and the spending limit, `None` turns it off.
    pub value: T,
    /// When it was set.
    pub at: DateTime<Utc>,
    /// Who set it: the subject of the control-plane caller.
    pub by: String,
}

impl<T> Change<T> {
    /// `value`, set by `by` at `at`.
    pub fn new(value: T, by: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self { value, at, by: by.into() }
    }
}

/// The settings a person changed for one area while the kernel ran (spec 3.3 §8.1 "Areas",
/// §10 K3). Each one set replaces, for the area, the value the kernel and the area's
/// partition record started with; one never set leaves the start value in force.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{AreaSettings, Change};
/// use adk_kernel::{PartitionMode, QuietHours};
///
/// let now = chrono::Utc::now();
/// let settings = AreaSettings::new("work")
///     .with_quiet_time(Change::new(Some("22:00-07:00".parse::<QuietHours>().unwrap()), "ana", now))
///     .with_mode(Change::new(PartitionMode::Ask, "ana", now));
/// let wire = serde_json::to_value(&settings).unwrap();
/// assert_eq!(wire["quietTime"]["value"]["start"], "22:00");
/// assert_eq!(wire["mode"]["value"], "ask");
/// assert!(wire.get("dailySpendingLimit").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AreaSettings {
    /// The area: a partition, `default` on a kernel without partitions.
    pub partition: PartitionId,
    /// Quiet time; `value: None` is no quiet time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_time: Option<Change<Option<QuietHours>>>,
    /// The daily spending limit in micro-USD; `value: None` is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_spending_limit: Option<Change<Option<u64>>>,
    /// How much the area's agents act alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Change<PartitionMode>>,
}

impl AreaSettings {
    /// No changed setting for `partition`.
    pub fn new(partition: impl Into<PartitionId>) -> Self {
        Self {
            partition: partition.into(),
            quiet_time: None,
            daily_spending_limit: None,
            mode: None,
        }
    }

    /// Sets the changed quiet time.
    #[must_use]
    pub fn with_quiet_time(mut self, change: Change<Option<QuietHours>>) -> Self {
        self.quiet_time = Some(change);
        self
    }

    /// Sets the changed daily spending limit.
    #[must_use]
    pub fn with_daily_spending_limit(mut self, change: Change<Option<u64>>) -> Self {
        self.daily_spending_limit = Some(change);
        self
    }

    /// Sets the changed mode.
    #[must_use]
    pub fn with_mode(mut self, change: Change<PartitionMode>) -> Self {
        self.mode = Some(change);
        self
    }
}

/// How far one viewer of one person has read "While you were away" (spec 3.3 §10 K4): the
/// last `/proc` ledger row it marked seen.
///
/// The viewer is `device:<id>` for a paired device, else the name a surface gives itself
/// (`home`, `terminal`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::AwayCursor;
///
/// let cursor = AwayCursor::new("ana", "device:0123456789abcdef", 41, chrono::Utc::now());
/// assert_eq!(serde_json::to_value(&cursor).unwrap()["seq"], 41);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AwayCursor {
    /// The person.
    pub user: String,
    /// The viewer.
    pub viewer: String,
    /// The last ledger row seen; rows after it are new to the viewer.
    pub seq: u64,
    /// When the viewer marked it seen.
    pub seen_at: DateTime<Utc>,
}

impl AwayCursor {
    /// `viewer` of `user` has seen every row up to `seq`, at `seen_at`.
    pub fn new(
        user: impl Into<String>,
        viewer: impl Into<String>,
        seq: u64,
        seen_at: DateTime<Utc>,
    ) -> Self {
        Self { user: user.into(), viewer: viewer.into(), seq, seen_at }
    }
}

/// Where the control plane keeps [`AreaSettings`] and [`AwayCursor`]s (spec 3.3 §10 K3,
/// K4).
///
/// | Method | Contract |
/// |---|---|
/// | `area_settings`, `list_area_settings` | One area's record, or every one ordered by partition id |
/// | `put_area_settings` | Replaces the area's record |
/// | `cursor` | One viewer's cursor |
/// | `advance_cursor` | Stores the cursor unless the stored one is past its `seq`, and returns the stored one: a cursor never moves back |
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{AwayCursor, ControlStore, InMemoryControlStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryControlStore::new();
/// let now = chrono::Utc::now();
/// store.advance_cursor(AwayCursor::new("ana", "home", 9, now)).await?;
/// let stored = store.advance_cursor(AwayCursor::new("ana", "home", 4, now)).await?;
/// assert_eq!(stored.seq, 9, "a cursor never moves back");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait ControlStore: Send + Sync {
    /// The settings changed for `partition`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn area_settings(&self, partition: &PartitionId) -> Result<Option<AreaSettings>>;

    /// Every area's changed settings, ordered by partition id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list_area_settings(&self) -> Result<Vec<AreaSettings>>;

    /// Replaces the changed settings of `settings.partition`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put_area_settings(&self, settings: AreaSettings) -> Result<()>;

    /// The cursor of `viewer` of `user`, or `None` before it first marks anything seen.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn cursor(&self, user: &str, viewer: &str) -> Result<Option<AwayCursor>>;

    /// Stores `cursor` when the stored cursor of its viewer is at or before its `seq`, and
    /// returns the cursor stored after the call.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn advance_cursor(&self, cursor: AwayCursor) -> Result<AwayCursor>;
}

/// A [`ControlStore`] in process memory (spec 3.3 §10 K3, K4): changed settings and cursors
/// end with the process, so a restart brings back the start settings.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{AreaSettings, ControlStore, InMemoryControlStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryControlStore::new();
/// store.put_area_settings(AreaSettings::new("work")).await?;
/// assert_eq!(store.list_area_settings().await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryControlStore {
    settings: RwLock<BTreeMap<PartitionId, AreaSettings>>,
    cursors: RwLock<BTreeMap<(String, String), AwayCursor>>,
}

impl InMemoryControlStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ControlStore for InMemoryControlStore {
    async fn area_settings(&self, partition: &PartitionId) -> Result<Option<AreaSettings>> {
        Ok(self.settings.read().await.get(partition).cloned())
    }

    async fn list_area_settings(&self) -> Result<Vec<AreaSettings>> {
        Ok(self.settings.read().await.values().cloned().collect())
    }

    async fn put_area_settings(&self, settings: AreaSettings) -> Result<()> {
        self.settings.write().await.insert(settings.partition.clone(), settings);
        Ok(())
    }

    async fn cursor(&self, user: &str, viewer: &str) -> Result<Option<AwayCursor>> {
        Ok(self.cursors.read().await.get(&(user.to_string(), viewer.to_string())).cloned())
    }

    async fn advance_cursor(&self, cursor: AwayCursor) -> Result<AwayCursor> {
        let mut cursors = self.cursors.write().await;
        let key = (cursor.user.clone(), cursor.viewer.clone());
        match cursors.get(&key) {
            Some(stored) if stored.seq > cursor.seq => Ok(stored.clone()),
            _ => {
                cursors.insert(key, cursor.clone());
                Ok(cursor)
            }
        }
    }
}
