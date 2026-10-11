//! Leases with fencing epochs: one owner of a named resource at a time.
//!
//! A [`LeaseStore`] grants a time-limited lease on a resource name (a scheduler
//! leadership slot, a session, a queue shard). Each grant carries an `epoch` that is one
//! higher than the resource's previous epoch, so a holder that stalls past its expiry
//! and wakes up after another owner took over holds a lower epoch and is refused —
//! the fencing rule. Writes made on behalf of a lease carry its epoch, and a store
//! refuses a stale one.
//!
//! | Operation | Free or expired resource | Held by the caller | Held by another owner |
//! |-----------|--------------------------|--------------------|-----------------------|
//! | [`acquire`](LeaseStore::acquire) | Granted, epoch + 1 | Granted again, epoch + 1 | `None` |
//! | [`renew`](LeaseStore::renew) | Extended while the epoch is current and not released | Extended | `lease.stale_epoch` |
//! | [`release`](LeaseStore::release) | No-op when stale | Released | No-op |
//!
//! A lease that expired but was not taken by anyone keeps its epoch, so its holder may
//! still renew it. A released lease cannot be renewed. Epochs never repeat: a store
//! keeps one record per resource after release, so the next grant continues the count.
//!
//! # Backend primitives
//!
//! | Backend | Primitive for a single winner |
//! |---------|-------------------------------|
//! | SQL | `INSERT ... ON CONFLICT DO UPDATE ... WHERE expires_at <= now OR owner = $owner RETURNING` (one row per resource) |
//! | Redis | One Lua script reading and writing the resource hash |
//! | MongoDB | `findOneAndUpdate` with an expiry filter and `upsert` |
//! | Neo4j, Firestore | A transaction that reads the resource record and writes the new epoch |

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};

/// Error code returned when a lease or run write carries an epoch that is no longer current.
pub const LEASE_STALE_EPOCH_CODE: &str = "lease.stale_epoch";

/// A time-limited grant of a resource to one owner.
///
/// # Example
///
/// ```rust
/// use adk_core::lease::Lease;
/// use chrono::Utc;
///
/// let lease = Lease { resource: "scheduler".into(), owner: "w1".into(), epoch: 1, expires_at: Utc::now() };
/// assert_eq!(serde_json::to_value(&lease).unwrap()["expiresAt"], serde_json::to_value(lease.expires_at).unwrap());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    /// The leased resource name.
    pub resource: String,
    /// The owner the lease was granted to.
    pub owner: String,
    /// The fencing epoch: one higher than the resource's previous grant.
    pub epoch: u64,
    /// When the lease lapses unless renewed.
    pub expires_at: DateTime<Utc>,
}

/// Grants, renews, and releases leases on named resources.
///
/// Every backend passes `adk_core::lease_store_conformance_tests!` (feature `conformance`).
#[async_trait]
pub trait LeaseStore: Send + Sync {
    /// Grants `resource` to `owner` for `ttl`.
    ///
    /// Returns `Some(lease)` when the resource is free, its lease expired or was
    /// released, or `owner` already holds it; the epoch is the previous epoch plus one
    /// (1 for a resource never leased). Returns `None` when another owner holds an
    /// unexpired lease.
    ///
    /// # Errors
    ///
    /// Returns `lease.invalid_ttl` for a zero `ttl`, or a backend error.
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>>;

    /// Extends `lease` to `ttl` from now, keeping its epoch.
    ///
    /// # Errors
    ///
    /// Returns [`LEASE_STALE_EPOCH_CODE`] when the resource's current epoch or owner
    /// differs from `lease`, or the lease was released.
    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease>;

    /// Releases `lease` if it is still current; a stale lease is a no-op.
    async fn release(&self, lease: &Lease) -> Result<()>;

    /// Returns the unexpired, unreleased lease on `resource`, if any.
    async fn current(&self, resource: &str) -> Result<Option<Lease>>;
}

