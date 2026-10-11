//! Neo4j-backed [`SpendLedger`](adk_core::SpendLedger), [`ActionLedger`](adk_core::ActionLedger), and
//! [`ApprovalStore`](adk_core::ApprovalStore).
//!
//! Every store keeps its records as nodes and serializes conflicting writes with Neo4j's
//! transaction write locks: a transaction first writes a lock node, which holds an
//! exclusive lock until commit, and only then reads the state it decides on. Reads after
//! the lock see every transaction that committed before it, so the decision cannot act
//! on a stale read.
//!
//! | Store | Label | Key | Serialized by |
//! |-------|-------|-----|---------------|
//! | [`Neo4jSpendLedger`] | `AdkSpendEntry` | `id` (unique) | Write lock on the organization's `AdkSpendOrg` node, taken before the limit check |
//! | [`Neo4jActionLedger`] | `AdkAction` | `key` (unique) | The uniqueness constraint for `begin`; a write lock on the record for `complete` |
//! | [`Neo4jApprovalStore`] | `AdkApprovalPending`, `AdkApprovalDecision` | scope + `fingerprint` (unique) | Write lock on the scope's `AdkApprovalScope` node for `decide` and `take_decision` |
//! | [`Neo4jApprovalStore`] requests and delegations | `AdkApprovalRequest`, `AdkApprovalDelegation` | `id` (unique) | The uniqueness constraint for `create_request`; a write lock on the request before its version is compared for `replace_request` |
//!
//! Lock nodes are created by an auto-commit `MERGE` before the transaction, so the
//! transaction only ever upgrades straight to a write lock and two writers cannot
//! deadlock on a shared-to-exclusive upgrade.
//!
//! Timestamps are stored losslessly: action records as RFC 3339 strings, spend and
//! approval times as integer microseconds since the epoch. Call `migrate` once per
//! database to create the constraints the stores rely on.

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
use neo4rs::{Graph, Neo4jErrorKind, Query, Row, Txn, query};

/// Attempts at a transaction that fails with a transient error, such as a detected
/// deadlock, before the error is returned.
const TRANSIENT_ATTEMPTS: usize = 5;

/// Schema codes that mean an `IF NOT EXISTS` statement raced another one and lost.
const SCHEMA_ALREADY_EXISTS: [&str; 4] = [
    "Neo.ClientError.Schema.EquivalentSchemaRuleAlreadyExists",
    "Neo.ClientError.Schema.ConstraintAlreadyExists",
    "Neo.ClientError.Schema.IndexAlreadyExists",
    "Neo.ClientError.Schema.ConstraintWithNameAlreadyExists",
];

const CONSTRAINT_VIOLATION: &str = "Neo.ClientError.Schema.ConstraintValidationFailed";

/// A failure inside a store operation: either the database's or the store's own verdict.
enum Failure {
    Neo4j(neo4rs::Error),
    Store(AdkError),
}

impl From<neo4rs::Error> for Failure {
    fn from(error: neo4rs::Error) -> Self {
        Self::Neo4j(error)
    }
}

impl From<AdkError> for Failure {
    fn from(error: AdkError) -> Self {
        Self::Store(error)
    }
}

impl Failure {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Neo4j(neo4rs::Error::Neo4j(error)) if error.kind() == Neo4jErrorKind::Transient)
    }

    fn into_adk(self, storage_error: impl FnOnce(String) -> AdkError) -> AdkError {
        match self {
            Self::Neo4j(error) => storage_error(error.to_string()),
            Self::Store(error) => error,
        }
    }
}

fn neo4j_code(error: &neo4rs::Error) -> Option<&str> {
    match error {
        neo4rs::Error::Neo4j(error) => Some(error.code()),
        _ => None,
    }
}

/// Retries `$attempt` while it fails with a transient error, then maps the failure with
/// `$storage_error`.
macro_rules! retry_transient {
    ($storage_error:expr, $attempt:expr) => {{
        let mut tries = 1;
        loop {
            match $attempt.await {
                Err(failure) if failure.is_transient() && tries < TRANSIENT_ATTEMPTS => tries += 1,
                result => break result.map_err(|failure| failure.into_adk($storage_error)),
            }
        }
    }};
}

/// Reads every row of `query` inside `txn`.
async fn rows(txn: &mut Txn, query: Query) -> std::result::Result<Vec<Row>, neo4rs::Error> {
    let mut stream = txn.execute(query).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next(&mut *txn).await? {
        rows.push(row);
    }
    Ok(rows)
}

/// Reads every row of an auto-commit `query`.
async fn graph_rows(graph: &Graph, query: Query) -> std::result::Result<Vec<Row>, neo4rs::Error> {
    let mut stream = graph.execute(query).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next().await? {
        rows.push(row);
    }
    Ok(rows)
}

