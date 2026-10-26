//! Edge stores: subscriptions, consent receipts, status, held calls, leads and idempotency
//! records (spec 3.5 §7.9).
//!
//! Each store is a trait with an in-memory implementation in [`memory`] and a conformance
//! suite in [`conformance`] that every backend runs. SQLite, PostgreSQL and Redis backends
//! implement the same traits elsewhere.
//!
//! | Store | Holds | Spec |
//! |---|---|---|
//! | [`SubscriptionStore`] | Event subscriptions, scoped to the requester that created them | §7.5, §7.9 |
//! | [`ConsentReceiptStore`] | Signed consent receipts and their revocation | §7.5, A-I8 |
//! | [`StatusStore`] | The current signed site status | §7.5 |
//! | [`HeldCallStore`] | Calls waiting for the owner, and their outcome | §7.4 |
//! | [`LeadStore`] | Leads captured at the edge | §7.9, §7.10 |
//! | [`IdempotencyStore`] | Outcomes of keyed calls, so a repeat is answered without executing | §7.4, A-I7 |

use std::collections::BTreeMap;

use async_trait::async_trait;
use awp_types::{
    Bounded, Channel, ConsentReceipt, ConsentReceiptRef, ConsentRequirement, ConsentScope,
    ContactRef, ErrorBody, EventSubscription, EventType, HeldCall, HeldCallState, LeadRef,
    RequesterId, SiteStatus, SubscriptionState, ThreadRef, TrustTier,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub mod conformance;
pub mod memory;

pub use memory::{
    InMemoryConsentReceiptStore, InMemoryHeldCallStore, InMemoryIdempotencyStore,
    InMemoryLeadStore, InMemoryStatusStore, InMemorySubscriptionStore, MemoryEdgeStores,
};

/// Why an edge store operation failed (spec 3.5 §7.9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// No record with this id, or none the caller may see.
    #[error("{0} was not found")]
    NotFound(String),
    /// The operation conflicts with the stored state.
    #[error("{0}")]
    Conflict(String),
    /// The backend failed.
    #[error("edge store backend failed: {0}; check the store's connection and retry")]
    Backend(String),
}

/// The result of an edge store operation.
pub type StoreResult<T> = Result<T, StoreError>;

// --- Subscriptions -------------------------------------------------------------------------

/// A stored event subscription and the requester that owns it (spec 3.5 §7.5).
///
/// The store keeps the signing secret in `subscription.secret`; routes return it only in
/// the response that creates the subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionRecord {
    /// The verified requester that created the subscription.
    pub owner: RequesterId,
    /// The subscription, with its secret.
    pub subscription: EventSubscription,
}

/// A change to a subscription: `PATCH /awp/v2/events/{id}` (spec 3.5 §7.9).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionUpdate {
    /// New event types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<EventType>>,
    /// New delivery URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `active` reactivates a suspended subscription and clears its failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SubscriptionState>,
}

/// Event subscriptions, each visible only to the requester that created it (spec 3.5 §7.5).
#[async_trait]
pub trait SubscriptionStore: Send + Sync {
    /// Stores a new subscription.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] when the id is taken.
    async fn create(&self, record: SubscriptionRecord) -> StoreResult<()>;

    /// Returns subscription `id` when `owner` created it.
    async fn get(&self, owner: &RequesterId, id: &str) -> StoreResult<Option<SubscriptionRecord>>;

    /// Returns every subscription `owner` created.
    async fn list(&self, owner: &RequesterId) -> StoreResult<Vec<SubscriptionRecord>>;

    /// Returns the active subscriptions that name `event_type`, for delivery.
    async fn active_for(&self, event_type: &EventType) -> StoreResult<Vec<SubscriptionRecord>>;

    /// Applies `update` to subscription `id` of `owner` and returns it.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when `owner` has no subscription `id`.
    async fn update(
        &self,
        owner: &RequesterId,
        id: &str,
        update: SubscriptionUpdate,
    ) -> StoreResult<SubscriptionRecord>;

    /// Records a delivery outcome: success clears the failure count; a failure increments it
    /// and suspends the subscription at
    /// [`SubscriptionSuspended::MAX_FAILURES`](awp_types::SubscriptionSuspended::MAX_FAILURES).
    /// Returns the subscription afterwards.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when no subscription has id `id`.
    async fn record_delivery(&self, id: &str, delivered: bool) -> StoreResult<SubscriptionRecord>;

    /// Deletes subscription `id` of `owner`; returns whether it existed.
    async fn delete(&self, owner: &RequesterId, id: &str) -> StoreResult<bool>;
}

