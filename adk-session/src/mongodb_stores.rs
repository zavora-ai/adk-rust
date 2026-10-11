//! MongoDB-backed [`SpendLedger`](adk_core::SpendLedger), [`ActionLedger`](adk_core::ActionLedger), and
//! [`ApprovalStore`](adk_core::ApprovalStore).
//!
//! All three work on a standalone `mongod` as well as on a replica set or sharded
//! cluster: none of them needs a multi-document transaction.
//!
//! | Store | Collections | How writes stay atomic |
//! |-------|-------------|------------------------|
//! | [`MongoSpendLedger`] | `adk_spend_orgs`, `adk_spend_entries` | Compare-and-set on the organization's `seq`, which also parks the new hold in `pending` until its entry is written |
//! | [`MongoActionLedger`] | `adk_action_ledger` | `insert_one` on `_id` = idempotency key; `update_one` filtered on `outcome: null` |
//! | [`MongoApprovalStore`] | `adk_approvals`, `adk_approval_requests`, `adk_approval_delegations` | `find_one_and_delete` takes a decision; upserts with `$setOnInsert` record pending calls; `insert_one` on `_id` creates a request once; `update_one` filtered on `version` changes it once per version |
//!
//! # Spend reservations
//!
//! Every reservation for an organization claims the organization document with one
//! conditional update, `{_id: org, seq: <read>, pending: null}` →
//! `{$inc: {seq: 1}, $set: {pending: <hold>}}`. The limit check runs between reading
//! `seq` and that claim, so a claim succeeds only when no other reservation landed in
//! between. The winner then writes its entry and clears `pending`. A reservation that
//! finds `pending` set writes that entry itself first, so a process that dies between
//! the claim and the entry write leaves a hold that expires after its time to live
//! rather than one that is lost. Entries are written only for granted holds, so a
//! losing reservation never makes another one refuse. Commits and releases update the
//! entry alone, filtered on `state: "reserved"`.

use std::sync::Arc;
use std::time::Duration;

use adk_core::principal::SubjectRef;
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, ApprovalDelegation, ApprovalRequest,
    ApprovalRequestQuery, ApprovalScope, ApprovalStore, Clock, DEFAULT_RESERVATION_TTL,
    ErrorCategory, ErrorComponent, PendingApproval, ReservationId, Result, SpendError, SpendKey,
    SpendLedger, SpendLimits, SpendPeriod, SystemClock, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest, ToolEffect,
    action_ledger::{already_completed_error, duplicate_key_error, missing_key_error},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mongodb::bson::{Bson, Document, doc};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, FindOptions, IndexOptions, UpdateOptions};
use mongodb::{Client, Collection, Database, IndexModel};

const SPEND_ORGS: &str = "adk_spend_orgs";
const SPEND_ENTRIES: &str = "adk_spend_entries";
const ACTION_LEDGER: &str = "adk_action_ledger";
const APPROVALS: &str = "adk_approvals";
const APPROVAL_REQUESTS: &str = "adk_approval_requests";
const APPROVAL_DELEGATIONS: &str = "adk_approval_delegations";

const STATE_RESERVED: &str = "reserved";
const STATE_COMMITTED: &str = "committed";
const STATE_RELEASED: &str = "released";

/// Claims attempted before a reservation gives up under contention.
const MAX_CLAIM_ATTEMPTS: u32 = 200;

/// MongoDB's duplicate key error code.
const DUPLICATE_KEY: i32 = 11000;

async fn connect(connection_string: &str, database_name: &str) -> mongodb::error::Result<Database> {
    let options = ClientOptions::parse(connection_string).await?;
    Ok(Client::with_options(options)?.database(database_name))
}

fn is_duplicate_key(error: &mongodb::error::Error) -> bool {
    match error.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(write)) => write.code == DUPLICATE_KEY,
        ErrorKind::Command(command) => command.code == DUPLICATE_KEY,
        _ => false,
    }
}

fn upsert() -> UpdateOptions {
    UpdateOptions::builder().upsert(true).build()
}