fn column<T: serde::de::DeserializeOwned>(row: &Row, name: &str) -> std::result::Result<T, String> {
    row.get::<T>(name).map_err(|error| format!("column '{name}' unreadable: {error}"))
}

/// Runs idempotent schema statements, tolerating a concurrent `migrate` that created the
/// same rule first.
async fn run_schema(
    graph: &Graph,
    statements: &[&str],
    storage_error: fn(String) -> AdkError,
) -> Result<()> {
    for statement in statements {
        let mut tries = 1;
        loop {
            match graph.run(query(statement)).await {
                Ok(()) => break,
                Err(error)
                    if neo4j_code(&error)
                        .is_some_and(|code| SCHEMA_ALREADY_EXISTS.contains(&code)) =>
                {
                    break;
                }
                Err(error) if tries < TRANSIENT_ATTEMPTS => {
                    tracing::debug!(error = %error, "retrying neo4j schema statement");
                    tries += 1;
                }
                Err(error) => return Err(storage_error(format!("migration failed: {error}"))),
            }
        }
    }
    Ok(())
}

async fn connect(
    uri: &str,
    user: &str,
    password: &str,
    storage_error: fn(String) -> AdkError,
) -> Result<Graph> {
    Graph::new(uri, user, password)
        .await
        .map_err(|error| storage_error(format!("neo4j connection failed: {error}")))
}

// ---------------------------------------------------------------------------
// Spend ledger
// ---------------------------------------------------------------------------

const STATE_RESERVED: &str = "reserved";
const STATE_COMMITTED: &str = "committed";
const STATE_RELEASED: &str = "released";

fn spend_unavailable(message: String) -> AdkError {
    SpendError::Unavailable(message).into()
}

fn stored_amount(amount_micro_usd: u64) -> Result<i64> {
    i64::try_from(amount_micro_usd).map_err(|_| SpendError::AmountTooLarge(amount_micro_usd).into())
}

/// Committed spend in the window plus live holds, for one limit's scope.
const CONSUMED: &str = "\
MATCH (e:AdkSpendEntry {org: $org}) \
WHERE ($agent IS NULL OR e.agent = $agent) AND ($vendor IS NULL OR e.vendor = $vendor) \
RETURN coalesce(sum(CASE \
    WHEN e.state = 'committed' AND e.settled_at_us >= $since THEN e.committed_micro_usd \
    WHEN e.state = 'reserved' AND e.expires_at_us > $now THEN e.reserved_micro_usd \
    ELSE 0 END), 0) AS consumed";

const SPENT: &str = "\
MATCH (e:AdkSpendEntry {org: $org, state: 'committed'}) \
WHERE e.settled_at_us >= $since \
    AND ($agent IS NULL OR e.agent = $agent) AND ($vendor IS NULL OR e.vendor = $vendor) \
RETURN coalesce(sum(e.committed_micro_usd), 0) AS spent";

