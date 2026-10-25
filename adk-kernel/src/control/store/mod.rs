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

/// What an area started with when its settings were last changed (spec 3.3 §10 K3): the
/// values the changes replace.
///
/// A change holds while the area still starts with the value it replaced; a different start
/// value — a later `adk-rust up --mode`, `adk-rust partitions set-mode`, or quiet hours and
/// a cap set at start — is newer, and takes over from it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::AreaStart;
/// use adk_kernel::PartitionMode;
///
/// let start = AreaStart::new(Vec::new(), Some(5_000_000), Some(PartitionMode::Ask));
/// assert_eq!(serde_json::to_value(&start).unwrap()["mode"], "ask");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AreaStart {
    /// The quiet windows the kernel and the partition record started with.
    #[serde(default)]
    pub quiet_time: Vec<QuietHours>,
    /// The daily spend cap it started with, in micro-USD; `None` is no cap.
    #[serde(default)]
    pub daily_spending_limit: Option<u64>,
    /// The mode its agents started in; `None` when the kernel was not told.
    #[serde(default)]
    pub mode: Option<PartitionMode>,
}

impl AreaStart {
    /// An area that started with `quiet_time`, `daily_spending_limit` and `mode`.
    pub fn new(
        quiet_time: Vec<QuietHours>,
        daily_spending_limit: Option<u64>,
        mode: Option<PartitionMode>,
    ) -> Self {
        Self { quiet_time, daily_spending_limit, mode }
    }
}

/// The settings a person changed for one area while the kernel ran (spec 3.3 §8.1 "Areas",
/// §10 K3). Each one set replaces, for the area, the value the kernel and the area's
/// partition record started with, while the area still starts with that value
/// ([`start`](Self::start)); one never set leaves the start value in force.
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
    /// What the area started with when a setting was last changed; `None` on a record
    /// written without it, whose changes hold whatever the area starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<AreaStart>,
    /// The quiet times earlier changes set, oldest first, at most
    /// [`QUIET_TIME_HISTORY`](Self::QUIET_TIME_HISTORY): past work is judged against the
    /// quiet time in force when it started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_quiet_time: Vec<Change<Option<QuietHours>>>,
}

impl AreaSettings {
    /// How many earlier quiet times a record keeps.
    pub const QUIET_TIME_HISTORY: usize = 32;

    /// No changed setting for `partition`.
    pub fn new(partition: impl Into<PartitionId>) -> Self {
        Self {
            partition: partition.into(),
            quiet_time: None,
            daily_spending_limit: None,
            mode: None,
            start: None,
            earlier_quiet_time: Vec::new(),
        }
    }

    /// Records that the area started with `start`.
    #[must_use]
    pub fn with_start(mut self, start: AreaStart) -> Self {
        self.start = Some(start);
        self
    }

    /// The changed quiet time, when the area still starts with the windows it replaced.
    pub fn quiet_time_over(&self, start: &[QuietHours]) -> Option<&Change<Option<QuietHours>>> {
        self.quiet_time
            .as_ref()
            .filter(|_| self.start.as_ref().is_none_or(|was| was.quiet_time == start))
    }

    /// The changed daily spending limit, when the area still starts with the cap it
    /// replaced.
    pub fn daily_spending_limit_over(&self, start: Option<u64>) -> Option<&Change<Option<u64>>> {
        self.daily_spending_limit
            .as_ref()
            .filter(|_| self.start.as_ref().is_none_or(|was| was.daily_spending_limit == start))
    }

    /// The changed mode, when the area's agents still start in the mode it replaced.
    pub fn mode_over(&self, start: Option<PartitionMode>) -> Option<&Change<PartitionMode>> {
        self.mode.as_ref().filter(|_| self.start.as_ref().is_none_or(|was| was.mode == start))
    }