// --- Consent receipts ----------------------------------------------------------------------

/// A stored consent receipt and when it was revoked (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRecord {
    /// The signed receipt as issued.
    pub receipt: ConsentReceipt,
    /// When it was revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ConsentRecord {
    /// Returns `true` when the receipt covers `requirement` and is neither revoked nor
    /// expired at `at` (spec 3.5 A-I8).
    pub fn is_live(&self, requirement: &ConsentRequirement, at: DateTime<Utc>) -> bool {
        self.revoked_at.is_none_or(|revoked| at < revoked) && self.receipt.covers(requirement, at)
    }
}

/// Consent receipts the edge issued (spec 3.5 §7.5, A-I8).
#[async_trait]
pub trait ConsentReceiptStore: Send + Sync {
    /// Stores a receipt the edge signed.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] when the id is taken.
    async fn insert(&self, receipt: ConsentReceipt) -> StoreResult<()>;

    /// Returns receipt `id`.
    async fn get(&self, id: Uuid) -> StoreResult<Option<ConsentRecord>>;

    /// Returns every receipt of `subject_ref`.
    async fn for_subject(&self, subject_ref: &str) -> StoreResult<Vec<ConsentRecord>>;

    /// Revokes receipt `id` at `at`; a revoked receipt keeps its first revocation time.
    /// Returns the record, or `None` when no receipt has that id.
    async fn revoke(&self, id: Uuid, at: DateTime<Utc>) -> StoreResult<Option<ConsentRecord>>;

    /// Revokes every live receipt of `subject_ref` for `scope` at `at` and returns them.
    async fn revoke_scope(
        &self,
        subject_ref: &str,
        scope: &ConsentScope,
        at: DateTime<Utc>,
    ) -> StoreResult<Vec<ConsentRecord>>;

    /// Erases every receipt of `subject_ref` (`POST /awp/v2/consent/erase`); returns how many.
    async fn erase(&self, subject_ref: &str) -> StoreResult<usize>;
}

/// Returns `true` when one of `receipts` is live for `requirement` at `at` (spec 3.5 A-I8).
///
/// # Errors
///
/// As [`ConsentReceiptStore::get`].
pub async fn has_live_receipt(
    store: &dyn ConsentReceiptStore,
    receipts: &[Uuid],
    requirement: &ConsentRequirement,
    at: DateTime<Utc>,
) -> StoreResult<bool> {
    for id in receipts {
        if store.get(*id).await?.is_some_and(|record| record.is_live(requirement, at)) {
            return Ok(true);
        }
    }
    Ok(false)
}

// --- Status --------------------------------------------------------------------------------

/// The current signed site status (spec 3.5 §7.5).
#[async_trait]
pub trait StatusStore: Send + Sync {
    /// Returns the current status, if one was published.
    async fn current(&self) -> StoreResult<Option<SiteStatus>>;

    /// Replaces the status and returns the previous one, for `site.status.changed`.
    async fn set(&self, status: SiteStatus) -> StoreResult<Option<SiteStatus>>;
}

// --- Held calls ----------------------------------------------------------------------------

/// A held call with the request it holds (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldCallRecord {
    /// The held-call document.
    pub call: HeldCall,
    /// The verified requester that made the call; only it may read the call from `known` up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<RequesterId>,
    /// The caller's tier when it made the call.
    #[serde(default)]
    pub tier: TrustTier,
    /// The call's input, executed after approval.
    pub input: Value,
    /// The call's idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Consent receipt ids the call referenced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consent: Vec<Uuid>,
}

/// Returns `true` when a held call may move from `from` to `to` (spec 3.5 §7.4): a pending
/// call is approved, rejected or expires; an approved call completes.
pub fn held_call_transition_allowed(from: HeldCallState, to: HeldCallState) -> bool {
    matches!(
        (from, to),
        (HeldCallState::Pending, HeldCallState::Approved)
            | (HeldCallState::Pending, HeldCallState::Rejected)
            | (HeldCallState::Pending, HeldCallState::Expired)
            | (HeldCallState::Approved, HeldCallState::Completed)
    )
}

/// The outcome a held call moves to (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct HeldCallTransition {
    /// The new state.
    pub to: HeldCallState,
    /// The output, for a completed call.
    pub result: Option<Value>,
    /// The error, for a rejected, expired or failed call.
    pub error: Option<ErrorBody>,
}