/// Neo4j-backed [`SpendLedger`].
///
/// Each reservation is one `AdkSpendEntry` node. A reservation write-locks its
/// organization's `AdkSpendOrg` node before checking limits, so every reservation that
/// could share a cap waits for the one before it to commit. Limits are configuration:
/// give every process the same [`SpendLimits`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
/// use adk_session::Neo4jSpendLedger;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let ledger = Neo4jSpendLedger::new("bolt://localhost:7687", "neo4j", "password")
///     .await?
///     .with_limits(
///         SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
///     );
/// ledger.migrate().await?;
/// # Ok(())
/// # }
/// ```
pub struct Neo4jSpendLedger {
    graph: Graph,
    limits: SpendLimits,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Neo4jSpendLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jSpendLedger")
            .field("limits", &self.limits)
            .field("ttl", &self.ttl)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl Neo4jSpendLedger {
    /// Connects to Neo4j with no limits and [`DEFAULT_RESERVATION_TTL`].
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, spend_unavailable).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self {
            graph,
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

    /// Creates the constraints and index the ledger relies on.
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &[
                "CREATE CONSTRAINT adk_spend_org_unique IF NOT EXISTS \
                 FOR (o:AdkSpendOrg) REQUIRE o.org IS UNIQUE",
                "CREATE CONSTRAINT adk_spend_entry_id_unique IF NOT EXISTS \
                 FOR (e:AdkSpendEntry) REQUIRE e.id IS UNIQUE",
                "CREATE INDEX adk_spend_entry_org_state IF NOT EXISTS \
                 FOR (e:AdkSpendEntry) ON (e.org, e.state)",
                // The consumption query filters on the organization alone, which the
                // composite index above cannot serve.
                "CREATE INDEX adk_spend_entry_org IF NOT EXISTS FOR (e:AdkSpendEntry) ON (e.org)",
            ],
            spend_unavailable,
        )
        .await
    }

    async fn try_reserve(
        &self,
        key: &SpendKey,
        amount: i64,
        amount_micro_usd: u64,
    ) -> std::result::Result<ReservationId, Failure> {
        // Created outside the transaction, so the transaction takes the write lock directly
        // instead of upgrading a MERGE's lock, which two reservations could deadlock on.
        self.graph
            .run(query("MERGE (:AdkSpendOrg {org: $org})").param("org", key.org.clone()))
            .await?;
        let mut txn = self.graph.start_txn().await?;
        txn.run(
            query("MATCH (o:AdkSpendOrg {org: $org}) SET o.seq = coalesce(o.seq, 0) + 1")
                .param("org", key.org.clone()),
        )
        .await?;
        let now = self.clock.now();
        let now_us = now.timestamp_micros();
        for limit in self.limits.applicable(key) {
            let since = limit
                .key
                .period
                .window_start(now)
                .map_or(i64::MIN, |start| start.timestamp_micros());
            let found = rows(
                &mut txn,
                query(CONSUMED)
                    .param("org", limit.key.org.clone())
                    .param("agent", limit.key.agent.clone())
                    .param("vendor", limit.key.vendor.clone())
                    .param("since", since)
                    .param("now", now_us),
            )
            .await?;
            let consumed: i64 = match found.first() {
                Some(row) => column(row, "consumed").map_err(spend_unavailable)?,
                None => 0,
            };
            let consumed = u64::try_from(consumed).unwrap_or(0);
            if consumed.saturating_add(amount_micro_usd) > limit.max_micro_usd {
                let _ = txn.rollback().await;
                return Err(AdkError::from(SpendError::LimitExceeded {
                    limit: limit.key.clone(),
                    max_micro_usd: limit.max_micro_usd,
                    consumed_micro_usd: consumed,
                    requested_micro_usd: amount_micro_usd,
                })
                .into());
            }
        }
        let ttl = i64::try_from(self.ttl.as_micros()).unwrap_or(i64::MAX);
        let id = ReservationId::generate();
        txn.run(
            query(
                "CREATE (:AdkSpendEntry {id: $id, org: $org, agent: $agent, vendor: $vendor, \
                 state: $state, reserved_micro_usd: $amount, created_at_us: $now, \
                 expires_at_us: $expires})",
            )
            .param("id", id.as_str())
            .param("org", key.org.clone())
            .param("agent", key.agent.clone())
            .param("vendor", key.vendor.clone())
            .param("state", STATE_RESERVED)
            .param("amount", amount)
            .param("now", now_us)
            .param("expires", now_us.saturating_add(ttl)),
        )
        .await?;
        txn.commit().await?;
        Ok(id)
    }

    /// Moves a `reserved` entry to `state`, write-locking it first so a concurrent
    /// settlement of the same reservation sees the first one's result.
    async fn try_settle(
        &self,
        id: &ReservationId,
        state: &str,
        committed: Option<i64>,
    ) -> std::result::Result<bool, Failure> {
        let mut txn = self.graph.start_txn().await?;
        txn.run(
            query("MATCH (e:AdkSpendEntry {id: $id}) SET e.lock_seq = coalesce(e.lock_seq, 0) + 1")
                .param("id", id.as_str()),
        )
        .await?;
        let settled = rows(
            &mut txn,
            query(
                "MATCH (e:AdkSpendEntry {id: $id}) WHERE e.state = $reserved \
                 SET e.state = $state, e.committed_micro_usd = $committed, \
                     e.settled_at_us = $now \
                 RETURN e.id AS id",
            )
            .param("id", id.as_str())
            .param("reserved", STATE_RESERVED)
            .param("state", state)
            .param("committed", committed)
            .param("now", self.clock.now().timestamp_micros()),
        )
        .await?;
        txn.commit().await?;
        Ok(!settled.is_empty())
    }
}

#[async_trait]
impl SpendLedger for Neo4jSpendLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let amount = stored_amount(amount_micro_usd)?;
        retry_transient!(spend_unavailable, self.try_reserve(key, amount, amount_micro_usd))
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let amount = stored_amount(actual_micro_usd)?;
        if retry_transient!(spend_unavailable, self.try_settle(&id, STATE_COMMITTED, Some(amount)))?
        {
            Ok(())
        } else {
            Err(SpendError::UnknownReservation(id).into())
        }
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        if retry_transient!(spend_unavailable, self.try_settle(&id, STATE_RELEASED, None))? {
            Ok(())
        } else {
            Err(SpendError::UnknownReservation(id).into())
        }
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        let since = period
            .window_start(self.clock.now())
            .map_or(i64::MIN, |start| start.timestamp_micros());
        let found = graph_rows(
            &self.graph,
            query(SPENT)
                .param("org", key.org.clone())
                .param("agent", key.agent.clone())
                .param("vendor", key.vendor.clone())
                .param("since", since),
        )
        .await
        .map_err(|error| spend_unavailable(error.to_string()))?;
        let spent: i64 = match found.first() {
            Some(row) => column(row, "spent").map_err(spend_unavailable)?,
            None => 0,
        };
        Ok(u64::try_from(spent).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------
// Action ledger
// ---------------------------------------------------------------------------

fn ledger_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.action_ledger.storage",
        format!("action ledger storage failed: {message}"),
    )
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| ledger_error(format!("serialize failed: {e}")))
}