    /// Drops every change whose start value `start` has replaced since, then records
    /// `start` (spec 3.3 §10 K3): call it before changing a setting.
    pub fn settle(&mut self, start: &AreaStart) {
        if self.quiet_time_over(&start.quiet_time).is_none() {
            self.quiet_time = None;
        }
        if self.daily_spending_limit_over(start.daily_spending_limit).is_none() {
            self.daily_spending_limit = None;
        }
        if self.mode_over(start.mode).is_none() {
            self.mode = None;
        }
        self.start = Some(start.clone());
    }

    /// Sets the changed quiet time, keeping the one it replaces among the earlier ones.
    pub fn change_quiet_time(&mut self, change: Change<Option<QuietHours>>) {
        if let Some(earlier) = self.quiet_time.replace(change) {
            self.earlier_quiet_time.push(earlier);
            let excess = self.earlier_quiet_time.len().saturating_sub(Self::QUIET_TIME_HISTORY);
            self.earlier_quiet_time.drain(..excess);
        }
    }

    /// The person's quiet time in force at `at`, as the area's changes record it: the
    /// newest change made at or before `at`, to the millisecond the ledger stamps its rows
    /// with, or `None` before the first one, when the start windows held.
    pub fn quiet_time_at(&self, at: DateTime<Utc>) -> Option<&Change<Option<QuietHours>>> {
        let at = at.timestamp_millis();
        self.earlier_quiet_time
            .iter()
            .chain(self.quiet_time.as_ref())
            .filter(|change| change.at.timestamp_millis() <= at)
            .max_by_key(|change| change.at)
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
/// | `update_area_settings` | Changes the area's record with no other write in between, so concurrent changes all hold |
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

    /// Applies `change` to the record of `partition`, an empty one when it has none, and
    /// stores it with no other write in between: when another write changed the record
    /// meanwhile, `change` is applied again to that one, so concurrent changes of different
    /// settings all hold (spec 3.3 §10 K3). Returns the record stored.
    ///
    /// The default reads, applies and puts, which keeps that promise only for a store with
    /// one writer; every store of this crate overrides it.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn update_area_settings(
        &self,
        partition: &PartitionId,
        change: &(dyn for<'a> Fn(&'a mut AreaSettings) + Send + Sync),
    ) -> Result<AreaSettings> {
        let mut settings = self
            .area_settings(partition)
            .await?
            .unwrap_or_else(|| AreaSettings::new(partition.clone()));
        change(&mut settings);
        self.put_area_settings(settings.clone()).await?;
        Ok(settings)
    }

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

/// A [`ControlStore`] that answers a read of an area's settings the backend fails with the
/// settings it last read (spec 3.3 §10 K3): admission keeps the person's quiet time and
/// limit through a store outage rather than falling back to the start settings.
pub(crate) struct LastRead {
    inner: std::sync::Arc<dyn ControlStore>,
    read: std::sync::Mutex<BTreeMap<PartitionId, Option<AreaSettings>>>,
}

impl LastRead {
    /// Remembers what `inner` answers.
    pub(crate) fn new(inner: std::sync::Arc<dyn ControlStore>) -> Self {
        Self { inner, read: std::sync::Mutex::default() }
    }

    fn remembered(&self) -> std::sync::MutexGuard<'_, BTreeMap<PartitionId, Option<AreaSettings>>> {
        self.read.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl ControlStore for LastRead {
    async fn area_settings(&self, partition: &PartitionId) -> Result<Option<AreaSettings>> {
        match self.inner.area_settings(partition).await {
            Ok(settings) => {
                self.remembered().insert(partition.clone(), settings.clone());
                Ok(settings)
            }
            Err(error) => match self.remembered().get(partition) {
                Some(settings) => {
                    tracing::warn!(partition = %partition, error = %error, "could not read the changed settings; the ones read last apply");
                    Ok(settings.clone())
                }
                None => Err(error),
            },
        }
    }

    async fn list_area_settings(&self) -> Result<Vec<AreaSettings>> {
        self.inner.list_area_settings().await
    }

    async fn put_area_settings(&self, settings: AreaSettings) -> Result<()> {
        self.inner.put_area_settings(settings).await
    }

    async fn update_area_settings(
        &self,
        partition: &PartitionId,
        change: &(dyn for<'a> Fn(&'a mut AreaSettings) + Send + Sync),
    ) -> Result<AreaSettings> {
        self.inner.update_area_settings(partition, change).await
    }

