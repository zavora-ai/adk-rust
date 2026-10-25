//! In-memory edge stores, for development, tests and single-process edges (spec 3.5 §7.9).
//!
//! Records live for the life of the process. Every store is cheap to clone; clones share
//! their records, as handles to one database do.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{
    ConsentReceipt, ConsentScope, EventType, HeldCallState, LeadRef, RequesterId, SiteStatus,
    SubscriptionState, SubscriptionSuspended,
};
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::{
    ConsentReceiptStore, ConsentRecord, HeldCallRecord, HeldCallStore, HeldCallTransition,
    IdempotencyBegin, IdempotencyStore, Lead, LeadStore, RecordedOutcome, StatusStore, StoreError,
    StoreResult, SubscriptionRecord, SubscriptionStore, SubscriptionUpdate,
    held_call_transition_allowed,
};

/// In-memory [`SubscriptionStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemorySubscriptionStore {
    records: Arc<RwLock<HashMap<String, SubscriptionRecord>>>,
}

impl InMemorySubscriptionStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SubscriptionStore for InMemorySubscriptionStore {
    async fn create(&self, record: SubscriptionRecord) -> StoreResult<()> {
        let mut records = self.records.write().await;
        let id = record.subscription.id.clone();
        if records.contains_key(&id) {
            return Err(StoreError::Conflict(format!("subscription {id} already exists")));
        }
        records.insert(id, record);
        Ok(())
    }

    async fn get(&self, owner: &RequesterId, id: &str) -> StoreResult<Option<SubscriptionRecord>> {
        Ok(self.records.read().await.get(id).filter(|record| &record.owner == owner).cloned())
    }

    async fn list(&self, owner: &RequesterId) -> StoreResult<Vec<SubscriptionRecord>> {
        let mut found: Vec<SubscriptionRecord> = self
            .records
            .read()
            .await
            .values()
            .filter(|record| &record.owner == owner)
            .cloned()
            .collect();
        found.sort_by(|a, b| a.subscription.id.cmp(&b.subscription.id));
        Ok(found)
    }

    async fn active_for(&self, event_type: &EventType) -> StoreResult<Vec<SubscriptionRecord>> {
        let mut found: Vec<SubscriptionRecord> = self
            .records
            .read()
            .await
            .values()
            .filter(|record| {
                record.subscription.state == SubscriptionState::Active
                    && record.subscription.types.contains(event_type)
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| a.subscription.id.cmp(&b.subscription.id));
        Ok(found)
    }

    async fn update(
        &self,
        owner: &RequesterId,
        id: &str,
        update: SubscriptionUpdate,
    ) -> StoreResult<SubscriptionRecord> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(id)
            .filter(|record| &record.owner == owner)
            .ok_or_else(|| StoreError::NotFound(format!("subscription {id}")))?;
        if let Some(types) = update.types {
            record.subscription.types = types;
        }
        if let Some(url) = update.url {
            record.subscription.url = url;
        }
        if let Some(state) = update.state {
            if state == SubscriptionState::Active {
                record.subscription.failures = 0;
            }
            record.subscription.state = state;
        }
        Ok(record.clone())
    }