/// Builds the error a lease store returns when a lease is no longer current.
///
/// # Example
///
/// ```rust
/// let error = adk_core::lease::lease_stale_epoch_error("scheduler", 3);
/// assert_eq!(error.code, adk_core::lease::LEASE_STALE_EPOCH_CODE);
/// ```
pub fn lease_stale_epoch_error(resource: &str, epoch: u64) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Conflict,
        LEASE_STALE_EPOCH_CODE,
        format!(
            "lease on '{resource}' at epoch {epoch} is no longer held; stop work done under it \
             and acquire the resource again"
        ),
    )
}

/// Returns `now + ttl`, refusing a zero or overflowing `ttl` with error `code`.
pub(crate) fn deadline(
    now: DateTime<Utc>,
    ttl: Duration,
    code: &'static str,
) -> Result<DateTime<Utc>> {
    chrono::Duration::from_std(ttl)
        .ok()
        .filter(|_| !ttl.is_zero())
        .and_then(|ttl| now.checked_add_signed(ttl))
        .ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                code,
                format!("time to live {ttl:?} must be greater than zero and fit in a timestamp"),
            )
        })
}

#[derive(Debug, Clone)]
struct LeaseSlot {
    lease: Lease,
    released: bool,
}

/// Process-local [`LeaseStore`].
///
/// Leases do not survive a restart and are not shared between processes, so it fences
/// only tasks within one process. Use a durable backend across replicas.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::lease::{InMemoryLeaseStore, LeaseStore};
///
/// # futures::executor::block_on(async {
/// let store = InMemoryLeaseStore::new();
/// let lease = store.acquire("scheduler", "w1", Duration::from_secs(30)).await?.unwrap();
/// assert_eq!(lease.epoch, 1);
/// assert!(store.acquire("scheduler", "w2", Duration::from_secs(30)).await?.is_none());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug)]
pub struct InMemoryLeaseStore {
    slots: Mutex<HashMap<String, LeaseSlot>>,
    clock: Arc<dyn Clock>,
}

impl Default for InMemoryLeaseStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryLeaseStore {
    /// Creates an empty store that reads the system clock.
    pub fn new() -> Self {
        Self { slots: Mutex::new(HashMap::new()), clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl LeaseStore for InMemoryLeaseStore {
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>> {
        let now = self.clock.now();
        let expires_at = deadline(now, ttl, "lease.invalid_ttl")?;
        let mut slots = self.slots.lock().await;
        let previous = match slots.get(resource) {
            Some(slot) => {
                let live = !slot.released && slot.lease.expires_at > now;
                if live && slot.lease.owner != owner {
                    return Ok(None);
                }
                slot.lease.epoch
            }
            None => 0,
        };
        let lease = Lease {
            resource: resource.to_string(),
            owner: owner.to_string(),
            epoch: previous + 1,
            expires_at,
        };
        slots.insert(resource.to_string(), LeaseSlot { lease: lease.clone(), released: false });
        Ok(Some(lease))
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
        let expires_at = deadline(self.clock.now(), ttl, "lease.invalid_ttl")?;
        let mut slots = self.slots.lock().await;
        match slots.get_mut(&lease.resource) {
            Some(slot)
                if !slot.released
                    && slot.lease.epoch == lease.epoch
                    && slot.lease.owner == lease.owner =>
            {
                slot.lease.expires_at = expires_at;
                Ok(slot.lease.clone())
            }
            _ => Err(lease_stale_epoch_error(&lease.resource, lease.epoch)),
        }
    }

    async fn release(&self, lease: &Lease) -> Result<()> {
        let mut slots = self.slots.lock().await;
        if let Some(slot) = slots.get_mut(&lease.resource)
            && slot.lease.epoch == lease.epoch
            && slot.lease.owner == lease.owner
        {
            slot.released = true;
        }
        Ok(())
    }

    async fn current(&self, resource: &str) -> Result<Option<Lease>> {
        let now = self.clock.now();
        let slots = self.slots.lock().await;
        Ok(slots
            .get(resource)
            .filter(|slot| !slot.released && slot.lease.expires_at > now)
            .map(|slot| slot.lease.clone()))
    }
}
