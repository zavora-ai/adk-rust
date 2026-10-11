//! Firestore-backed [`SpendLedger`](adk_core::SpendLedger), [`ActionLedger`](adk_core::ActionLedger),
//! and [`ApprovalStore`](adk_core::ApprovalStore).
//!
//! Every read-modify-write runs in a Firestore read-write transaction. Firestore
//! transactions are serializable: a transaction that read a document another
//! transaction changed before it committed is aborted, and the `firestore` crate retries
//! it with exponential backoff.
//!
//! | Store | Collection (document ID) | How it stays atomic |
//! |-------|--------------------------|---------------------|
//! | [`FirestoreSpendLedger`] | `adk_spend_entries` (reservation ID), `adk_spend_orgs` (hex of the organization), `adk_spend_orgs/{org}/days` (UTC date) | `reserve` reads the organization document, the current month's day totals, and the live holds, then writes the hold and bumps the organization's `seq`, so two reservations for one organization conflict and one retries |
//! | [`FirestoreActionLedger`] | `adk_action_ledger` (digest of the key) | `begin` is a create, which fails when the document exists; `complete` re-reads the record in a transaction |
//! | [`FirestoreApprovalStore`] | `adk_approvals` (digest of scope, kind, and fingerprint) | `take_decision` reads and deletes the decision in one transaction, so concurrent takers see it once |
//! | [`FirestoreApprovalStore`] requests and delegations | `adk_approval_requests`, `adk_approval_delegations` (digest of the ID) | `create_request` is a create, which fails when the document exists; `replace_request` reads and compares the version in a transaction, so of concurrent replaces at one version one commits and the others retry, see the new version, and report `false` |
//!
//! `list_requests` queries with equality filters only — the application, `open`, or a
//! single state — and orders, pages, and applies the remaining filters in memory, so no
//! composite index is needed; a listing reads every request of the application (or
//! every open request) it filters.
//!
//! Document IDs cannot hold `/` and are length-limited, so idempotency keys and
//! fingerprints are addressed by their SHA-256 in hex. Each document also stores the full key,
//! fingerprint, and scope, and every read checks them: two values that share a digest
//! never read each other's record. A write that would land on a document holding a
//! different value fails instead of overwriting it.
//!
//! Committed spend is kept as per-attribution totals, one document per UTC day plus a
//! lifetime total on the organization document, so a reservation reads at most one
//! month of day documents however many calls were made. Every query uses equality
//! filters or one range filter, which Firestore serves from its automatic single-field
//! indexes; no composite index is needed.
//!
//! Timestamps on action records and approvals are stored as RFC 3339 text with
//! nanoseconds, so they read back exactly.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use adk_core::action_ledger::{already_completed_error, duplicate_key_error, missing_key_error};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, ApprovalDelegation, ApprovalRequest,
    ApprovalRequestQuery, ApprovalScope, ApprovalStore, Clock, DEFAULT_RESERVATION_TTL,
    ErrorCategory, ErrorComponent, PendingApproval, ReservationId, Result, SpendError, SpendKey,
    SpendLedger, SpendLimits, SpendPeriod, SystemClock, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use firestore::errors::{BackoffError, FirestoreError};
use firestore::{
    FirestoreDb, FirestoreTransaction, FirestoreTransactionOptions, FirestoreWritePrecondition,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const SPEND_ENTRIES: &str = "adk_spend_entries";
const SPEND_ORGS: &str = "adk_spend_orgs";
const SPEND_DAYS: &str = "days";
const ACTIONS: &str = "adk_action_ledger";
const APPROVALS: &str = "adk_approvals";
const APPROVAL_REQUESTS: &str = "adk_approval_requests";
const APPROVAL_DELEGATIONS: &str = "adk_approval_delegations";

const STATE_RESERVED: &str = "reserved";
const STATE_COMMITTED: &str = "committed";
const STATE_RELEASED: &str = "released";

const KIND_PENDING: &str = "pending";
const KIND_DECISION: &str = "decision";

/// How long one operation may spend retrying conflicting transactions.
const TRANSACTION_DEADLINE: chrono::TimeDelta = chrono::TimeDelta::seconds(120);

type TxResult<T> = std::result::Result<T, BackoffError<AdkError>>;
type TxFuture<'b, T> = Pin<Box<dyn Future<Output = TxResult<T>> + Send + 'b>>;

fn transaction_options() -> FirestoreTransactionOptions {
    FirestoreTransactionOptions::new().with_max_elapsed_time(TRANSACTION_DEADLINE)
}

/// Retries a transaction on a read Firestore reports as retryable, such as a contention
/// abort, and gives up on any other failure.
fn retry_or_fail(
    error: FirestoreError,
    fail: impl FnOnce(FirestoreError) -> AdkError,
) -> BackoffError<AdkError> {
    let retryable = matches!(&error, FirestoreError::DatabaseError(db) if db.retry_possible);
    if retryable {
        BackoffError::transient(fail(error))
    } else {
        BackoffError::permanent(fail(error))
    }
}

/// Recovers the [`AdkError`] a transaction body failed with, or reports the transaction
/// failure itself through `fail`.
fn transaction_error(error: FirestoreError, fail: impl FnOnce(String) -> AdkError) -> AdkError {
    match error {
        FirestoreError::ErrorInTransaction(inner) => match inner.source.downcast::<AdkError>() {
            Ok(adk_error) => *adk_error,
            Err(source) => fail(source.to_string()),
        },
        other => fail(other.to_string()),
    }
}

fn storage_error(code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Session, ErrorCategory::Unavailable, code, message)
}

fn to_json<T: Serialize>(value: &T, fail: impl FnOnce(String) -> AdkError) -> Result<String> {
    serde_json::to_string(value).map_err(|e| fail(format!("serialize failed: {e}")))
}