    async fn cursor(&self, user: &str, viewer: &str) -> Result<Option<AwayCursor>> {
        self.inner.cursor(user, viewer).await
    }

    async fn advance_cursor(&self, cursor: AwayCursor) -> Result<AwayCursor> {
        self.inner.advance_cursor(cursor).await
    }
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

    async fn update_area_settings(
        &self,
        partition: &PartitionId,
        change: &(dyn for<'a> Fn(&'a mut AreaSettings) + Send + Sync),
    ) -> Result<AreaSettings> {
        let mut all = self.settings.write().await;
        let settings =
            all.entry(partition.clone()).or_insert_with(|| AreaSettings::new(partition.clone()));
        change(settings);
        Ok(settings.clone())
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A store whose reads of area settings fail while `down` is set.
    struct Flaky {
        inner: InMemoryControlStore,
        down: AtomicBool,
    }

    #[async_trait]
    impl ControlStore for Flaky {
        async fn area_settings(&self, partition: &PartitionId) -> Result<Option<AreaSettings>> {
            if self.down.load(Ordering::SeqCst) {
                return Err(crate::KernelError::Store {
                    backend: "test",
                    reason: "the pool timed out".into(),
                });
            }
            self.inner.area_settings(partition).await
        }
        async fn list_area_settings(&self) -> Result<Vec<AreaSettings>> {
            self.inner.list_area_settings().await
        }
        async fn put_area_settings(&self, settings: AreaSettings) -> Result<()> {
            self.inner.put_area_settings(settings).await
        }
        async fn cursor(&self, user: &str, viewer: &str) -> Result<Option<AwayCursor>> {
            self.inner.cursor(user, viewer).await
        }
        async fn advance_cursor(&self, cursor: AwayCursor) -> Result<AwayCursor> {
            self.inner.advance_cursor(cursor).await
        }
    }

    /// Through a store outage, admission keeps the quiet time and limit the person set: a
    /// read that fails answers what was read last, and one that never succeeded fails (spec
    /// 3.3 §10 K3).
    #[tokio::test]
    async fn a_failed_read_answers_the_settings_read_last() {
        let flaky = Arc::new(Flaky { inner: InMemoryControlStore::new(), down: false.into() });
        let limit = Change::new(Some(1_000_000), "ana", Utc::now());
        let lowered = AreaSettings::new("default").with_daily_spending_limit(limit);
        flaky.put_area_settings(lowered.clone()).await.unwrap();
        let store = LastRead::new(flaky.clone());
        let default = PartitionId::default();
        assert_eq!(store.area_settings(&default).await.unwrap(), Some(lowered.clone()));

        flaky.down.store(true, Ordering::SeqCst);
        assert_eq!(store.area_settings(&default).await.unwrap(), Some(lowered));
        let never_read = PartitionId::new("work");
        assert!(store.area_settings(&never_read).await.is_err(), "nothing read before");
    }

    /// A quiet time is in force from the millisecond it was set, the precision the ledger
    /// stamps its rows with, until the next one.
    #[test]
    fn the_quiet_time_in_force_at_an_instant() {
        let at = |millis: i64| DateTime::from_timestamp_millis(millis).unwrap();
        let window = "22:00-07:00".parse::<QuietHours>().unwrap();
        let mut settings = AreaSettings::new("default");
        let mut set = Change::new(Some(window), "ana", at(1_000));
        set.at += chrono::Duration::microseconds(400);
        settings.change_quiet_time(set);
        settings.change_quiet_time(Change::new(None, "ana", at(5_000)));
        assert_eq!(settings.quiet_time_at(at(999)), None, "the start windows held");
        assert_eq!(settings.quiet_time_at(at(1_000)).unwrap().value, Some(window));
        assert_eq!(settings.quiet_time_at(at(4_999)).unwrap().value, Some(window));
        assert_eq!(settings.quiet_time_at(at(5_000)).unwrap().value, None);
    }
}
