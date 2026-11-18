//! Memory expiry: the one rule every backend applies.
//!
//! A [`MemoryEntry`] expires at its `expires_at`. An entry written without one takes the
//! service's default TTL, measured from the write; a service without a default keeps it until
//! it is deleted.
//!
//! | Step | Rule | Where |
//! |------|------|-------|
//! | Write | `expires_at` is the entry's own, else `now + default TTL`, else `None`, to the millisecond | [`resolve_expiry`] |
//! | Read | An entry is expired once `expires_at <= now`; search and recency listings never return one | [`is_expired`] |
//! | Purge | [`MemoryService::purge_expired`] removes expired entries; [`spawn_expiry_sweep`] runs it on an interval | each backend |
//!
//! Backends that filter in their store (SQL, Cypher, MongoDB) state the same comparison as
//! [`is_expired`], and the `conformance` suite holds every backend to it.
//!
//! | Backend | Default TTL | Native expiry |
//! |---------|-------------|---------------|
//! | `InMemoryMemoryService` | `with_default_ttl` | none; `purge_expired` sweeps |
//! | `SqliteMemoryService` | `with_default_ttl` | none; `purge_expired` sweeps |
//! | `PostgresMemoryService` | `with_default_ttl` | none; `purge_expired` sweeps |
//! | `RedisMemoryService` | `RedisMemoryConfig::ttl` | each list key expires with its latest entry |
//! | `MongoMemoryService` | `with_default_ttl` | TTL index on `expires_at` |
//! | `Neo4jMemoryService` | `with_default_ttl` | none; `purge_expired` sweeps |
//! | `VertexAiMemoryBankService` | `with_default_ttl` | Memory Bank deletes a memory at its `expireTime` |
//! | `GraphMemoryService` | `with_default_ttl` (the episodic log) | none; `purge_expired` sweeps |
//!
//! # Example
//!
//! ```rust
//! use adk_core::Content;
//! use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
//! use std::time::Duration;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_core::Result<()> {
//! let service = InMemoryMemoryService::new().with_default_ttl(Duration::from_secs(30 * 24 * 3600));
//! let stale = MemoryEntry::new(Content::new("user").with_text("Parking: level 3."), "user")
//!     .with_expires_at(chrono::Utc::now() - chrono::Duration::minutes(1));
//! let fresh = MemoryEntry::new(Content::new("user").with_text("Parking: level 5."), "user");
//! service.add_session("app", "ada", "s1", vec![stale, fresh]).await?;
//!
//! let found = service
//!     .search(SearchRequest {
//!         query: "parking".into(),
//!         user_id: "ada".into(),
//!         app_name: "app".into(),
//!         limit: None,
//!         min_score: None,
//!         project_id: None,
//!     })
//!     .await?;
//! assert_eq!(found.memories.len(), 1);
//! assert!(found.memories[0].expires_at.is_some(), "the default TTL applied");
//! assert_eq!(service.purge_expired().await?, 1);
//! # Ok(())
//! # }
//! ```

use crate::service::{MemoryEntry, MemoryService};
use chrono::{DateTime, SubsecRound, Utc};
use std::sync::Arc;
use std::time::Duration;

/// Whether a memory expiring at `expires_at` has expired at `now`: once `expires_at <= now`.
///
/// # Example
///
/// ```rust
/// use adk_memory::expiry::is_expired;
///
/// let now = chrono::Utc::now();
/// assert!(is_expired(Some(now), now));
/// assert!(!is_expired(Some(now + chrono::Duration::seconds(1)), now));
/// assert!(!is_expired(None, now));
/// ```
pub fn is_expired(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    expires_at.is_some_and(|at| at <= now)
}

/// The expiry `ttl` after `from`, to the millisecond; the far future when it overflows.
///
/// # Example
///
/// ```rust
/// use adk_memory::expiry::expiry_after;
/// use std::time::Duration;
///
/// let now = chrono::Utc::now();
/// assert!(expiry_after(now, Duration::from_secs(60)) > now);
/// ```
pub fn expiry_after(from: DateTime<Utc>, ttl: Duration) -> DateTime<Utc> {
    let at = chrono::Duration::from_std(ttl)
        .ok()
        .and_then(|ttl| from.checked_add_signed(ttl))
        .unwrap_or(DateTime::<Utc>::MAX_UTC);
    to_millis(at)
}