fn from_json<T: serde::de::DeserializeOwned>(
    text: &str,
    what: &str,
    fail: impl FnOnce(String) -> AdkError,
) -> Result<T> {
    serde_json::from_str(text).map_err(|e| fail(format!("stored {what} is corrupt: {e}")))
}

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

async fn connect(project_id: &str) -> std::result::Result<FirestoreDb, String> {
    FirestoreDb::new(project_id).await.map_err(|e| format!("firestore connection failed: {e}"))
}

/// The Firestore emulator accepts the `owner` token as an administrator.
struct EmulatorOwnerToken;

#[async_trait]
impl gcloud_sdk::Source for EmulatorOwnerToken {
    async fn token(&self) -> gcloud_sdk::error::Result<gcloud_sdk::Token> {
        Ok(gcloud_sdk::Token::new(
            "Bearer".to_string(),
            "owner".into(),
            Utc::now() + chrono::Duration::hours(1),
        ))
    }
}

/// Connects to the Firestore emulator at `FIRESTORE_EMULATOR_HOST` without Application
/// Default Credentials, for local development and tests.
///
/// Pass the client to `from_db` on any Firestore store.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::FirestoreSpendLedger;
/// use adk_session::firestore_stores::connect_firestore_emulator;
///
/// # async fn demo() -> adk_core::Result<()> {
/// // FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
/// let ledger = FirestoreSpendLedger::from_db(connect_firestore_emulator("demo-project").await?);
/// # let _ = ledger;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns `session.firestore.emulator` when `FIRESTORE_EMULATOR_HOST` is unset or the
/// client cannot be created. Without the variable the client would reach production
/// Firestore with a token it rejects, so the call fails instead.
pub async fn connect_firestore_emulator(project_id: &str) -> Result<FirestoreDb> {
    let fail = |message: String| storage_error("session.firestore.emulator", message);
    if std::env::var_os("FIRESTORE_EMULATOR_HOST").is_none() {
        return Err(fail(
            "FIRESTORE_EMULATOR_HOST is not set; set it to the emulator's host:port".to_string(),
        ));
    }
    FirestoreDb::with_options_token_source(
        firestore::FirestoreDbOptions::new(project_id.to_string()),
        Vec::new(),
        gcloud_sdk::TokenSourceType::ExternalSource(Box::new(EmulatorOwnerToken)),
    )
    .await
    .map_err(|e| fail(format!("firestore emulator connection failed: {e}")))
}

// ---------------------------------------------------------------------------
// Spend ledger
// ---------------------------------------------------------------------------

fn spend_unavailable(error: impl std::fmt::Display) -> AdkError {
    SpendError::Unavailable(error.to_string()).into()
}

fn stored_amount(amount_micro_usd: u64) -> Result<i64> {
    i64::try_from(amount_micro_usd).map_err(|_| SpendError::AmountTooLarge(amount_micro_usd).into())
}

/// Committed spend for one attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttributedTotal {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    micro_usd: i64,
}

impl AttributedTotal {
    fn key(&self, org: &str) -> SpendKey {
        SpendKey {
            org: org.to_string(),
            agent: self.agent.clone(),
            vendor: self.vendor.clone(),
            period: SpendPeriod::Lifetime,
        }
    }
}

fn add_to_totals(totals: &mut Vec<AttributedTotal>, key: &SpendKey, amount: i64) {
    match totals.iter_mut().find(|total| total.agent == key.agent && total.vendor == key.vendor) {
        Some(total) => total.micro_usd = total.micro_usd.saturating_add(amount),
        None => totals.push(AttributedTotal {
            agent: key.agent.clone(),
            vendor: key.vendor.clone(),
            micro_usd: amount,
        }),
    }
}

fn covered_total<'a>(
    org: &str,
    scope: &SpendKey,
    totals: impl IntoIterator<Item = &'a AttributedTotal>,
) -> u64 {
    totals
        .into_iter()
        .filter(|total| scope.covers(&total.key(org)))
        .fold(0u64, |sum, total| sum.saturating_add(u64::try_from(total.micro_usd).unwrap_or(0)))
}

/// One organization: the lock every reservation for it writes, and lifetime totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OrgDoc {
    org: String,
    #[serde(default)]
    seq: i64,
    #[serde(default)]
    lifetime: Vec<AttributedTotal>,
}

/// Committed spend settled on one UTC day.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DayDoc {
    /// `YYYY-MM-DD`, so text order is date order.
    day: String,
    #[serde(default)]
    totals: Vec<AttributedTotal>,
}

/// One reservation, kept after settlement for audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntryDoc {
    org: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    state: String,
    reserved_micro_usd: i64,
    #[serde(default)]
    committed_micro_usd: Option<i64>,
    created_at_us: i64,
    expires_at_us: i64,
    #[serde(default)]
    settled_at_us: Option<i64>,
}

impl EntryDoc {
    fn key(&self) -> SpendKey {
        SpendKey {
            org: self.org.clone(),
            agent: self.agent.clone(),
            vendor: self.vendor.clone(),
            period: SpendPeriod::Lifetime,
        }
    }
}