/// Reads an integer field the server may return as `Int32`, `Int64`, or `Double`.
fn read_i64(document: &Document, field: &str) -> Option<i64> {
    match document.get(field)? {
        Bson::Int32(value) => Some(i64::from(*value)),
        Bson::Int64(value) => Some(*value),
        Bson::Double(value) => Some(*value as i64),
        _ => None,
    }
}

fn read_str<'a>(document: &'a Document, field: &str) -> Option<&'a str> {
    document.get_str(field).ok()
}

fn optional(value: Option<&str>) -> Bson {
    value.map_or(Bson::Null, |value| Bson::String(value.to_string()))
}

// ---------------------------------------------------------------------------
// Spend ledger
// ---------------------------------------------------------------------------

fn unavailable(error: impl std::fmt::Display) -> AdkError {
    SpendError::Unavailable(error.to_string()).into()
}

fn stored_amount(amount_micro_usd: u64) -> Result<i64> {
    i64::try_from(amount_micro_usd).map_err(|_| SpendError::AmountTooLarge(amount_micro_usd).into())
}

fn window_start_micros(period: SpendPeriod, now: DateTime<Utc>) -> i64 {
    period.window_start(now).map_or(i64::MIN, |start| start.timestamp_micros())
}

/// Adds the scope fields a key names to an entry filter; `None` matches any value.
fn scope_filter(filter: &mut Document, key: &SpendKey) {
    filter.insert("org", key.org.as_str());
    if let Some(agent) = &key.agent {
        filter.insert("agent", agent.as_str());
    }
    if let Some(vendor) = &key.vendor {
        filter.insert("vendor", vendor.as_str());
    }
}

/// Small randomized pause between claim attempts, growing with the attempt number.
fn claim_backoff(attempt: u32) -> Duration {
    let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 5;
    Duration::from_millis(u64::from(attempt.min(20)) + jitter)
}