    async fn record_delivery(&self, id: &str, delivered: bool) -> StoreResult<SubscriptionRecord> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(format!("subscription {id}")))?;
        if delivered {
            record.subscription.failures = 0;
        } else {
            record.subscription.failures = record.subscription.failures.saturating_add(1);
            if record.subscription.failures >= SubscriptionSuspended::MAX_FAILURES {
                record.subscription.state = SubscriptionState::Suspended;
            }
        }
        Ok(record.clone())
    }

    async fn delete(&self, owner: &RequesterId, id: &str) -> StoreResult<bool> {
        let mut records = self.records.write().await;
        if records.get(id).is_some_and(|record| &record.owner == owner) {
            records.remove(id);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// In-memory [`ConsentReceiptStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryConsentReceiptStore {
    records: Arc<RwLock<HashMap<Uuid, ConsentRecord>>>,
}

impl InMemoryConsentReceiptStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConsentReceiptStore for InMemoryConsentReceiptStore {
    async fn insert(&self, receipt: ConsentReceipt) -> StoreResult<()> {
        let mut records = self.records.write().await;
        if records.contains_key(&receipt.id) {
            return Err(StoreError::Conflict(format!(
                "consent receipt {} already exists",
                receipt.id
            )));
        }
        records.insert(receipt.id, ConsentRecord { receipt, revoked_at: None });
        Ok(())
    }

    async fn get(&self, id: Uuid) -> StoreResult<Option<ConsentRecord>> {
        Ok(self.records.read().await.get(&id).cloned())
    }

    async fn for_subject(&self, subject_ref: &str) -> StoreResult<Vec<ConsentRecord>> {
        let mut found: Vec<ConsentRecord> = self
            .records
            .read()
            .await
            .values()
            .filter(|record| record.receipt.subject_ref == subject_ref)
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            (a.receipt.granted_at, a.receipt.id).cmp(&(b.receipt.granted_at, b.receipt.id))
        });
        Ok(found)
    }

    async fn revoke(&self, id: Uuid, at: DateTime<Utc>) -> StoreResult<Option<ConsentRecord>> {
        let mut records = self.records.write().await;
        Ok(records.get_mut(&id).map(|record| {
            record.revoked_at.get_or_insert(at);
            record.clone()
        }))
    }

    async fn revoke_scope(
        &self,
        subject_ref: &str,
        scope: &ConsentScope,
        at: DateTime<Utc>,
    ) -> StoreResult<Vec<ConsentRecord>> {
        let mut records = self.records.write().await;
        let mut revoked: Vec<ConsentRecord> = records
            .values_mut()
            .filter(|record| {
                record.revoked_at.is_none()
                    && record.receipt.subject_ref == subject_ref
                    && &record.receipt.scope == scope
            })
            .map(|record| {
                record.revoked_at = Some(at);
                record.clone()
            })
            .collect();
        revoked.sort_by_key(|record| record.receipt.id);
        Ok(revoked)
    }

    async fn erase(&self, subject_ref: &str) -> StoreResult<usize> {
        let mut records = self.records.write().await;
        let before = records.len();
        records.retain(|_, record| record.receipt.subject_ref != subject_ref);
        Ok(before - records.len())
    }
}

/// In-memory [`StatusStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryStatusStore {
    current: Arc<RwLock<Option<SiteStatus>>>,
}

impl InMemoryStatusStore {
    /// Creates a store with no published status.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StatusStore for InMemoryStatusStore {
    async fn current(&self) -> StoreResult<Option<SiteStatus>> {
        Ok(self.current.read().await.clone())
    }

    async fn set(&self, status: SiteStatus) -> StoreResult<Option<SiteStatus>> {
        Ok(self.current.write().await.replace(status))
    }
}

/// In-memory [`HeldCallStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryHeldCallStore {
    records: Arc<RwLock<HashMap<String, HeldCallRecord>>>,
}

impl InMemoryHeldCallStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl HeldCallStore for InMemoryHeldCallStore {
    async fn create(&self, record: HeldCallRecord) -> StoreResult<()> {
        if record.call.state != HeldCallState::Pending {
            return Err(StoreError::Conflict("a held call starts pending".to_owned()));
        }
        let mut records = self.records.write().await;
        if records.contains_key(&record.call.id) {
            return Err(StoreError::Conflict(format!(
                "held call {} already exists",
                record.call.id
            )));
        }
        records.insert(record.call.id.clone(), record);
        Ok(())
    }

    async fn get(&self, id: &str) -> StoreResult<Option<HeldCallRecord>> {
        Ok(self.records.read().await.get(id).cloned())
    }

    async fn pending(&self) -> StoreResult<Vec<HeldCallRecord>> {
        let mut found: Vec<HeldCallRecord> = self
            .records
            .read()
            .await
            .values()
            .filter(|record| record.call.state == HeldCallState::Pending)
            .cloned()
            .collect();
        found.sort_by(|a, b| (a.call.created_at, &a.call.id).cmp(&(b.call.created_at, &b.call.id)));
        Ok(found)
    }

    async fn transition(
        &self,
        id: &str,
        transition: HeldCallTransition,
    ) -> StoreResult<HeldCallRecord> {
        let mut records = self.records.write().await;
        let record =
            records.get_mut(id).ok_or_else(|| StoreError::NotFound(format!("held call {id}")))?;
        if !held_call_transition_allowed(record.call.state, transition.to) {
            return Err(StoreError::Conflict(format!(
                "held call {id} cannot move from {:?} to {:?}",
                record.call.state, transition.to
            )));
        }
        if transition.to == HeldCallState::Completed
            && transition.result.is_none()
            && transition.error.is_none()
        {
            return Err(StoreError::Conflict(format!(
                "held call {id} completes with a result or an error"
            )));
        }
        record.call.state = transition.to;
        if transition.result.is_some() {
            record.call.result = transition.result;
        }
        if transition.error.is_some() {
            record.call.error = transition.error;
        }
        Ok(record.clone())
    }
}