fn day_id(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

/// The reads one reservation's limit check needs.
struct OrgSnapshot {
    org: OrgDoc,
    /// Day documents of the current month, up to and including today.
    month: Vec<DayDoc>,
    holds: Vec<EntryDoc>,
}

impl OrgSnapshot {
    fn committed(&self, scope: &SpendKey, period: SpendPeriod, today: &str) -> u64 {
        let org = &self.org.org;
        match period {
            SpendPeriod::Day => covered_total(
                org,
                scope,
                self.month.iter().filter(|day| day.day == today).flat_map(|day| &day.totals),
            ),
            SpendPeriod::Month => {
                covered_total(org, scope, self.month.iter().flat_map(|day| &day.totals))
            }
            SpendPeriod::Lifetime => covered_total(org, scope, &self.org.lifetime),
        }
    }

    fn held(&self, scope: &SpendKey, now_us: i64) -> u64 {
        self.holds
            .iter()
            .filter(|hold| hold.expires_at_us > now_us && scope.covers(&hold.key()))
            .fold(0u64, |sum, hold| {
                sum.saturating_add(u64::try_from(hold.reserved_micro_usd).unwrap_or(0))
            })
    }
}

async fn read_org(db: &FirestoreDb, org: &str) -> std::result::Result<OrgDoc, FirestoreError> {
    let stored: Option<OrgDoc> =
        db.fluent().select().by_id_in(SPEND_ORGS).obj::<OrgDoc>().one(hex(org)).await?;
    Ok(stored.unwrap_or_else(|| OrgDoc { org: org.to_string(), ..OrgDoc::default() }))
}

async fn read_month(
    db: &FirestoreDb,
    org: &str,
    now: DateTime<Utc>,
) -> std::result::Result<Vec<DayDoc>, FirestoreError> {
    let parent = db.parent_path(SPEND_ORGS, hex(org))?;
    let month_start = SpendPeriod::Month
        .window_start(now)
        .map(|start| day_id(start.date_naive()))
        .unwrap_or_default();
    let today = day_id(now.date_naive());
    let days: Vec<DayDoc> = db
        .fluent()
        .select()
        .from(SPEND_DAYS)
        .parent(&parent)
        .filter(|q| q.for_all([q.field("day").greater_than_or_equal(month_start.as_str())]))
        .obj::<DayDoc>()
        .query()
        .await?;
    Ok(days.into_iter().filter(|day| day.day <= today).collect())
}

async fn read_holds(
    db: &FirestoreDb,
    org: &str,
) -> std::result::Result<Vec<EntryDoc>, FirestoreError> {
    db.fluent()
        .select()
        .from(SPEND_ENTRIES)
        .filter(|q| q.for_all([q.field("org").eq(org), q.field("state").eq(STATE_RESERVED)]))
        .obj::<EntryDoc>()
        .query()
        .await
}

/// Firestore-backed [`SpendLedger`].
///
/// See the [module documentation](self) for the data model.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
/// use adk_session::FirestoreSpendLedger;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let ledger = FirestoreSpendLedger::new("my-gcp-project")
///     .await?
///     .with_limits(
///         SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
///     );
/// ledger.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreSpendLedger {
    db: FirestoreDb,
    limits: SpendLimits,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FirestoreSpendLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreSpendLedger")
            .field("limits", &self.limits)
            .field("ttl", &self.ttl)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl FirestoreSpendLedger {
    /// Connects to `project_id` with Application Default Credentials, no limits, and
    /// [`DEFAULT_RESERVATION_TTL`]. Honors `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await.map_err(spend_unavailable)?))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self {
            db,
            limits: SpendLimits::new(),
            ttl: DEFAULT_RESERVATION_TTL,
            clock: Arc::new(SystemClock),
        }
    }

    /// Sets the limits every reservation is checked against.
    pub fn with_limits(mut self, limits: SpendLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets how long an unsettled reservation holds budget.
    pub fn with_reservation_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Sets the clock that picks period windows and expires reservations.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Prepares the ledger's storage.
    ///
    /// Firestore creates collections on first write and serves every query the ledger
    /// makes from automatic single-field indexes, so there is nothing to create; the
    /// method exists so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    /// Settles a reserved entry as `state`, recording `committed` when it is a commit.
    async fn settle(&self, id: ReservationId, committed: Option<i64>) -> Result<()> {
        let now = self.clock.now();
        let id_text = id.as_str().to_string();
        let settled = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, bool> {
                    let id_text = id_text.clone();
                    Box::pin(async move {
                        settle_in_transaction(&db, transaction, &id_text, committed, now).await
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, spend_unavailable))?;
        if settled { Ok(()) } else { Err(SpendError::UnknownReservation(id).into()) }
    }
}

async fn settle_in_transaction(
    db: &FirestoreDb,
    transaction: &mut FirestoreTransaction<'_>,
    id: &str,
    committed: Option<i64>,
    now: DateTime<Utc>,
) -> TxResult<bool> {
    let fail = |e: FirestoreError| spend_unavailable(e);
    let entry: Option<EntryDoc> = db
        .fluent()
        .select()
        .by_id_in(SPEND_ENTRIES)
        .obj::<EntryDoc>()
        .one(id)
        .await
        .map_err(|e| retry_or_fail(e, fail))?;
    let Some(entry) = entry.filter(|entry| entry.state == STATE_RESERVED) else {
        return Ok(false);
    };
    let settled = EntryDoc {
        state: if committed.is_some() { STATE_COMMITTED } else { STATE_RELEASED }.to_string(),
        committed_micro_usd: committed,
        settled_at_us: Some(now.timestamp_micros()),
        ..entry.clone()
    };

    let mut writes = Vec::new();
    if let Some(amount) = committed {
        let mut org = read_org(db, &entry.org).await.map_err(|e| retry_or_fail(e, fail))?;
        let parent =
            db.parent_path(SPEND_ORGS, hex(&entry.org)).map_err(|e| retry_or_fail(e, fail))?;
        let today = day_id(now.date_naive());
        let mut day: DayDoc = db
            .fluent()
            .select()
            .by_id_in(SPEND_DAYS)
            .parent(&parent)
            .obj::<DayDoc>()
            .one(&today)
            .await
            .map_err(|e| retry_or_fail(e, fail))?
            .unwrap_or_else(|| DayDoc { day: today.clone(), totals: Vec::new() });
        add_to_totals(&mut day.totals, &entry.key(), amount);
        add_to_totals(&mut org.lifetime, &entry.key(), amount);
        writes.push((parent, today, day, org));
    }

    db.fluent()
        .update()
        .in_col(SPEND_ENTRIES)
        .precondition(FirestoreWritePrecondition::Exists(true))
        .document_id(id)
        .object(&settled)
        .add_to_transaction(transaction)
        .map_err(|e| retry_or_fail(e, fail))?;
    for (parent, today, day, org) in &writes {
        db.fluent()
            .update()
            .in_col(SPEND_DAYS)
            .document_id(today)
            .parent(parent)
            .object(day)
            .add_to_transaction(transaction)
            .map_err(|e| retry_or_fail(e, fail))?;
        db.fluent()
            .update()
            .in_col(SPEND_ORGS)
            .document_id(hex(&org.org))
            .object(org)
            .add_to_transaction(transaction)
            .map_err(|e| retry_or_fail(e, fail))?;
    }
    Ok(true)
}

#[async_trait]
impl SpendLedger for FirestoreSpendLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let amount = stored_amount(amount_micro_usd)?;
        let now = self.clock.now();
        let ttl = i64::try_from(self.ttl.as_micros()).unwrap_or(i64::MAX);
        let request = Arc::new((key.clone(), self.limits.clone()));
        let id = ReservationId::generate();
        let id_text = id.as_str().to_string();
        let refused = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<AdkError>> {
                    let request = request.clone();
                    let id_text = id_text.clone();
                    Box::pin(async move {
                        let (key, limits) = &*request;
                        let fail = |e: FirestoreError| spend_unavailable(e);
                        let snapshot = OrgSnapshot {
                            org: read_org(&db, &key.org)
                                .await
                                .map_err(|e| retry_or_fail(e, fail))?,
                            month: read_month(&db, &key.org, now)
                                .await
                                .map_err(|e| retry_or_fail(e, fail))?,
                            holds: read_holds(&db, &key.org)
                                .await
                                .map_err(|e| retry_or_fail(e, fail))?,
                        };
                        let now_us = now.timestamp_micros();
                        let today = day_id(now.date_naive());
                        for limit in limits.applicable(key) {
                            let consumed = snapshot
                                .committed(&limit.key, limit.key.period, &today)
                                .saturating_add(snapshot.held(&limit.key, now_us));
                            if consumed.saturating_add(amount_micro_usd) > limit.max_micro_usd {
                                return Ok(Some(
                                    SpendError::LimitExceeded {
                                        limit: limit.key.clone(),
                                        max_micro_usd: limit.max_micro_usd,
                                        consumed_micro_usd: consumed,
                                        requested_micro_usd: amount_micro_usd,
                                    }
                                    .into(),
                                ));
                            }
                        }
                        let entry = EntryDoc {
                            org: key.org.clone(),
                            agent: key.agent.clone(),
                            vendor: key.vendor.clone(),
                            state: STATE_RESERVED.to_string(),
                            reserved_micro_usd: amount,
                            committed_micro_usd: None,
                            created_at_us: now_us,
                            expires_at_us: now_us.saturating_add(ttl),
                            settled_at_us: None,
                        };
                        // Every reservation for the organization writes its document, so
                        // two that read the same totals conflict and one retries.
                        let org =
                            OrgDoc { seq: snapshot.org.seq.saturating_add(1), ..snapshot.org };
                        db.fluent()
                            .update()
                            .in_col(SPEND_ENTRIES)
                            .precondition(FirestoreWritePrecondition::Exists(false))
                            .document_id(&id_text)
                            .object(&entry)
                            .add_to_transaction(transaction)
                            .map_err(|e| retry_or_fail(e, fail))?;
                        db.fluent()
                            .update()
                            .in_col(SPEND_ORGS)
                            .document_id(hex(&key.org))
                            .object(&org)
                            .add_to_transaction(transaction)
                            .map_err(|e| retry_or_fail(e, fail))?;
                        Ok(None)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, spend_unavailable))?;
        match refused {
            Some(error) => Err(error),
            None => Ok(id),
        }
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let amount = stored_amount(actual_micro_usd)?;
        self.settle(id, Some(amount)).await
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        self.settle(id, None).await
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        let now = self.clock.now();
        let today = day_id(now.date_naive());
        let totals: Vec<AttributedTotal> = match period {
            SpendPeriod::Lifetime => {
                read_org(&self.db, &key.org).await.map_err(spend_unavailable)?.lifetime
            }
            SpendPeriod::Month | SpendPeriod::Day => read_month(&self.db, &key.org, now)
                .await
                .map_err(spend_unavailable)?
                .into_iter()
                .filter(|day| period == SpendPeriod::Month || day.day == today)
                .flat_map(|day| day.totals)
                .collect(),
        };
        Ok(covered_total(&key.org, key, &totals))
    }
}