/// MongoDB-backed [`SpendLedger`].
///
/// Works on standalone servers and replica sets; see the
/// [module documentation](crate::mongodb_stores) for how reservations serialize.
/// Amounts are stored as 64-bit integers, so an amount above `i64::MAX` micro-USD is
/// refused with `spend.amount_too_large`.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
/// use adk_session::MongoSpendLedger;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let ledger = MongoSpendLedger::new("mongodb://localhost:27017", "adk")
///     .await?
///     .with_limits(
///         SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
///     );
/// ledger.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoSpendLedger {
    db: Database,
    limits: SpendLimits,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl MongoSpendLedger {
    /// Connects to `connection_string` and uses `database_name`, with no limits and
    /// [`DEFAULT_RESERVATION_TTL`].
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(
            connect(connection_string, database_name).await.map_err(unavailable)?,
        ))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
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

    /// Creates the entry index the limit checks read through.
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        self.entries()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "org": 1, "state": 1 })
                    .options(
                        IndexOptions::builder()
                            .name("adk_spend_entries_org_state".to_string())
                            .build(),
                    )
                    .build(),
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    fn orgs(&self) -> Collection<Document> {
        self.db.collection(SPEND_ORGS)
    }

    fn entries(&self) -> Collection<Document> {
        self.db.collection(SPEND_ENTRIES)
    }

    /// Writes a granted hold's entry. Idempotent, because a concurrent reservation may
    /// write the same parked hold.
    async fn write_entry(&self, entry: &Document) -> Result<()> {
        let id = entry.get("_id").cloned().unwrap_or(Bson::Null);
        match self
            .entries()
            .update_one(doc! { "_id": id }, doc! { "$setOnInsert": entry.clone() })
            .with_options(upsert())
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if is_duplicate_key(&error) => Ok(()),
            Err(error) => Err(unavailable(error)),
        }
    }

    /// Writes the entry parked in `pending` and clears it.
    async fn settle_pending(&self, org: &str, pending: &Document) -> Result<()> {
        self.write_entry(pending).await?;
        let id = pending.get("_id").cloned().unwrap_or(Bson::Null);
        self.orgs()
            .update_one(
                doc! { "_id": org, "pending._id": id },
                doc! { "$unset": { "pending": "" } },
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn consumed(&self, limit: &adk_core::SpendLimit, now: DateTime<Utc>) -> Result<i64> {
        let mut filter = doc! {
            "$or": [
                { "state": STATE_COMMITTED, "settled_us": { "$gte": window_start_micros(limit.key.period, now) } },
                { "state": STATE_RESERVED, "expires_us": { "$gt": now.timestamp_micros() } },
            ],
        };
        scope_filter(&mut filter, &limit.key);
        self.sum(
            filter,
            doc! {
                "$cond": [{ "$eq": ["$state", STATE_COMMITTED] }, "$committed", "$reserved"]
            },
        )
        .await
    }

    async fn sum(&self, filter: Document, amount: Document) -> Result<i64> {
        let pipeline = [
            doc! { "$match": filter },
            doc! { "$group": { "_id": Bson::Null, "total": { "$sum": amount } } },
        ];
        let mut cursor = self.entries().aggregate(pipeline).await.map_err(unavailable)?;
        if cursor.advance().await.map_err(unavailable)? {
            let row = cursor.deserialize_current().map_err(unavailable)?;
            return Ok(read_i64(&row, "total").unwrap_or(0));
        }
        Ok(0)
    }

    async fn settle(&self, id: ReservationId, update: Document) -> Result<()> {
        let result = self
            .entries()
            .update_one(
                doc! { "_id": id.as_str(), "state": STATE_RESERVED },
                doc! { "$set": update },
            )
            .await
            .map_err(unavailable)?;
        if result.matched_count == 0 {
            return Err(SpendError::UnknownReservation(id).into());
        }
        Ok(())
    }
}

#[async_trait]
impl SpendLedger for MongoSpendLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let amount = stored_amount(amount_micro_usd)?;
        match self
            .orgs()
            .update_one(doc! { "_id": &key.org }, doc! { "$setOnInsert": { "seq": 0_i64 } })
            .with_options(upsert())
            .await
        {
            Ok(_) => {}
            // A concurrent first reservation created the document.
            Err(error) if is_duplicate_key(&error) => {}
            Err(error) => return Err(unavailable(error)),
        }

        for attempt in 0..MAX_CLAIM_ATTEMPTS {
            let org = self
                .orgs()
                .find_one(doc! { "_id": &key.org })
                .await
                .map_err(unavailable)?
                .ok_or_else(|| unavailable("the organization document disappeared"))?;
            if let Ok(pending) = org.get_document("pending") {
                self.settle_pending(&key.org, pending).await?;
                continue;
            }
            let seq = read_i64(&org, "seq").unwrap_or(0);

            let now = self.clock.now();
            for limit in self.limits.applicable(key) {
                let consumed = u64::try_from(self.consumed(limit, now).await?).unwrap_or(0);
                if consumed.saturating_add(amount_micro_usd) > limit.max_micro_usd {
                    return Err(SpendError::LimitExceeded {
                        limit: limit.key.clone(),
                        max_micro_usd: limit.max_micro_usd,
                        consumed_micro_usd: consumed,
                        requested_micro_usd: amount_micro_usd,
                    }
                    .into());
                }
            }

            let id = ReservationId::generate();
            let ttl = i64::try_from(self.ttl.as_micros()).unwrap_or(i64::MAX);
            let entry = doc! {
                "_id": id.as_str(),
                "org": &key.org,
                "agent": optional(key.agent.as_deref()),
                "vendor": optional(key.vendor.as_deref()),
                "state": STATE_RESERVED,
                "reserved": amount,
                "committed": Bson::Null,
                "created_us": now.timestamp_micros(),
                "expires_us": now.timestamp_micros().saturating_add(ttl),
                "settled_us": Bson::Null,
            };
            let claimed = self
                .orgs()
                .update_one(
                    doc! { "_id": &key.org, "seq": seq, "pending": Bson::Null },
                    doc! { "$inc": { "seq": 1_i64 }, "$set": { "pending": entry.clone() } },
                )
                .await
                .map_err(unavailable)?;
            if claimed.modified_count == 1 {
                self.settle_pending(&key.org, &entry).await?;
                return Ok(id);
            }
            tokio::time::sleep(claim_backoff(attempt)).await;
        }
        Err(unavailable(format!(
            "reservation for {key} lost {MAX_CLAIM_ATTEMPTS} claims to concurrent reservations"
        )))
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let amount = stored_amount(actual_micro_usd)?;
        let now = self.clock.now().timestamp_micros();
        self.settle(id, doc! { "state": STATE_COMMITTED, "committed": amount, "settled_us": now })
            .await
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        let now = self.clock.now().timestamp_micros();
        self.settle(id, doc! { "state": STATE_RELEASED, "settled_us": now }).await
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        let mut filter = doc! {
            "state": STATE_COMMITTED,
            "settled_us": { "$gte": window_start_micros(period, self.clock.now()) },
        };
        scope_filter(&mut filter, key);
        let spent = self.sum(filter, doc! { "$ifNull": ["$committed", 0_i64] }).await?;
        Ok(u64::try_from(spent).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------
// Action ledger
// ---------------------------------------------------------------------------

fn store_error(code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Session, ErrorCategory::Unavailable, code, message)
}

fn ledger_error(message: String) -> AdkError {
    store_error("session.action_ledger.storage", message)
}

fn parse_time(text: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|e| ledger_error(format!("action ledger field '{field}' is corrupt: {e}")))
}