/// In-memory [`LeadStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryLeadStore {
    records: Arc<RwLock<HashMap<LeadRef, Lead>>>,
}

impl InMemoryLeadStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LeadStore for InMemoryLeadStore {
    async fn insert(&self, lead: Lead) -> StoreResult<()> {
        let mut records = self.records.write().await;
        if records.contains_key(&lead.id) {
            return Err(StoreError::Conflict(format!("lead {} already exists", lead.id.0)));
        }
        records.insert(lead.id.clone(), lead);
        Ok(())
    }

    async fn get(&self, id: &LeadRef) -> StoreResult<Option<Lead>> {
        Ok(self.records.read().await.get(id).cloned())
    }

    async fn since(&self, since: DateTime<Utc>) -> StoreResult<Vec<Lead>> {
        let mut found: Vec<Lead> = self
            .records
            .read()
            .await
            .values()
            .filter(|lead| lead.captured_at >= since)
            .cloned()
            .collect();
        found.sort_by(|a, b| (a.captured_at, &a.id).cmp(&(b.captured_at, &b.id)));
        Ok(found)
    }

    async fn set_score(&self, id: &LeadRef, score: f64) -> StoreResult<()> {
        let mut records = self.records.write().await;
        let lead =
            records.get_mut(id).ok_or_else(|| StoreError::NotFound(format!("lead {}", id.0)))?;
        lead.score = Some(score);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct IdempotencyRecord {
    input_digest: String,
    began_at: DateTime<Utc>,
    outcome: Option<RecordedOutcome>,
}

/// In-memory [`IdempotencyStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryIdempotencyStore {
    records: Arc<RwLock<HashMap<String, IdempotencyRecord>>>,
}

impl InMemoryIdempotencyStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl IdempotencyStore for InMemoryIdempotencyStore {
    async fn begin(
        &self,
        key: &str,
        input_digest: &str,
        at: DateTime<Utc>,
    ) -> StoreResult<IdempotencyBegin> {
        let mut records = self.records.write().await;
        Ok(match records.get(key) {
            None => {
                records.insert(
                    key.to_owned(),
                    IdempotencyRecord {
                        input_digest: input_digest.to_owned(),
                        began_at: at,
                        outcome: None,
                    },
                );
                IdempotencyBegin::Started
            }
            Some(record) if record.input_digest != input_digest => IdempotencyBegin::Conflict,
            Some(IdempotencyRecord { outcome: Some(outcome), .. }) => {
                IdempotencyBegin::Recorded(outcome.clone())
            }
            Some(_) => IdempotencyBegin::InFlight,
        })
    }

    async fn complete(&self, key: &str, outcome: RecordedOutcome) -> StoreResult<()> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(key)
            .ok_or_else(|| StoreError::NotFound(format!("idempotency key {key}")))?;
        if record.outcome.is_some() {
            return Err(StoreError::Conflict(format!(
                "idempotency key {key} already has an outcome; outcomes are write-once"
            )));
        }
        record.outcome = Some(outcome);
        Ok(())
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
        let mut records = self.records.write().await;
        let before = records.len();
        records.retain(|_, record| record.began_at >= cutoff);
        Ok(before - records.len())
    }
}

/// One in-memory store of each kind, sharing nothing with other instances; clones share
/// their records.
///
/// # Example
///
/// ```
/// use adk_awp::v2::store::{MemoryEdgeStores, StatusStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let stores = MemoryEdgeStores::default();
/// assert!(stores.status.current().await.unwrap().is_none());
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct MemoryEdgeStores {
    /// Subscriptions.
    pub subscriptions: InMemorySubscriptionStore,
    /// Consent receipts.
    pub consent: InMemoryConsentReceiptStore,
    /// Site status.
    pub status: InMemoryStatusStore,
    /// Held calls.
    pub held_calls: InMemoryHeldCallStore,
    /// Leads.
    pub leads: InMemoryLeadStore,
    /// Idempotency records.
    pub idempotency: InMemoryIdempotencyStore,
}