// ---------------------------------------------------------------------------
// Action ledger
// ---------------------------------------------------------------------------

fn action_error(message: String) -> AdkError {
    storage_error("session.action_ledger.storage", message)
}

/// One ledgered call. The record is stored as JSON text so its timestamps are exact.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActionDoc {
    key: String,
    record: String,
}

/// SHA-256 of `value`'s JSON text, in hex: path-safe, fixed length, and collision
/// resistant, so a crafted key cannot occupy another call's document.
fn document_id(value: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;

    Sha256::digest(value.to_string().as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut id, byte| {
            let _ = write!(id, "{byte:02x}");
            id
        },
    )
}

fn action_id(key: &str) -> String {
    document_id(&json!(key))
}

/// Firestore-backed [`ActionLedger`], so a non-idempotent tool call is never repeated
/// across a process restart.
///
/// Each record stores the idempotency key, tool name, argument digest, effect, start
/// time, and outcome — never the arguments or result themselves. See the
/// [module documentation](self) for the data model.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::FirestoreActionLedger;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let ledger = FirestoreActionLedger::new("my-gcp-project").await?;
/// ledger.migrate().await?;
/// let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreActionLedger {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreActionLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreActionLedger").finish_non_exhaustive()
    }
}

impl FirestoreActionLedger {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.action_ledger.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await.map_err(action_error)?))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Prepares the ledger's storage.
    ///
    /// Firestore creates collections on first write and the ledger reads documents only
    /// by ID, so there is nothing to create; the method exists so every backend is set
    /// up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ActionLedger for FirestoreActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let doc = ActionDoc { key: record.key.clone(), record: to_json(record, action_error)? };
        let id = action_id(&record.key);
        let mut attempt = 0;
        loop {
            let created = self
                .db
                .fluent()
                .insert()
                .into(ACTIONS)
                .document_id(&id)
                .object(&doc)
                .execute::<ActionDoc>()
                .await;
            let error = match created {
                Ok(_) => return Ok(()),
                Err(FirestoreError::DataConflictError(_)) => {
                    return Err(duplicate_key_error(&record.key));
                }
                Err(error) => error,
            };
            // A contended create can fail without saying whether another writer won, so
            // the document decides: present means this call must not run.
            let existing: Option<ActionDoc> = self
                .db
                .fluent()
                .select()
                .by_id_in(ACTIONS)
                .obj::<ActionDoc>()
                .one(&id)
                .await
                .map_err(|e| action_error(format!("action ledger begin failed: {e}")))?;
            if existing.is_some() {
                return Err(duplicate_key_error(&record.key));
            }
            attempt += 1;
            if attempt >= 5 {
                return Err(action_error(format!("action ledger begin failed: {error}")));
            }
            tokio::time::sleep(std::time::Duration::from_millis(20 * attempt)).await;
        }
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let now = Utc::now();
        let request = Arc::new((key.to_string(), outcome));
        let result = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<AdkError>> {
                    let request = request.clone();
                    Box::pin(async move {
                        let (key, outcome) = &*request;
                        let fail = |e: FirestoreError| {
                            action_error(format!("action ledger complete failed: {e}"))
                        };
                        let id = action_id(key);
                        let stored: Option<ActionDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(ACTIONS)
                            .obj::<ActionDoc>()
                            .one(&id)
                            .await
                            .map_err(|e| retry_or_fail(e, fail))?;
                        let Some(stored) = stored.filter(|doc| doc.key == *key) else {
                            return Ok(Some(missing_key_error(key)));
                        };
                        let mut record: ActionRecord =
                            from_json(&stored.record, "action record", action_error)
                                .map_err(BackoffError::permanent)?;
                        if record.outcome.is_some() {
                            return Ok(Some(already_completed_error(key)));
                        }
                        record.outcome = Some(outcome.clone());
                        record.completed_at = Some(now);
                        let doc = ActionDoc {
                            key: key.clone(),
                            record: to_json(&record, action_error)
                                .map_err(BackoffError::permanent)?,
                        };
                        db.fluent()
                            .update()
                            .in_col(ACTIONS)
                            .precondition(FirestoreWritePrecondition::Exists(true))
                            .document_id(&id)
                            .object(&doc)
                            .add_to_transaction(transaction)
                            .map_err(|e| retry_or_fail(e, fail))?;
                        Ok(None)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, action_error))?;
        match result {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        let stored: Option<ActionDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(ACTIONS)
            .obj::<ActionDoc>()
            .one(action_id(key))
            .await
            .map_err(|e| action_error(format!("action ledger read failed: {e}")))?;
        stored
            .filter(|doc| doc.key == key)
            .map(|doc| from_json(&doc.record, "action record", action_error))
            .transpose()
    }
}