/// The expiry an entry is stored with: its own, else `now + default_ttl`, else none.
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_memory::MemoryEntry;
/// use adk_memory::expiry::resolve_expiry;
/// use std::time::Duration;
///
/// let now = chrono::Utc::now();
/// let entry = MemoryEntry::new(Content::new("user").with_text("hi"), "user");
/// assert_eq!(resolve_expiry(&entry, None, now), None);
/// assert!(resolve_expiry(&entry, Some(Duration::from_secs(60)), now).is_some());
/// ```
pub fn resolve_expiry(
    entry: &MemoryEntry,
    default_ttl: Option<Duration>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    entry.expires_at.map(to_millis).or_else(|| default_ttl.map(|ttl| expiry_after(now, ttl)))
}

/// Truncates a time to whole milliseconds, the finest precision every backend stores.
pub(crate) fn to_millis(at: DateTime<Utc>) -> DateTime<Utc> {
    at.trunc_subsecs(3)
}

/// The entry as a backend stores it: an id assigned when it has none, and its resolved expiry.
pub(crate) fn stamp(
    mut entry: MemoryEntry,
    default_ttl: Option<Duration>,
    now: DateTime<Utc>,
) -> MemoryEntry {
    entry.expires_at = resolve_expiry(&entry, default_ttl, now);
    if entry.id.is_none() {
        entry.id = Some(new_memory_id());
    }
    entry
}

/// A fresh memory id.
pub(crate) fn new_memory_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Runs [`MemoryService::purge_expired`] on `service` every `every`, until the task is aborted.
///
/// A failed purge is logged and retried on the next tick. Abort the returned handle to stop
/// the sweep.
///
/// # Example
///
/// ```rust
/// use adk_memory::InMemoryMemoryService;
/// use adk_memory::expiry::spawn_expiry_sweep;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let sweep = spawn_expiry_sweep(Arc::new(InMemoryMemoryService::new()), Duration::from_secs(3600));
/// sweep.abort();
/// # }
/// ```
pub fn spawn_expiry_sweep(
    service: Arc<dyn MemoryService>,
    every: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match service.purge_expired().await {
                Ok(0) => {}
                Ok(removed) => tracing::debug!(memory.purged = removed, "purged expired memories"),
                Err(error) => tracing::warn!(error = %error, "memory expiry sweep failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Content;

    #[test]
    fn an_entry_expires_exactly_at_its_expiry() {
        let now = to_millis(Utc::now());
        let millisecond = chrono::Duration::milliseconds(1);
        assert!(is_expired(Some(now - millisecond), now));
        assert!(is_expired(Some(now), now));
        assert!(!is_expired(Some(now + millisecond), now));
        assert!(!is_expired(None, now));
    }

    #[test]
    fn an_entry_expiry_overrides_the_default() {
        let now = to_millis(Utc::now());
        let own = now + chrono::Duration::days(2);
        let entry =
            MemoryEntry::new(Content::new("user").with_text("x"), "user").with_expires_at(own);
        assert_eq!(resolve_expiry(&entry, Some(Duration::from_secs(60)), now), Some(own));
        let plain = MemoryEntry::new(Content::new("user").with_text("x"), "user");
        assert_eq!(
            resolve_expiry(&plain, Some(Duration::from_secs(60)), now),
            Some(now + chrono::Duration::seconds(60))
        );
        assert_eq!(resolve_expiry(&plain, None, now), None);
    }

    #[test]
    fn stamping_keeps_a_set_id_and_assigns_a_missing_one() {
        let now = Utc::now();
        let kept = stamp(MemoryEntry::new(Content::new("user"), "user").with_id("m1"), None, now);
        assert_eq!(kept.id.as_deref(), Some("m1"));
        let assigned = stamp(MemoryEntry::new(Content::new("user"), "user"), None, now);
        assert!(assigned.id.is_some_and(|id| !id.is_empty()));
    }

    #[test]
    fn an_overflowing_ttl_never_expires_in_practice() {
        let at = expiry_after(Utc::now(), Duration::MAX);
        assert!(at > Utc::now() + chrono::Duration::days(365 * 1000));
    }
}