/// Calls held for the owner's decision (spec 3.5 §7.4).
#[async_trait]
pub trait HeldCallStore: Send + Sync {
    /// Stores a new pending call.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] when the id is taken or the call is not pending.
    async fn create(&self, record: HeldCallRecord) -> StoreResult<()>;

    /// Returns call `id`.
    async fn get(&self, id: &str) -> StoreResult<Option<HeldCallRecord>>;

    /// Returns the pending calls.
    async fn pending(&self) -> StoreResult<Vec<HeldCallRecord>>;

    /// Moves call `id` to `transition.to` and returns it.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for an unknown id; [`StoreError::Conflict`] when
    /// [`held_call_transition_allowed`] refuses the move, or a completed call carries neither
    /// a result nor an error.
    async fn transition(
        &self,
        id: &str,
        transition: HeldCallTransition,
    ) -> StoreResult<HeldCallRecord>;
}

// --- Leads ---------------------------------------------------------------------------------

/// A lead captured at the edge (spec 3.5 §7.9, §7.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lead {
    /// Lead id; the `leadRef` of the `lead_captured` signal.
    pub id: LeadRef,
    /// When it was captured.
    pub captured_at: DateTime<Utc>,
    /// Where it came from.
    pub source: Channel,
    /// How to reach the person; never the address itself.
    pub contact_ref: ContactRef,
    /// The consent receipts the person granted.
    #[serde(default)]
    pub consent: Vec<ConsentReceiptRef>,
    /// Answers to qualifying questions, by question id.
    #[serde(default)]
    pub answers: BTreeMap<String, Bounded>,
    /// The conversation thread it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ref: Option<ThreadRef>,
    /// The back office's qualification score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// Leads captured at the edge (spec 3.5 §7.9).
#[async_trait]
pub trait LeadStore: Send + Sync {
    /// Stores a new lead.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] when the id is taken.
    async fn insert(&self, lead: Lead) -> StoreResult<()>;

    /// Returns lead `id`.
    async fn get(&self, id: &LeadRef) -> StoreResult<Option<Lead>>;

    /// Returns the leads captured at or after `since`, oldest first.
    async fn since(&self, since: DateTime<Utc>) -> StoreResult<Vec<Lead>>;

    /// Sets lead `id`'s score.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for an unknown id.
    async fn set_score(&self, id: &LeadRef, score: f64) -> StoreResult<()>;

    /// Deletes lead `id`, so a subject's erasure reaches it (spec 3.5 §7.5; AWP 2.0 §11.3);
    /// returns whether it existed.
    ///
    /// The provided method refuses: a store that keeps leads must be able to erase them, and
    /// every store in this crate and in `adk-server` overrides it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] from a store that cannot delete.
    async fn delete(&self, id: &LeadRef) -> StoreResult<bool> {
        Err(StoreError::Backend(format!(
            "this lead store cannot delete lead {}; implement LeadStore::delete so erasure \
             reaches leads",
            id.0
        )))
    }
}

// --- Idempotency ---------------------------------------------------------------------------

/// The recorded outcome of a keyed call (spec 3.5 §7.4, A-I7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum RecordedOutcome {
    /// The capability returned this output.
    Completed {
        /// The output object.
        output: Value,
    },
    /// The call was held.
    Held {
        /// The held call's id.
        call_id: String,
    },
    /// The call failed after it began.
    Failed {
        /// The error body answered.
        error: ErrorBody,
    },
}

/// What [`IdempotencyStore::begin`] found for a key (spec 3.5 §7.4, A-I7).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum IdempotencyBegin {
    /// No record existed; the call may execute and must be completed.
    Started,
    /// The same input is executing under this key.
    InFlight,
    /// The same input finished under this key; answer with the outcome.
    Recorded(RecordedOutcome),
    /// The key was used with a different input.
    Conflict,
}

/// Outcomes of keyed calls, so a repeat is answered without executing again (spec 3.5 §7.4,
/// A-I7; AWP 2.0 §7.7).
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Begins a call under `key` with input digest `input_digest`, atomically: exactly one
    /// concurrent caller gets [`IdempotencyBegin::Started`].
    async fn begin(
        &self,
        key: &str,
        input_digest: &str,
        at: DateTime<Utc>,
    ) -> StoreResult<IdempotencyBegin>;

    /// Records the outcome of a call that began under `key`.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when no call began under `key`; [`StoreError::Conflict`]
    /// when an outcome is already recorded.
    async fn complete(&self, key: &str, outcome: RecordedOutcome) -> StoreResult<()>;

    /// Removes records that began before `cutoff`; keys must be kept at least 24 hours.
    async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize>;
}