fn from_json<T: serde::de::DeserializeOwned>(json: &str, field: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|e| ledger_error(format!("property '{field}' is corrupt: {e}")))
}

fn parse_time(text: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|e| ledger_error(format!("property '{field}' is corrupt: {e}")))
}

/// Neo4j-backed [`ActionLedger`], so a non-idempotent tool call is never repeated across
/// a process restart.
///
/// Each record is one `AdkAction` node holding the idempotency key, tool name, argument
/// digest, effect, start time, and outcome — never the arguments or result themselves. A
/// uniqueness constraint on `key` makes `begin` admit one caller per key.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::Neo4jActionLedger;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let ledger = Neo4jActionLedger::new("bolt://localhost:7687", "neo4j", "password").await?;
/// ledger.migrate().await?;
/// let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
pub struct Neo4jActionLedger {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jActionLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jActionLedger").finish_non_exhaustive()
    }
}

impl Neo4jActionLedger {
    /// Connects to Neo4j. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.action_ledger.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, ledger_error).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Creates the uniqueness constraint that makes `begin` admit one caller per key.
    ///
    /// # Errors
    ///
    /// Returns `session.action_ledger.storage` when the schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &["CREATE CONSTRAINT adk_action_key_unique IF NOT EXISTS \
               FOR (a:AdkAction) REQUIRE a.key IS UNIQUE"],
            ledger_error,
        )
        .await
    }

    /// Records `outcome` on a begun record, write-locking it first so concurrent
    /// completes see each other's results. Returns whether the outcome was recorded.
    async fn try_complete(&self, key: &str, outcome: &str) -> std::result::Result<bool, Failure> {
        let mut txn = self.graph.start_txn().await?;
        txn.run(
            query("MATCH (a:AdkAction {key: $key}) SET a.lock_seq = coalesce(a.lock_seq, 0) + 1")
                .param("key", key),
        )
        .await?;
        let completed = rows(
            &mut txn,
            query(
                "MATCH (a:AdkAction {key: $key}) WHERE a.outcome IS NULL \
                 SET a.outcome = $outcome, a.completed_at = $now RETURN a.key AS key",
            )
            .param("key", key)
            .param("outcome", outcome)
            .param("now", Utc::now().to_rfc3339()),
        )
        .await?;
        txn.commit().await?;
        Ok(!completed.is_empty())
    }
}

#[async_trait]
impl ActionLedger for Neo4jActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let statement = query(
            "CREATE (:AdkAction {key: $key, tool_name: $tool_name, args_digest: $args_digest, \
             effect: $effect, started_at: $started_at, outcome: $outcome, \
             completed_at: $completed_at})",
        )
        .param("key", record.key.clone())
        .param("tool_name", record.tool_name.clone())
        .param("args_digest", record.args_digest.clone())
        .param("effect", to_json(&record.effect)?)
        .param("started_at", record.started_at.to_rfc3339())
        .param("outcome", record.outcome.as_ref().map(to_json).transpose()?)
        .param("completed_at", record.completed_at.map(|time| time.to_rfc3339()));
        match self.graph.run(statement).await {
            Ok(()) => Ok(()),
            Err(error) if neo4j_code(&error) == Some(CONSTRAINT_VIOLATION) => {
                Err(duplicate_key_error(&record.key))
            }
            Err(error) => Err(ledger_error(format!("begin failed: {error}"))),
        }
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let outcome = to_json(&outcome)?;
        if retry_transient!(ledger_error, self.try_complete(key, &outcome))? {
            return Ok(());
        }
        Err(match self.get(key).await? {
            Some(_) => already_completed_error(key),
            None => missing_key_error(key),
        })
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (a:AdkAction {key: $key}) RETURN a.key AS key, a.tool_name AS tool_name, \
                 a.args_digest AS args_digest, a.effect AS effect, a.started_at AS started_at, \
                 a.outcome AS outcome, a.completed_at AS completed_at",
            )
            .param("key", key),
        )
        .await
        .map_err(|e| ledger_error(format!("read failed: {e}")))?;
        let Some(row) = found.first() else {
            return Ok(None);
        };
        let text = |name: &str| column::<String>(row, name).map_err(ledger_error);
        let optional = |name: &str| column::<Option<String>>(row, name).map_err(ledger_error);
        let effect: ToolEffect = from_json(&text("effect")?, "effect")?;
        Ok(Some(ActionRecord {
            key: text("key")?,
            tool_name: text("tool_name")?,
            args_digest: text("args_digest")?,
            effect,
            started_at: parse_time(&text("started_at")?, "started_at")?,
            outcome: optional("outcome")?.map(|json| from_json(&json, "outcome")).transpose()?,
            completed_at: optional("completed_at")?
                .map(|time| parse_time(&time, "completed_at"))
                .transpose()?,
        }))
    }
}