// ---------------------------------------------------------------------------
// Approval store
// ---------------------------------------------------------------------------

fn approval_error(message: String) -> AdkError {
    storage_error("session.approval_store.storage", message)
}

/// A pending request or a decision for one fingerprint in one scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApprovalDoc {
    app_name: String,
    user_id: String,
    session_id: String,
    kind: String,
    fingerprint: String,
    /// JSON of a [`PendingApproval`] or a [`ToolApproval`].
    payload: String,
}

impl ApprovalDoc {
    fn is_for(&self, scope: &ApprovalScope, kind: &str, fingerprint: &str) -> bool {
        self.app_name == scope.app_name
            && self.user_id == scope.user_id
            && self.session_id == scope.session_id
            && self.kind == kind
            && self.fingerprint == fingerprint
    }
}

fn approval_id(scope: &ApprovalScope, kind: &str, fingerprint: &str) -> String {
    document_id(&json!([scope.app_name, scope.user_id, scope.session_id, kind, fingerprint]))
}

fn digest_collision(fingerprint: &str) -> AdkError {
    approval_error(format!(
        "approval document for fingerprint '{fingerprint}' holds a different call with the same \
         digest; the decision was not recorded"
    ))
}

/// An [`ApprovalRequest`] with the fields its queries filter on.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequestDoc {
    id: String,
    app: String,
    state: String,
    open: bool,
    created_us: i64,
    version: i64,
    /// JSON of the [`ApprovalRequest`].
    record: String,
}

impl RequestDoc {
    fn new(request: &ApprovalRequest) -> Result<Self> {
        Ok(Self {
            id: request.id.clone(),
            app: request.scope.app_name.clone(),
            state: request.state.as_str().to_string(),
            open: request.is_open(),
            created_us: request.created_at.timestamp_micros(),
            version: i64::try_from(request.version).map_err(|_| {
                approval_error(format!(
                    "approval request version {} is out of range",
                    request.version
                ))
            })?,
            record: to_json(request, approval_error)?,
        })
    }

    fn request(&self) -> Result<ApprovalRequest> {
        from_json(&self.record, "approval request", approval_error)
    }
}

/// An [`ApprovalDelegation`] with its delegate as query fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DelegationDoc {
    id: String,
    to_kind: String,
    to_id: String,
    /// JSON of the [`ApprovalDelegation`].
    record: String,
}