/// MongoDB-backed [`ActionLedger`], so a non-idempotent tool call is never repeated
/// across a process restart.
///
/// One document per call in `adk_action_ledger`, keyed by idempotency key, holding the
/// tool name, argument digest, effect, start time, and outcome — never the arguments
/// or result themselves.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::MongoActionLedger;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let ledger = MongoActionLedger::new("mongodb://localhost:27017", "adk").await?;
/// ledger.migrate().await?;
/// let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoActionLedger {
    db: Database,
}

impl MongoActionLedger {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        let db = connect(connection_string, database_name)
            .await
            .map_err(|e| ledger_error(format!("action ledger connection failed: {e}")))?;
        Ok(Self::from_database(db))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Checks that the server is reachable. The ledger needs no index beyond `_id`.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be reached.
    pub async fn migrate(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| ledger_error(format!("action ledger ping failed: {e}")))?;
        Ok(())
    }

    fn collection(&self) -> Collection<Document> {
        self.db.collection(ACTION_LEDGER)
    }
}

fn to_bson<T: serde::Serialize>(value: &T, what: &str) -> Result<Bson> {
    mongodb::bson::to_bson(value)
        .map_err(|e| ledger_error(format!("action ledger could not encode {what}: {e}")))
}

fn record_from_document(document: &Document) -> Result<ActionRecord> {
    let required = |field: &str| -> Result<&str> {
        read_str(document, field)
            .ok_or_else(|| ledger_error(format!("action ledger field '{field}' is missing")))
    };
    let decode = |field: &str| -> Result<Option<Bson>> {
        Ok(match document.get(field) {
            None | Some(Bson::Null) => None,
            Some(value) => Some(value.clone()),
        })
    };
    let effect: ToolEffect =
        mongodb::bson::from_bson(Bson::String(required("effect")?.to_string()))
            .map_err(|e| ledger_error(format!("action ledger field 'effect' is corrupt: {e}")))?;
    let outcome: Option<ActionOutcome> = decode("outcome")?
        .map(mongodb::bson::from_bson)
        .transpose()
        .map_err(|e| ledger_error(format!("action ledger field 'outcome' is corrupt: {e}")))?;
    Ok(ActionRecord {
        key: required("_id")?.to_string(),
        tool_name: required("tool_name")?.to_string(),
        args_digest: required("args_digest")?.to_string(),
        effect,
        started_at: parse_time(required("started_at")?, "started_at")?,
        outcome,
        completed_at: read_str(document, "completed_at")
            .map(|text| parse_time(text, "completed_at"))
            .transpose()?,
    })
}