// ---------------------------------------------------------------------------
// Approval store
// ---------------------------------------------------------------------------

fn approval_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.approval_store.storage",
        format!("approval store storage failed: {message}"),
    )
}

fn scoped(statement: &str, scope: &ApprovalScope) -> Query {
    query(statement)
        .param("app_name", scope.app_name.clone())
        .param("user_id", scope.user_id.clone())
        .param("session_id", scope.session_id.clone())
}

const SCOPE_MATCH: &str = "{app_name: $app_name, user_id: $user_id, session_id: $session_id}";
const ENTRY_MATCH: &str =
    "{app_name: $app_name, user_id: $user_id, session_id: $session_id, fingerprint: $fingerprint}";

fn decision_name(decision: ToolConfirmationDecision) -> &'static str {
    match decision {
        ToolConfirmationDecision::Approve => "approve",
        ToolConfirmationDecision::Deny => "deny",
    }
}

/// Reads a decision row into a [`ToolApproval`].
fn approval_from_row(row: &Row) -> Result<ToolApproval> {
    let decision = match column::<String>(row, "decision").map_err(approval_error)?.as_str() {
        "approve" => ToolConfirmationDecision::Approve,
        "deny" => ToolConfirmationDecision::Deny,
        other => return Err(approval_error(format!("unknown decision '{other}'"))),
    };
    let expires_at = column::<Option<i64>>(row, "expires_at_us")
        .map_err(approval_error)?
        .map(|micros| {
            DateTime::from_timestamp_micros(micros)
                .ok_or_else(|| approval_error(format!("expiry {micros} is out of range")))
        })
        .transpose()?;
    let edited_args = column::<Option<String>>(row, "edited_args")
        .map_err(approval_error)?
        .map(|json| {
            serde_json::from_str(&json)
                .map_err(|e| approval_error(format!("edited arguments are corrupt: {e}")))
        })
        .transpose()?;
    let mut approval = match decision {
        ToolConfirmationDecision::Approve => ToolApproval::approve(),
        ToolConfirmationDecision::Deny => ToolApproval::deny(),
    };
    approval.expires_at = expires_at;
    approval.edited_args = edited_args;
    approval.message = column::<Option<String>>(row, "message").map_err(approval_error)?;
    Ok(approval)
}

const DECISION_COLUMNS: &str = "d.decision AS decision, d.expires_at_us AS expires_at_us, \
    d.edited_args AS edited_args, d.message AS message";

fn stored_version(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| approval_error(format!("approval request version {version} is out of range")))
}

fn encode_json<T: serde::Serialize>(value: &T, what: &str) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| approval_error(format!("could not encode {what}: {e}")))
}

fn decode_records<T: serde::de::DeserializeOwned>(found: &[Row], what: &str) -> Result<Vec<T>> {
    found
        .iter()
        .map(|row| {
            let json: String = column(row, "record").map_err(approval_error)?;
            serde_json::from_str(&json)
                .map_err(|e| approval_error(format!("stored {what} is corrupt: {e}")))
        })
        .collect()
}

fn subject_kind(subject: &SubjectRef) -> Result<String> {
    match serde_json::to_value(subject.kind) {
        Ok(serde_json::Value::String(kind)) => Ok(kind),
        _ => Err(approval_error("could not encode a subject kind".to_string())),
    }
}

/// Binds a request's indexed properties and record to `$app`, `$state`, `$open`,
/// `$created_us`, `$version`, and `$record`.
fn with_request(statement: Query, request: &ApprovalRequest) -> Result<Query> {
    Ok(statement
        .param("id", request.id.clone())
        .param("app", request.scope.app_name.clone())
        .param("state", request.state.as_str())
        .param("open", request.is_open())
        .param("created_us", request.created_at.timestamp_micros())
        .param("version", stored_version(request.version)?)
        .param("record", encode_json(request, "request")?))
}

const SET_REQUEST: &str = "r.app = $app, r.state = $state, r.open = $open, \
    r.created_us = $created_us, r.version = $version, r.record = $record";