fn subject_kind(subject: &SubjectRef) -> Result<String> {
    match serde_json::to_value(subject.kind) {
        Ok(serde_json::Value::String(kind)) => Ok(kind),
        _ => Err(approval_error("could not encode a subject kind".to_string())),
    }
}

fn request_doc_id(id: &str) -> String {
    document_id(&json!(["request", id]))
}

fn delegation_doc_id(id: &str) -> String {
    document_id(&json!(["delegation", id]))
}

/// Firestore-backed [`ApprovalStore`].
///
/// Pending requests and decisions survive restarts and are shared by every process that
/// uses the same database. See the [module documentation](self) for the data model.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::FirestoreApprovalStore;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = FirestoreApprovalStore::new("my-gcp-project").await?;
/// store.migrate().await?;
/// let config = RunConfig::builder().approval_store(Arc::new(store)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreApprovalStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreApprovalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreApprovalStore").finish_non_exhaustive()
    }
}

impl FirestoreApprovalStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.approval_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await.map_err(approval_error)?))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Prepares the store's storage.
    ///
    /// Firestore creates collections on first write and serves the pending-list query
    /// from automatic single-field indexes, so there is nothing to create; the method
    /// exists so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    async fn read(
        &self,
        scope: &ApprovalScope,
        kind: &str,
        fingerprint: &str,
    ) -> Result<Option<ApprovalDoc>> {
        let stored: Option<ApprovalDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(APPROVALS)
            .obj::<ApprovalDoc>()
            .one(approval_id(scope, kind, fingerprint))
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        Ok(stored.filter(|doc| doc.is_for(scope, kind, fingerprint)))
    }

    async fn read_request(&self, id: &str) -> Result<Option<RequestDoc>> {
        let stored: Option<RequestDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(APPROVAL_REQUESTS)
            .obj::<RequestDoc>()
            .one(request_doc_id(id))
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        Ok(stored.filter(|doc| doc.id == id))
    }

    async fn read_delegation(&self, id: &str) -> Result<Option<DelegationDoc>> {
        let stored: Option<DelegationDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(APPROVAL_DELEGATIONS)
            .obj::<DelegationDoc>()
            .one(delegation_doc_id(id))
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        Ok(stored.filter(|doc| doc.id == id))
    }
}