#[async_trait]
impl ActionLedger for MongoActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let effect = to_bson(&record.effect, "the effect")?;
        let outcome = match &record.outcome {
            Some(outcome) => to_bson(outcome, "the outcome")?,
            None => Bson::Null,
        };
        let document = doc! {
            "_id": &record.key,
            "tool_name": &record.tool_name,
            "args_digest": &record.args_digest,
            "effect": effect,
            "started_at": record.started_at.to_rfc3339(),
            "outcome": outcome,
            "completed_at": optional(record.completed_at.map(|time| time.to_rfc3339()).as_deref()),
        };
        match self.collection().insert_one(document).await {
            Ok(_) => Ok(()),
            Err(error) if is_duplicate_key(&error) => Err(duplicate_key_error(&record.key)),
            Err(error) => Err(ledger_error(format!("action ledger begin failed: {error}"))),
        }
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let outcome = to_bson(&outcome, "the outcome")?;
        // `outcome: null` also matches a document without the field.
        let updated = self
            .collection()
            .update_one(
                doc! { "_id": key, "outcome": Bson::Null },
                doc! { "$set": { "outcome": outcome, "completed_at": Utc::now().to_rfc3339() } },
            )
            .await
            .map_err(|e| ledger_error(format!("action ledger complete failed: {e}")))?;
        if updated.matched_count == 0 {
            return Err(match self.get(key).await? {
                Some(_) => already_completed_error(key),
                None => missing_key_error(key),
            });
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        self.collection()
            .find_one(doc! { "_id": key })
            .await
            .map_err(|e| ledger_error(format!("action ledger read failed: {e}")))?
            .as_ref()
            .map(record_from_document)
            .transpose()
    }
}

// ---------------------------------------------------------------------------
// Approval store
// ---------------------------------------------------------------------------

const KIND_PENDING: &str = "pending";
const KIND_DECISION: &str = "decision";

fn approval_error(message: String) -> AdkError {
    store_error("session.approval_store.storage", message)
}

/// The `_id` of an approval document. Field order is fixed so equality matches.
fn approval_id(scope: &ApprovalScope, kind: &str, fingerprint: &str) -> Document {
    doc! {
        "app": &scope.app_name,
        "user": &scope.user_id,
        "session": &scope.session_id,
        "kind": kind,
        "fingerprint": fingerprint,
    }
}

fn stored_version(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| approval_error(format!("approval request version {version} is out of range")))
}

fn encode_json<T: serde::Serialize>(value: &T, what: &str) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| approval_error(format!("approval store could not encode {what}: {e}")))
}

fn decode_record<T: serde::de::DeserializeOwned>(document: &Document, what: &str) -> Result<T> {
    let json = read_str(document, "record")
        .ok_or_else(|| approval_error(format!("approval store {what} has no 'record' field")))?;
    serde_json::from_str(json)
        .map_err(|e| approval_error(format!("approval store {what} is corrupt: {e}")))
}

fn subject_kind(subject: &SubjectRef) -> Result<String> {
    match serde_json::to_value(subject.kind) {
        Ok(serde_json::Value::String(kind)) => Ok(kind),
        _ => Err(approval_error("approval store could not encode a subject kind".to_string())),
    }
}

/// The fields of a request document other than `_id`.
fn request_fields(request: &ApprovalRequest) -> Result<Document> {
    Ok(doc! {
        "app": &request.scope.app_name,
        "state": request.state.as_str(),
        "open": request.is_open(),
        "created_us": request.created_at.timestamp_micros(),
        "version": stored_version(request.version)?,
        "record": encode_json(request, "request")?,
    })
}