/// Neo4j-backed [`ApprovalStore`], so calls held for approval and their decisions
/// survive a restart and are shared by every process.
///
/// Pending calls are `AdkApprovalPending` nodes and decisions `AdkApprovalDecision`
/// nodes, both keyed by scope and fingerprint. `decide` and `take_decision` write-lock
/// the scope's `AdkApprovalScope` node first, so concurrent takers serialize and one
/// approval authorizes one execution.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::Neo4jApprovalStore;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = Neo4jApprovalStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// store.migrate().await?;
/// let config = RunConfig::builder().approval_store(Arc::new(store)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
pub struct Neo4jApprovalStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jApprovalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jApprovalStore").finish_non_exhaustive()
    }
}

impl Neo4jApprovalStore {
    /// Connects to Neo4j. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.approval_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, approval_error).await?))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Creates the uniqueness constraints the store relies on.
    ///
    /// # Errors
    ///
    /// Returns `session.approval_store.storage` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &[
                "CREATE CONSTRAINT adk_approval_scope_unique IF NOT EXISTS \
                 FOR (s:AdkApprovalScope) REQUIRE (s.app_name, s.user_id, s.session_id) IS UNIQUE",
                "CREATE CONSTRAINT adk_approval_pending_unique IF NOT EXISTS \
                 FOR (p:AdkApprovalPending) \
                 REQUIRE (p.app_name, p.user_id, p.session_id, p.fingerprint) IS UNIQUE",
                "CREATE CONSTRAINT adk_approval_decision_unique IF NOT EXISTS \
                 FOR (d:AdkApprovalDecision) \
                 REQUIRE (d.app_name, d.user_id, d.session_id, d.fingerprint) IS UNIQUE",
                // `pending` lists one scope, which the four-property constraint cannot serve.
                "CREATE INDEX adk_approval_pending_scope IF NOT EXISTS \
                 FOR (p:AdkApprovalPending) ON (p.app_name, p.user_id, p.session_id)",
                "CREATE CONSTRAINT adk_approval_request_unique IF NOT EXISTS \
                 FOR (r:AdkApprovalRequest) REQUIRE r.id IS UNIQUE",
                "CREATE INDEX adk_approval_request_app IF NOT EXISTS \
                 FOR (r:AdkApprovalRequest) ON (r.app, r.created_us)",
                "CREATE INDEX adk_approval_request_open IF NOT EXISTS \
                 FOR (r:AdkApprovalRequest) ON (r.open, r.created_us)",
                "CREATE CONSTRAINT adk_approval_delegation_unique IF NOT EXISTS \
                 FOR (d:AdkApprovalDelegation) REQUIRE d.id IS UNIQUE",
                "CREATE INDEX adk_approval_delegation_to IF NOT EXISTS \
                 FOR (d:AdkApprovalDelegation) ON (d.to_kind, d.to_id)",
            ],
            approval_error,
        )
        .await
    }

    /// Write-locks the scope node inside `txn`. Returns `false` when the scope has never
    /// had a decision.
    async fn lock_scope(
        txn: &mut Txn,
        scope: &ApprovalScope,
    ) -> std::result::Result<bool, neo4rs::Error> {
        let locked = rows(
            txn,
            scoped(
                &format!(
                    "MATCH (s:AdkApprovalScope {SCOPE_MATCH}) \
                     SET s.seq = coalesce(s.seq, 0) + 1 RETURN count(s) AS locked"
                ),
                scope,
            ),
        )
        .await?;
        Ok(locked.first().and_then(|row| row.get::<i64>("locked").ok()).unwrap_or(0) > 0)
    }

    async fn try_decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: &ToolApproval,
    ) -> std::result::Result<(), Failure> {
        self.graph.run(scoped(&format!("MERGE (:AdkApprovalScope {SCOPE_MATCH})"), scope)).await?;
        let mut txn = self.graph.start_txn().await?;
        Self::lock_scope(&mut txn, scope).await?;
        txn.run(
            scoped(&format!("MATCH (p:AdkApprovalPending {ENTRY_MATCH}) DELETE p"), scope)
                .param("fingerprint", fingerprint),
        )
        .await?;
        txn.run(
            scoped(
                &format!(
                    "MERGE (d:AdkApprovalDecision {ENTRY_MATCH}) \
                     SET d.decision = $decision, d.expires_at_us = $expires_at_us, \
                         d.edited_args = $edited_args, d.message = $message"
                ),
                scope,
            )
            .param("fingerprint", fingerprint)
            .param("decision", decision_name(approval.decision))
            .param("expires_at_us", approval.expires_at.map(|at| at.timestamp_micros()))
            .param("edited_args", approval.edited_args.as_ref().map(ToString::to_string))
            .param("message", approval.message.clone()),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn try_take(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> std::result::Result<Option<ToolApproval>, Failure> {
        let mut txn = self.graph.start_txn().await?;
        if !Self::lock_scope(&mut txn, scope).await? {
            let _ = txn.rollback().await;
            return Ok(None);
        }
        let taken = rows(
            &mut txn,
            scoped(
                &format!(
                    "MATCH (d:AdkApprovalDecision {ENTRY_MATCH}) \
                     WITH d, {DECISION_COLUMNS} \
                     DELETE d RETURN decision, expires_at_us, edited_args, message"
                ),
                scope,
            )
            .param("fingerprint", fingerprint),
        )
        .await?;
        txn.commit().await?;
        Ok(taken.first().map(approval_from_row).transpose()?)
    }

    async fn try_replace(
        &self,
        request: &ApprovalRequest,
        expected_version: i64,
    ) -> std::result::Result<bool, Failure> {
        let mut txn = self.graph.start_txn().await?;
        // Write-lock the request before reading its version, so the comparison sees
        // every replace that committed first.
        let locked = rows(
            &mut txn,
            query(
                "MATCH (r:AdkApprovalRequest {id: $id}) \
                 SET r.lock_seq = coalesce(r.lock_seq, 0) + 1 RETURN count(r) AS locked",
            )
            .param("id", request.id.clone()),
        )
        .await?;
        if locked.first().and_then(|row| row.get::<i64>("locked").ok()).unwrap_or(0) == 0 {
            let _ = txn.rollback().await;
            return Ok(false);
        }
        let current = rows(
            &mut txn,
            query("MATCH (r:AdkApprovalRequest {id: $id}) RETURN r.version AS version")
                .param("id", request.id.clone()),
        )
        .await?;
        if current.first().and_then(|row| row.get::<i64>("version").ok()) != Some(expected_version)
        {
            let _ = txn.rollback().await;
            return Ok(false);
        }
        txn.run(with_request(
            query(&format!("MATCH (r:AdkApprovalRequest {{id: $id}}) SET {SET_REQUEST}")),
            request,
        )?)
        .await?;
        txn.commit().await?;
        Ok(true)
    }
}

#[async_trait]
impl ApprovalStore for Neo4jApprovalStore {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        let json = serde_json::to_string(request)
            .map_err(|e| approval_error(format!("serialize failed: {e}")))?;
        self.graph
            .run(
                scoped(
                    &format!(
                        "MERGE (p:AdkApprovalPending {ENTRY_MATCH}) \
                         ON CREATE SET p.request = $request, p.requested_at_us = $now"
                    ),
                    scope,
                )
                .param("fingerprint", request.fingerprint())
                .param("request", json)
                .param("now", Utc::now().timestamp_micros()),
            )
            .await
            .map_err(|e| approval_error(format!("record failed: {e}")))
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        let found = graph_rows(
            &self.graph,
            scoped(
                &format!(
                    "MATCH (p:AdkApprovalPending {SCOPE_MATCH}) \
                     RETURN p.fingerprint AS fingerprint, p.request AS request, \
                         p.requested_at_us AS requested_at_us \
                     ORDER BY p.requested_at_us, p.fingerprint"
                ),
                scope,
            ),
        )
        .await
        .map_err(|e| approval_error(format!("read failed: {e}")))?;
        found
            .iter()
            .map(|row| {
                let micros: i64 = column(row, "requested_at_us").map_err(approval_error)?;
                let request: String = column(row, "request").map_err(approval_error)?;
                Ok(PendingApproval {
                    fingerprint: column(row, "fingerprint").map_err(approval_error)?,
                    request: serde_json::from_str(&request)
                        .map_err(|e| approval_error(format!("request is corrupt: {e}")))?,
                    requested_at: DateTime::from_timestamp_micros(micros)
                        .ok_or_else(|| approval_error(format!("time {micros} is out of range")))?,
                })
            })
            .collect()
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        retry_transient!(approval_error, self.try_decide(scope, fingerprint, &approval))
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
        let taken = retry_transient!(approval_error, self.try_take(scope, fingerprint))?;
        Ok(taken.filter(|approval| approval.is_live(Utc::now())))
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        let found = graph_rows(
            &self.graph,
            scoped(
                &format!("MATCH (d:AdkApprovalDecision {ENTRY_MATCH}) RETURN {DECISION_COLUMNS}"),
                scope,
            )
            .param("fingerprint", fingerprint),
        )
        .await
        .map_err(|e| approval_error(format!("read failed: {e}")))?;
        match found.first() {
            Some(row) => Ok(approval_from_row(row)?.is_live(Utc::now())),
            None => Ok(false),
        }
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let statement = with_request(
            query(&format!(
                "MERGE (r:AdkApprovalRequest {{id: $id}}) ON CREATE SET {SET_REQUEST} \
                 RETURN r.record AS record"
            )),
            request,
        )?;
        let mut tries = 1;
        let found = loop {
            match graph_rows(&self.graph, statement.clone()).await {
                // Two concurrent merges of one id: the loser fails the constraint and
                // reads the winner's request on retry.
                Err(error)
                    if tries < TRANSIENT_ATTEMPTS
                        && (neo4j_code(&error) == Some(CONSTRAINT_VIOLATION)
                            || matches!(&error, neo4rs::Error::Neo4j(inner)
                                if inner.kind() == Neo4jErrorKind::Transient)) =>
                {
                    tries += 1;
                }
                result => {
                    break result.map_err(|e| approval_error(format!("create failed: {e}")))?;
                }
            }
        };
        decode_records(&found, "request")?
            .pop()
            .ok_or_else(|| approval_error(format!("approval request '{}' vanished", request.id)))
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        let found = graph_rows(
            &self.graph,
            query("MATCH (r:AdkApprovalRequest {id: $id}) RETURN r.record AS record")
                .param("id", id),
        )
        .await
        .map_err(|e| approval_error(format!("read failed: {e}")))?;
        Ok(decode_records(&found, "request")?.pop())
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        let expected = stored_version(expected_version)?;
        retry_transient!(approval_error, self.try_replace(request, expected))
    }

    async fn list_requests(
        &self,
        query_spec: &ApprovalRequestQuery,
    ) -> Result<Vec<ApprovalRequest>> {
        let states: Vec<String> =
            query_spec.states.iter().map(|state| state.as_str().to_string()).collect();
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (r:AdkApprovalRequest) \
                 WHERE ($app IS NULL OR r.app = $app) \
                   AND (size($states) = 0 OR r.state IN $states) \
                   AND (NOT $open_only OR r.open = true) \
                   AND ($after_id IS NULL OR r.created_us > $after_us \
                        OR (r.created_us = $after_us AND r.id > $after_id)) \
                 RETURN r.record AS record ORDER BY r.created_us, r.id LIMIT $limit",
            )
            .param("app", query_spec.app_name.clone())
            .param("states", states)
            .param("open_only", query_spec.open_only)
            .param("after_id", query_spec.after.as_ref().map(|cursor| cursor.id.clone()))
            .param(
                "after_us",
                query_spec.after.as_ref().map_or(0, |cursor| cursor.created_at.timestamp_micros()),
            )
            .param("limit", i64::try_from(query_spec.limit).unwrap_or(i64::MAX)),
        )
        .await
        .map_err(|e| approval_error(format!("read failed: {e}")))?;
        decode_records(&found, "request")
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        let statement = query(
            "MERGE (d:AdkApprovalDelegation {id: $id}) \
             SET d.to_kind = $to_kind, d.to_id = $to_id, d.record = $record",
        )
        .param("id", delegation.id.clone())
        .param("to_kind", subject_kind(&delegation.to)?)
        .param("to_id", delegation.to.id.clone())
        .param("record", encode_json(delegation, "delegation")?);
        let mut tries = 1;
        loop {
            match self.graph.run(statement.clone()).await {
                Err(error)
                    if tries < TRANSIENT_ATTEMPTS
                        && neo4j_code(&error) == Some(CONSTRAINT_VIOLATION) =>
                {
                    tries += 1;
                }
                result => {
                    break result.map_err(|e| approval_error(format!("write failed: {e}")));
                }
            }
        }
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (d:AdkApprovalDelegation {to_kind: $to_kind, to_id: $to_id}) \
                 RETURN d.record AS record ORDER BY d.id",
            )
            .param("to_kind", subject_kind(to)?)
            .param("to_id", to.id.clone()),
        )
        .await
        .map_err(|e| approval_error(format!("read failed: {e}")))?;
        decode_records(&found, "delegation")
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (d:AdkApprovalDelegation {id: $id}) \
                 WITH d, d.id AS id DELETE d RETURN count(id) AS deleted",
            )
            .param("id", id),
        )
        .await
        .map_err(|e| approval_error(format!("delete failed: {e}")))?;
        Ok(found.first().and_then(|row| row.get::<i64>("deleted").ok()).unwrap_or(0) > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_are_stored_by_stable_name() {
        assert_eq!(decision_name(ToolConfirmationDecision::Approve), "approve");
        assert_eq!(decision_name(ToolConfirmationDecision::Deny), "deny");
    }

    #[test]
    fn transient_errors_are_retried_and_others_are_not() {
        let store = Failure::Store(spend_unavailable("down".into()));
        assert!(!store.is_transient());
        let io = Failure::Neo4j(neo4rs::Error::ConnectionError);
        assert!(!io.is_transient());
        assert_eq!(
            io.into_adk(spend_unavailable).code,
            "spend.ledger_unavailable",
            "a database failure maps to the store's unavailable error"
        );
    }

    #[test]
    fn amounts_beyond_i64_are_refused() {
        assert_eq!(stored_amount(u64::MAX).unwrap_err().code, "spend.amount_too_large");
        assert_eq!(stored_amount(5).unwrap(), 5);
    }
}