#[async_trait]
impl ApprovalStore for FirestoreApprovalStore {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let fingerprint = request.fingerprint();
        let entry = PendingApproval {
            fingerprint: fingerprint.clone(),
            request: request.clone(),
            requested_at: Utc::now(),
        };
        let doc = ApprovalDoc {
            app_name: scope.app_name.clone(),
            user_id: scope.user_id.clone(),
            session_id: scope.session_id.clone(),
            kind: KIND_PENDING.to_string(),
            fingerprint: fingerprint.clone(),
            payload: to_json(&entry, approval_error)?,
        };
        let created = self
            .db
            .fluent()
            .insert()
            .into(APPROVALS)
            .document_id(approval_id(scope, KIND_PENDING, &fingerprint))
            .object(&doc)
            .execute::<ApprovalDoc>()
            .await;
        match created {
            Ok(_) => Ok(()),
            // Already pending, or decided since the create was attempted: the original
            // request and time are kept either way.
            Err(FirestoreError::DataConflictError(_)) => Ok(()),
            Err(error) => Err(approval_error(format!("approval store write failed: {error}"))),
        }
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        let docs: Vec<ApprovalDoc> = self
            .db
            .fluent()
            .select()
            .from(APPROVALS)
            .filter(|q| {
                q.for_all([
                    q.field("app_name").eq(scope.app_name.as_str()),
                    q.field("user_id").eq(scope.user_id.as_str()),
                    q.field("session_id").eq(scope.session_id.as_str()),
                    q.field("kind").eq(KIND_PENDING),
                ])
            })
            .obj::<ApprovalDoc>()
            .query()
            .await
            .map_err(|e| approval_error(format!("approval store query failed: {e}")))?;
        let mut pending = docs
            .iter()
            .filter(|doc| doc.is_for(scope, KIND_PENDING, &doc.fingerprint))
            .map(|doc| {
                from_json::<PendingApproval>(&doc.payload, "pending approval", approval_error)
            })
            .collect::<Result<Vec<_>>>()?;
        pending.sort_by_key(|entry| entry.requested_at);
        Ok(pending)
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        let decision = ApprovalDoc {
            app_name: scope.app_name.clone(),
            user_id: scope.user_id.clone(),
            session_id: scope.session_id.clone(),
            kind: KIND_DECISION.to_string(),
            fingerprint: fingerprint.to_string(),
            payload: to_json(&approval, approval_error)?,
        };
        let request = Arc::new((scope.clone(), decision));
        let refused = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<AdkError>> {
                    let request = request.clone();
                    Box::pin(async move {
                        let (scope, decision) = &*request;
                        let fail = |e: FirestoreError| {
                            approval_error(format!("approval store decide failed: {e}"))
                        };
                        let fingerprint = decision.fingerprint.as_str();
                        let decision_id = approval_id(scope, KIND_DECISION, fingerprint);
                        let pending_id = approval_id(scope, KIND_PENDING, fingerprint);
                        let existing: Option<ApprovalDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(APPROVALS)
                            .obj::<ApprovalDoc>()
                            .one(&decision_id)
                            .await
                            .map_err(|e| retry_or_fail(e, fail))?;
                        if existing
                            .is_some_and(|doc| !doc.is_for(scope, KIND_DECISION, fingerprint))
                        {
                            return Ok(Some(digest_collision(fingerprint)));
                        }
                        let pending: Option<ApprovalDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(APPROVALS)
                            .obj::<ApprovalDoc>()
                            .one(&pending_id)
                            .await
                            .map_err(|e| retry_or_fail(e, fail))?;
                        if pending.is_some_and(|doc| doc.is_for(scope, KIND_PENDING, fingerprint)) {
                            db.fluent()
                                .delete()
                                .from(APPROVALS)
                                .document_id(&pending_id)
                                .add_to_transaction(transaction)
                                .map_err(|e| retry_or_fail(e, fail))?;
                        }
                        db.fluent()
                            .update()
                            .in_col(APPROVALS)
                            .document_id(&decision_id)
                            .object(decision)
                            .add_to_transaction(transaction)
                            .map_err(|e| retry_or_fail(e, fail))?;
                        Ok(None)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, approval_error))?;
        match refused {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolConfirmationDecision>> {
        Ok(self.take_approval(scope, fingerprint).await?.map(|approval| approval.decision))
    }

    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolApproval>> {
        let request = Arc::new((scope.clone(), fingerprint.to_string()));
        let taken = self
            .db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<ToolApproval>> {
                    let request = request.clone();
                    Box::pin(async move {
                        let (scope, fingerprint) = &*request;
                        let fail = |e: FirestoreError| {
                            approval_error(format!("approval store take failed: {e}"))
                        };
                        let id = approval_id(scope, KIND_DECISION, fingerprint);
                        let stored: Option<ApprovalDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(APPROVALS)
                            .obj::<ApprovalDoc>()
                            .one(&id)
                            .await
                            .map_err(|e| retry_or_fail(e, fail))?;
                        let Some(stored) =
                            stored.filter(|doc| doc.is_for(scope, KIND_DECISION, fingerprint))
                        else {
                            return Ok(None);
                        };
                        let approval: ToolApproval =
                            from_json(&stored.payload, "decision", approval_error)
                                .map_err(BackoffError::permanent)?;
                        db.fluent()
                            .delete()
                            .from(APPROVALS)
                            .document_id(&id)
                            .precondition(FirestoreWritePrecondition::Exists(true))
                            .add_to_transaction(transaction)
                            .map_err(|e| retry_or_fail(e, fail))?;
                        Ok(Some(approval))
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, approval_error))?;
        Ok(taken.filter(|approval| approval.is_live(Utc::now())))
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        let Some(stored) = self.read(scope, KIND_DECISION, fingerprint).await? else {
            return Ok(false);
        };
        let approval: ToolApproval = from_json(&stored.payload, "decision", approval_error)?;
        Ok(approval.is_live(Utc::now()))
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let doc = RequestDoc::new(request)?;
        let created = self
            .db
            .fluent()
            .insert()
            .into(APPROVAL_REQUESTS)
            .document_id(request_doc_id(&request.id))
            .object(&doc)
            .execute::<RequestDoc>()
            .await;
        match created {
            Ok(_) => Ok(request.clone()),
            Err(FirestoreError::DataConflictError(_)) => {
                match self.read_request(&request.id).await? {
                    Some(existing) => existing.request(),
                    None => Err(approval_error(format!(
                        "approval request document for '{}' holds a different request with the same \
                     digest",
                        request.id
                    ))),
                }
            }
            Err(error) => Err(approval_error(format!("approval store write failed: {error}"))),
        }
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.read_request(id).await?.map(|doc| doc.request()).transpose()
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        let doc = RequestDoc::new(request)?;
        let Ok(expected) = i64::try_from(expected_version) else {
            return Ok(false);
        };
        let shared = Arc::new(doc);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, bool> {
                    let doc = shared.clone();
                    Box::pin(async move {
                        let fail = |e: FirestoreError| {
                            approval_error(format!("approval store replace failed: {e}"))
                        };
                        let doc_id = request_doc_id(&doc.id);
                        let stored: Option<RequestDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(APPROVAL_REQUESTS)
                            .obj::<RequestDoc>()
                            .one(&doc_id)
                            .await
                            .map_err(|e| retry_or_fail(e, fail))?;
                        if !stored
                            .is_some_and(|stored| stored.id == doc.id && stored.version == expected)
                        {
                            return Ok(false);
                        }
                        db.fluent()
                            .update()
                            .in_col(APPROVAL_REQUESTS)
                            .document_id(&doc_id)
                            .object(&*doc)
                            .add_to_transaction(transaction)
                            .map_err(|e| retry_or_fail(e, fail))?;
                        Ok(true)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(|e| transaction_error(e, approval_error))
    }

    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        let single_state = match query.states.as_slice() {
            [state] => Some(state.as_str()),
            _ => None,
        };
        let app = query.app_name.clone();
        let open_only = query.open_only;
        let docs: Vec<RequestDoc> = self
            .db
            .fluent()
            .select()
            .from(APPROVAL_REQUESTS)
            .filter(move |q| {
                q.for_all([
                    app.as_deref().and_then(|app| q.field("app").eq(app)),
                    open_only.then(|| q.field("open").eq(true)).flatten(),
                    single_state.and_then(|state| q.field("state").eq(state)),
                ])
            })
            .obj::<RequestDoc>()
            .query()
            .await
            .map_err(|e| approval_error(format!("approval store query failed: {e}")))?;
        let mut requests = Vec::new();
        for doc in docs {
            let request = doc.request()?;
            if query.matches(&request) {
                requests.push(request);
            }
        }
        requests.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        requests.truncate(query.limit);
        Ok(requests)
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        let doc = DelegationDoc {
            id: delegation.id.clone(),
            to_kind: subject_kind(&delegation.to)?,
            to_id: delegation.to.id.clone(),
            record: to_json(delegation, approval_error)?,
        };
        self.db
            .fluent()
            .update()
            .in_col(APPROVAL_DELEGATIONS)
            .document_id(delegation_doc_id(&delegation.id))
            .object(&doc)
            .execute::<DelegationDoc>()
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(())
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        let kind = subject_kind(to)?;
        let docs: Vec<DelegationDoc> = self
            .db
            .fluent()
            .select()
            .from(APPROVAL_DELEGATIONS)
            .filter(|q| {
                q.for_all([
                    q.field("to_kind").eq(kind.as_str()),
                    q.field("to_id").eq(to.id.as_str()),
                ])
            })
            .obj::<DelegationDoc>()
            .query()
            .await
            .map_err(|e| approval_error(format!("approval store query failed: {e}")))?;
        let mut delegations = docs
            .iter()
            .filter(|doc| doc.to_kind == kind && doc.to_id == to.id)
            .map(|doc| from_json::<ApprovalDelegation>(&doc.record, "delegation", approval_error))
            .collect::<Result<Vec<_>>>()?;
        delegations.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(delegations)
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        if self.read_delegation(id).await?.is_none() {
            return Ok(false);
        }
        match self
            .db
            .fluent()
            .delete()
            .from(APPROVAL_DELEGATIONS)
            .document_id(delegation_doc_id(id))
            .precondition(FirestoreWritePrecondition::Exists(true))
            .execute()
            .await
        {
            Ok(()) => Ok(true),
            // Deleted concurrently between the read and the delete.
            Err(FirestoreError::DataNotFoundError(_)) => Ok(false),
            Err(error) => Err(approval_error(format!("approval store delete failed: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(org: &str) -> OrgSnapshot {
        let total = |agent: Option<&str>, vendor: Option<&str>, micro_usd| AttributedTotal {
            agent: agent.map(str::to_string),
            vendor: vendor.map(str::to_string),
            micro_usd,
        };
        OrgSnapshot {
            org: OrgDoc {
                org: org.to_string(),
                seq: 3,
                lifetime: vec![total(Some("a"), Some("v"), 500), total(None, None, 7)],
            },
            month: vec![
                DayDoc { day: "2026-10-01".into(), totals: vec![total(Some("a"), Some("v"), 40)] },
                DayDoc { day: "2026-10-15".into(), totals: vec![total(Some("a"), None, 5)] },
            ],
            holds: vec![
                EntryDoc {
                    org: org.to_string(),
                    agent: Some("a".into()),
                    vendor: Some("v".into()),
                    state: STATE_RESERVED.into(),
                    reserved_micro_usd: 11,
                    committed_micro_usd: None,
                    created_at_us: 0,
                    expires_at_us: 100,
                    settled_at_us: None,
                },
                EntryDoc {
                    org: org.to_string(),
                    agent: Some("b".into()),
                    vendor: None,
                    state: STATE_RESERVED.into(),
                    reserved_micro_usd: 13,
                    committed_micro_usd: None,
                    created_at_us: 0,
                    expires_at_us: 10,
                    settled_at_us: None,
                },
            ],
        }
    }

    #[test]
    fn windows_sum_the_covered_totals() {
        let snapshot = snapshot("acme");
        let org = SpendKey::org("acme");
        let vendor = SpendKey::org("acme").with_vendor("v");
        assert_eq!(snapshot.committed(&org, SpendPeriod::Day, "2026-10-15"), 5);
        assert_eq!(snapshot.committed(&org, SpendPeriod::Month, "2026-10-15"), 45);
        assert_eq!(snapshot.committed(&org, SpendPeriod::Lifetime, "2026-10-15"), 507);
        assert_eq!(snapshot.committed(&vendor, SpendPeriod::Month, "2026-10-15"), 40);
        assert_eq!(snapshot.committed(&SpendKey::org("other"), SpendPeriod::Lifetime, ""), 0);
    }

    #[test]
    fn only_live_covered_holds_count() {
        let snapshot = snapshot("acme");
        assert_eq!(snapshot.held(&SpendKey::org("acme"), 5), 24);
        assert_eq!(snapshot.held(&SpendKey::org("acme"), 50), 11, "the second hold expired");
        assert_eq!(snapshot.held(&SpendKey::org("acme").with_agent("b"), 50), 0);
    }

    #[test]
    fn totals_accumulate_per_attribution() {
        let mut totals = Vec::new();
        let key = SpendKey::org("acme").with_agent("a");
        add_to_totals(&mut totals, &key, 10);
        add_to_totals(&mut totals, &key, 5);
        add_to_totals(&mut totals, &SpendKey::org("acme"), 1);
        assert_eq!(
            totals,
            vec![
                AttributedTotal { agent: Some("a".into()), vendor: None, micro_usd: 15 },
                AttributedTotal { agent: None, vendor: None, micro_usd: 1 },
            ]
        );
    }

    #[test]
    fn document_ids_are_path_safe_and_scope_bound() {
        let scope = ApprovalScope::new("app/1", "user:1", "session/x");
        let id = approval_id(&scope, KIND_DECISION, "transfer({\"a\":1})");
        assert!(!id.contains('/'));
        assert_ne!(id, approval_id(&scope, KIND_PENDING, "transfer({\"a\":1})"));
        assert!(!action_id("app/user/session/inv/call").contains('/'));
        assert_eq!(hex("a/b"), "612f62");
        let doc = ApprovalDoc {
            app_name: "app/1".into(),
            user_id: "user:1".into(),
            session_id: "session/x".into(),
            kind: KIND_DECISION.into(),
            fingerprint: "f".into(),
            payload: String::new(),
        };
        assert!(doc.is_for(&scope, KIND_DECISION, "f"));
        assert!(!doc.is_for(&scope, KIND_DECISION, "g"), "a digest collision does not match");
        assert!(!doc.is_for(&ApprovalScope::new("app/1", "user:1", "other"), KIND_DECISION, "f"));
    }

    #[test]
    fn day_ids_sort_by_date() {
        let early = day_id(NaiveDate::from_ymd_opt(2026, 9, 30).expect("date"));
        let late = day_id(NaiveDate::from_ymd_opt(2026, 10, 1).expect("date"));
        assert_eq!(early, "2026-09-30");
        assert!(early < late);
    }
}