async fn collect_records<T: serde::de::DeserializeOwned>(
    mut cursor: mongodb::Cursor<Document>,
    what: &str,
) -> Result<Vec<T>> {
    let mut found = Vec::new();
    while cursor
        .advance()
        .await
        .map_err(|e| approval_error(format!("approval store read failed: {e}")))?
    {
        let document = cursor
            .deserialize_current()
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        found.push(decode_record(&document, what)?);
    }
    Ok(found)
}

fn decode_approval(document: &Document) -> Result<ToolApproval> {
    let json = read_str(document, "approval")
        .ok_or_else(|| approval_error("approval store field 'approval' is missing".to_string()))?;
    serde_json::from_str(json)
        .map_err(|e| approval_error(format!("approval store field 'approval' is corrupt: {e}")))
}

/// MongoDB-backed [`ApprovalStore`]: calls held for confirmation and their decisions
/// survive restarts and are shared by every process using the database.
///
/// One document per pending call or decision in `adk_approvals`, keyed by scope, kind,
/// and fingerprint. Requests and decisions are stored as JSON strings, so arbitrary tool
/// arguments never meet MongoDB's field-name rules.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::MongoApprovalStore;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = MongoApprovalStore::new("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// let config = RunConfig::builder().approval_store(Arc::new(store)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoApprovalStore {
    db: Database,
}

impl MongoApprovalStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        let db = connect(connection_string, database_name)
            .await
            .map_err(|e| approval_error(format!("approval store connection failed: {e}")))?;
        Ok(Self::from_database(db))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Creates the indexes that list a scope's pending calls, approval requests by
    /// application, state, and openness, and delegations by delegate.
    ///
    /// # Errors
    ///
    /// Returns an error when an index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        let index = |name: &str, keys: Document| {
            IndexModel::builder()
                .keys(keys)
                .options(IndexOptions::builder().name(name.to_string()).build())
                .build()
        };
        self.requests()
            .create_indexes([
                index("adk_approval_requests_open", doc! { "open": 1, "created_us": 1, "_id": 1 }),
                index("adk_approval_requests_app", doc! { "app": 1, "created_us": 1, "_id": 1 }),
                index(
                    "adk_approval_requests_state",
                    doc! { "state": 1, "created_us": 1, "_id": 1 },
                ),
            ])
            .await
            .map_err(|e| approval_error(format!("approval store index creation failed: {e}")))?;
        self.delegations()
            .create_index(index(
                "adk_approval_delegations_to",
                doc! { "to_kind": 1, "to_id": 1, "_id": 1 },
            ))
            .await
            .map_err(|e| approval_error(format!("approval store index creation failed: {e}")))?;
        self.collection()
            .create_index(
                IndexModel::builder()
                    .keys(doc! {
                        "_id.app": 1, "_id.user": 1, "_id.session": 1, "_id.kind": 1,
                        "requested_us": 1,
                    })
                    .options(
                        IndexOptions::builder().name("adk_approvals_scope".to_string()).build(),
                    )
                    .build(),
            )
            .await
            .map_err(|e| approval_error(format!("approval store index creation failed: {e}")))?;
        Ok(())
    }

    fn collection(&self) -> Collection<Document> {
        self.db.collection(APPROVALS)
    }

    fn requests(&self) -> Collection<Document> {
        self.db.collection(APPROVAL_REQUESTS)
    }

    fn delegations(&self) -> Collection<Document> {
        self.db.collection(APPROVAL_DELEGATIONS)
    }

    async fn find(&self, id: Document) -> Result<Option<Document>> {
        self.collection()
            .find_one(doc! { "_id": id })
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))
    }
}

#[async_trait]
impl ApprovalStore for MongoApprovalStore {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let fingerprint = request.fingerprint();
        let request = serde_json::to_string(request)
            .map_err(|e| approval_error(format!("approval store could not encode request: {e}")))?;
        let now = Utc::now();
        match self
            .collection()
            .update_one(
                doc! { "_id": approval_id(scope, KIND_PENDING, &fingerprint) },
                doc! { "$setOnInsert": {
                    "request": request,
                    "requested_at": now.to_rfc3339(),
                    "requested_us": now.timestamp_micros(),
                } },
            )
            .with_options(upsert())
            .await
        {
            Ok(_) => Ok(()),
            // A concurrent record of the same call inserted it first; it keeps its time.
            Err(error) if is_duplicate_key(&error) => Ok(()),
            Err(error) => Err(approval_error(format!("approval store record failed: {error}"))),
        }
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        let mut cursor = self
            .collection()
            .find(doc! {
                "_id.app": &scope.app_name,
                "_id.user": &scope.user_id,
                "_id.session": &scope.session_id,
                "_id.kind": KIND_PENDING,
            })
            .with_options(FindOptions::builder().sort(doc! { "requested_us": 1 }).build())
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        let mut pending = Vec::new();
        while cursor
            .advance()
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?
        {
            let document = cursor
                .deserialize_current()
                .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
            let corrupt =
                |field: &str| approval_error(format!("approval store field '{field}' is corrupt"));
            let fingerprint = document
                .get_document("_id")
                .ok()
                .and_then(|id| read_str(id, "fingerprint"))
                .ok_or_else(|| corrupt("_id.fingerprint"))?
                .to_string();
            let request = read_str(&document, "request")
                .and_then(|json| serde_json::from_str(json).ok())
                .ok_or_else(|| corrupt("request"))?;
            let requested_at = read_str(&document, "requested_at")
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .ok_or_else(|| corrupt("requested_at"))?
                .with_timezone(&Utc);
            pending.push(PendingApproval { fingerprint, request, requested_at });
        }
        Ok(pending)
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        let approval = serde_json::to_string(&approval).map_err(|e| {
            approval_error(format!("approval store could not encode decision: {e}"))
        })?;
        let filter = doc! { "_id": approval_id(scope, KIND_DECISION, fingerprint) };
        let update = doc! { "$set": { "approval": approval } };
        // Two concurrent upserts of one `_id` can collide; the retry updates the winner's.
        let mut result = self
            .collection()
            .update_one(filter.clone(), update.clone())
            .with_options(upsert())
            .await;
        if matches!(&result, Err(error) if is_duplicate_key(error)) {
            result = self.collection().update_one(filter, update).with_options(upsert()).await;
        }
        result.map_err(|e| approval_error(format!("approval store decide failed: {e}")))?;
        // The decision is written first, so a failure here leaves a stale pending entry
        // next to the decision rather than a lost decision.
        self.collection()
            .delete_one(doc! { "_id": approval_id(scope, KIND_PENDING, fingerprint) })
            .await
            .map_err(|e| approval_error(format!("approval store decide failed: {e}")))?;
        Ok(())
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
        let taken = self
            .collection()
            .find_one_and_delete(doc! { "_id": approval_id(scope, KIND_DECISION, fingerprint) })
            .await
            .map_err(|e| approval_error(format!("approval store take failed: {e}")))?;
        let Some(document) = taken else {
            return Ok(None);
        };
        let approval = decode_approval(&document)?;
        Ok(approval.is_live(Utc::now()).then_some(approval))
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        match self.find(approval_id(scope, KIND_DECISION, fingerprint)).await? {
            Some(document) => Ok(decode_approval(&document)?.is_live(Utc::now())),
            None => Ok(false),
        }
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let mut document = doc! { "_id": &request.id };
        document.extend(request_fields(request)?);
        match self.requests().insert_one(document).await {
            Ok(_) => Ok(request.clone()),
            Err(error) if is_duplicate_key(&error) => {
                self.get_request(&request.id).await?.ok_or_else(|| {
                    approval_error(format!("approval request '{}' vanished", request.id))
                })
            }
            Err(error) => Err(approval_error(format!("approval store write failed: {error}"))),
        }
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.requests()
            .find_one(doc! { "_id": id })
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?
            .map(|document| decode_record(&document, "request"))
            .transpose()
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        let updated = self
            .requests()
            .update_one(
                doc! { "_id": &request.id, "version": stored_version(expected_version)? },
                doc! { "$set": request_fields(request)? },
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(updated.matched_count == 1)
    }

    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        let mut filter = Document::new();
        if let Some(app) = &query.app_name {
            filter.insert("app", app);
        }
        if !query.states.is_empty() {
            let states: Vec<&str> = query.states.iter().map(|state| state.as_str()).collect();
            filter.insert("state", doc! { "$in": states });
        }
        if query.open_only {
            filter.insert("open", true);
        }
        if let Some(after) = &query.after {
            let at = after.created_at.timestamp_micros();
            filter.insert(
                "$or",
                vec![
                    doc! { "created_us": { "$gt": at } },
                    doc! { "created_us": at, "_id": { "$gt": &after.id } },
                ],
            );
        }
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let cursor = self
            .requests()
            .find(filter)
            .with_options(
                FindOptions::builder()
                    .sort(doc! { "created_us": 1, "_id": 1 })
                    .limit(limit)
                    .build(),
            )
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        collect_records(cursor, "request").await
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        let filter = doc! { "_id": &delegation.id };
        let update = doc! { "$set": {
            "to_kind": subject_kind(&delegation.to)?,
            "to_id": &delegation.to.id,
            "record": encode_json(delegation, "delegation")?,
        } };
        // Two concurrent upserts of one `_id` can collide; the retry updates the winner's.
        let mut result = self
            .delegations()
            .update_one(filter.clone(), update.clone())
            .with_options(upsert())
            .await;
        if matches!(&result, Err(error) if is_duplicate_key(error)) {
            result = self.delegations().update_one(filter, update).with_options(upsert()).await;
        }
        result.map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(())
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        let cursor = self
            .delegations()
            .find(doc! { "to_kind": subject_kind(to)?, "to_id": &to.id })
            .with_options(FindOptions::builder().sort(doc! { "_id": 1 }).build())
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        collect_records(cursor, "delegation").await
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        let deleted = self
            .delegations()
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(deleted.deleted_count == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_filter_matches_only_named_dimensions() {
        let mut filter = Document::new();
        scope_filter(&mut filter, &SpendKey::org("acme").with_vendor("gemini"));
        assert_eq!(filter, doc! { "org": "acme", "vendor": "gemini" });
    }

    #[test]
    fn approval_ids_keep_a_fixed_field_order() {
        let scope = ApprovalScope::new("app", "user", "session");
        assert_eq!(
            approval_id(&scope, KIND_DECISION, "fp"),
            doc! { "app": "app", "user": "user", "session": "session", "kind": "decision", "fingerprint": "fp" }
        );
    }

    #[test]
    fn records_round_trip_through_bson() {
        let mut record = ActionRecord::new(
            "app/user/session/inv/call",
            "pay",
            &serde_json::json!({ "amount": 5 }),
            ToolEffect::NonIdempotent,
        );
        record.outcome = Some(ActionOutcome::Succeeded { result_digest: "fnv1a128:00".into() });
        record.completed_at = Some(Utc::now());
        let document = doc! {
            "_id": &record.key,
            "tool_name": &record.tool_name,
            "args_digest": &record.args_digest,
            "effect": to_bson(&record.effect, "effect").unwrap(),
            "started_at": record.started_at.to_rfc3339(),
            "outcome": to_bson(record.outcome.as_ref().unwrap(), "outcome").unwrap(),
            "completed_at": record.completed_at.unwrap().to_rfc3339(),
        };
        assert_eq!(record_from_document(&document).unwrap(), record);
    }

    #[test]
    fn integer_fields_read_from_any_numeric_type() {
        let row = doc! { "a": 7_i32, "b": 8_i64, "c": 9.0_f64, "d": "x" };
        assert_eq!(read_i64(&row, "a"), Some(7));
        assert_eq!(read_i64(&row, "b"), Some(8));
        assert_eq!(read_i64(&row, "c"), Some(9));
        assert_eq!(read_i64(&row, "d"), None);
    }
}
