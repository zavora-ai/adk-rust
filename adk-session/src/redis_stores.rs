//! Redis-backed [`SpendLedger`](adk_core::SpendLedger), [`ActionLedger`](adk_core::ActionLedger), and [`ApprovalStore`](adk_core::ApprovalStore).
//!
//! Every multi-step change runs as one Lua script, which Redis executes atomically, so
//! two processes sharing a server can neither overspend a cap, execute one call twice,
//! nor consume one approval twice. Keys that a script touches together share a hash tag
//! (`{...}`), so the stores also run on Redis Cluster.
//!
//! # Key layout
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `adk:spend:{<org>}:holds` | Hash | Unsettled reservations: id → JSON hold (attribution, amount, expiry) |
//! | `adk:spend:{<org>}:day:<YYYY-MM-DD>` | Hash | Committed micro-USD per attribution for one UTC day; expires 70 days after its last write |
//! | `adk:spend:{<org>}:lifetime` | Hash | Committed micro-USD per attribution, all time |
//! | `adk:action:<key>` | Hash | One ledgered call: tool name, argument digest, effect, start time, outcome |
//! | `adk:approval:{<app>:<user>:<session>}:pending` | Hash | Fingerprint → JSON [`PendingApproval`](adk_core::PendingApproval) |
//! | `adk:approval:{<app>:<user>:<session>}:decisions` | Hash | Fingerprint → JSON [`ToolApproval`](adk_core::ToolApproval) |
//! | `adk:{approvals}:request:<id>` | String | JSON [`ApprovalRequest`](adk_core::ApprovalRequest) |
//! | `adk:{approvals}:versions` | Hash | Request ID → version, the compare-and-swap token |
//! | `adk:{approvals}:index:all`, `:index:open`, `:index:state:<state>`, `:index:app:<app>` | Sorted set | Request IDs scored by creation time (µs); ties list by ID |
//! | `adk:{approvals}:delegation:<id>` | String | JSON [`ApprovalDelegation`](adk_core::ApprovalDelegation) |
//! | `adk:{approvals}:delegates:<kind>:<id>` | Sorted set | IDs of the delegations granted to one subject, all scored 0 so they list by ID |
//!
//! Organization, app, user, and session segments are percent-encoded (`%`, `:`, `{`,
//! `}`), so an identifier cannot spill into the next segment or move a key's hash slot.
//! Attributions are the JSON array `[agent, vendor]`, with `null` for "unattributed".
//!
//! # Atomicity
//!
//! | Operation | Mechanism |
//! |-----------|-----------|
//! | `reserve` | One script reads live holds and committed spend for every applicable cap, then writes the hold |
//! | `commit` | One script removes the hold and adds the amount to the day and lifetime totals |
//! | `release` | `HDEL` of the hold |
//! | `begin` | One script creates the record only when the key is absent |
//! | `complete` | One script sets the outcome only when the record exists and has none |
//! | `decide` | One script clears the pending entry and writes the decision |
//! | `take_decision` | One script reads and deletes the decision |
//! | `create_request` | One script writes the request and its index entries only when the ID is absent |
//! | `replace_request` | One script compares the stored version, then rewrites the request and moves its index entries |
//!
//! Approval requests and delegations share the `{approvals}` hash tag, so each script's
//! keys sit in one Redis Cluster slot; a request's application never changes after it
//! is created, so its application index entry is written once.
//!
//! Reservation holds are removed when settled. A hold that is never settled stays in
//! the holds hash, without counting once it expires, so a late commit still records
//! the spend. Amounts are summed in Lua as double-precision numbers, exact up to
//! 2^53 micro-USD (about nine billion US dollars) per window.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::action_ledger::{already_completed_error, duplicate_key_error, missing_key_error};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, ApprovalDelegation, ApprovalRequest,
    ApprovalRequestQuery, ApprovalScope, ApprovalState, ApprovalStore, Clock,
    DEFAULT_RESERVATION_TTL, ErrorCategory, ErrorComponent, PendingApproval, ReservationId, Result,
    SpendError, SpendKey, SpendLedger, SpendLimits, SpendPeriod, SystemClock, ToolApproval,
    ToolConfirmationDecision, ToolConfirmationRequest, ToolEffect,
};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use fred::prelude::*;

/// How long a committed day bucket is kept after its last write. Covers the longest
/// window that reads day buckets (a month) with room to spare.
const DAY_BUCKET_TTL_SECS: i64 = 70 * 24 * 60 * 60;

pub(crate) async fn connect(url: &str) -> std::result::Result<Client, String> {
    let config = Config::from_url(url).map_err(|e| format!("redis connection failed: {e}"))?;
    let client = Builder::from_config(config)
        .build()
        .map_err(|e| format!("redis connection failed: {e}"))?;
    client.init().await.map_err(|e| format!("redis connection failed: {e}"))?;
    Ok(client)
}

pub(crate) async fn load_scripts(
    client: &Client,
    scripts: &[&str],
) -> std::result::Result<(), String> {
    for script in scripts {
        let _: String = client
            .script_load(*script)
            .await
            .map_err(|e| format!("redis script load failed: {e}"))?;
    }
    Ok(())
}

/// Percent-encodes the characters that delimit key segments or hash tags.
fn encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            '{' => encoded.push_str("%7B"),
            '}' => encoded.push_str("%7D"),
            other => encoded.push(other),
        }
    }
    encoded
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

// ---------------------------------------------------------------------------
// Spend ledger
// ---------------------------------------------------------------------------

/// KEYS: holds, lifetime, then the day buckets from the first of the month to today
/// (today last). ARGV: now (µs), amount, reservation id, hold JSON, applicable limits JSON.
/// Returns `ok`, or `limit:<index>:<consumed>` for the first cap that refuses.
const RESERVE_SCRIPT: &str = r#"
local now = tonumber(ARGV[1])
local amount = tonumber(ARGV[2])
local limits = cjson.decode(ARGV[5])
local function covers(limit, agent, vendor)
  return (limit.a == cjson.null or limit.a == agent)
     and (limit.v == cjson.null or limit.v == vendor)
end
local live = {}
local holds = redis.call('HGETALL', KEYS[1])
for i = 1, #holds, 2 do
  local hold = cjson.decode(holds[i + 1])
  if tonumber(hold.x) > now then table.insert(live, hold) end
end
local function committed(key, limit)
  local total = 0
  local fields = redis.call('HGETALL', key)
  for i = 1, #fields, 2 do
    local attribution = cjson.decode(fields[i])
    if covers(limit, attribution[1], attribution[2]) then
      total = total + tonumber(fields[i + 1])
    end
  end
  return total
end
for index, limit in ipairs(limits) do
  local consumed = 0
  if limit.p == 'lifetime' then
    consumed = committed(KEYS[2], limit)
  elseif limit.p == 'day' then
    consumed = committed(KEYS[#KEYS], limit)
  else
    for k = 3, #KEYS do consumed = consumed + committed(KEYS[k], limit) end
  end
  for _, hold in ipairs(live) do
    if covers(limit, hold.a, hold.v) then consumed = consumed + tonumber(hold.m) end
  end
  if consumed + amount > tonumber(limit.m) then
    return 'limit:' .. (index - 1) .. ':' .. string.format('%.0f', consumed)
  end
end
redis.call('HSET', KEYS[1], ARGV[3], ARGV[4])
return 'ok'
"#;

/// KEYS: holds, lifetime, today's bucket. ARGV: reservation id, amount, bucket TTL (s).
/// Returns 1 when the hold was settled, 0 when it is unknown or already settled.
const COMMIT_SCRIPT: &str = r#"
local raw = redis.call('HGET', KEYS[1], ARGV[1])
if not raw then return 0 end
redis.call('HDEL', KEYS[1], ARGV[1])
local hold = cjson.decode(raw)
redis.call('HINCRBY', KEYS[3], hold.f, ARGV[2])
redis.call('EXPIRE', KEYS[3], ARGV[3])
redis.call('HINCRBY', KEYS[2], hold.f, ARGV[2])
return 1
"#;

fn unavailable(error: impl std::fmt::Display) -> AdkError {
    SpendError::Unavailable(error.to_string()).into()
}

fn stored_amount(amount_micro_usd: u64) -> Result<i64> {
    i64::try_from(amount_micro_usd).map_err(|_| SpendError::AmountTooLarge(amount_micro_usd).into())
}

/// The hash field committed spend is recorded under.
fn attribution_field(agent: Option<&str>, vendor: Option<&str>) -> String {
    serde_json::Value::Array(vec![
        agent.map_or(serde_json::Value::Null, Into::into),
        vendor.map_or(serde_json::Value::Null, Into::into),
    ])
    .to_string()
}

fn spend_prefix(org: &str) -> String {
    format!("adk:spend:{{{}}}", encode_segment(org))
}

fn day_key(prefix: &str, day: NaiveDate) -> String {
    format!("{prefix}:day:{}", day.format("%Y-%m-%d"))
}

/// Day bucket keys from the first of `now`'s month to `now`'s day, in order.
fn month_day_keys(prefix: &str, now: DateTime<Utc>) -> Vec<String> {
    let today = now.date_naive();
    let first = today.with_day(1).unwrap_or(today);
    first.iter_days().take_while(|day| *day <= today).map(|day| day_key(prefix, day)).collect()
}

/// Splits a reservation id into the organization it was reserved for.
fn reservation_org(id: &ReservationId) -> Option<String> {
    let (_, org_hex) = id.as_str().split_once('.')?;
    String::from_utf8(hex_decode(org_hex)?).ok()
}

/// Redis-backed [`SpendLedger`].
///
/// Reservations for one organization serialize on a Lua script over that
/// organization's keys, so concurrent reservations from any number of processes never
/// exceed a cap. Limits are configuration: give every process the same [`SpendLimits`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
/// use adk_session::RedisSpendLedger;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let ledger = RedisSpendLedger::new("redis://localhost:6379")
///     .await?
///     .with_limits(
///         SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
///     );
/// ledger.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisSpendLedger {
    client: Client,
    limits: SpendLimits,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl RedisSpendLedger {
    /// Connects to `url` with no limits and [`DEFAULT_RESERVATION_TTL`].
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(unavailable)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self {
            client,
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

    /// Loads the ledger's scripts into the server's script cache.
    ///
    /// Redis needs no schema; this exists so every durable store is set up the same way.
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &[RESERVE_SCRIPT, COMMIT_SCRIPT]).await.map_err(unavailable)
    }
}

#[async_trait]
impl SpendLedger for RedisSpendLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let amount = stored_amount(amount_micro_usd)?;
        let now = self.clock.now();
        let now_us = now.timestamp_micros();
        let ttl = i64::try_from(self.ttl.as_micros()).unwrap_or(i64::MAX);
        let applicable: Vec<_> = self.limits.applicable(key).collect();
        let limits_json = serde_json::Value::Array(
            applicable
                .iter()
                .map(|limit| {
                    serde_json::json!({
                        "p": limit.key.period.to_string(),
                        "a": limit.key.agent,
                        "v": limit.key.vendor,
                        "m": limit.max_micro_usd,
                    })
                })
                .collect(),
        )
        .to_string();
        let id = ReservationId::from(format!(
            "{}.{}",
            uuid::Uuid::new_v4(),
            hex_encode(key.org.as_bytes())
        ));
        let hold = serde_json::json!({
            "a": key.agent,
            "v": key.vendor,
            "f": attribution_field(key.agent.as_deref(), key.vendor.as_deref()),
            "m": amount,
            "x": now_us.saturating_add(ttl),
        })
        .to_string();

        let prefix = spend_prefix(&key.org);
        let mut keys = vec![format!("{prefix}:holds"), format!("{prefix}:lifetime")];
        keys.extend(month_day_keys(&prefix, now));
        let args = vec![
            now_us.to_string(),
            amount.to_string(),
            id.as_str().to_string(),
            hold,
            limits_json,
        ];
        let outcome: String =
            self.client.eval(RESERVE_SCRIPT, keys, args).await.map_err(unavailable)?;
        if outcome == "ok" {
            return Ok(id);
        }
        let refusal = outcome
            .strip_prefix("limit:")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(index, consumed)| {
                Some((index.parse::<usize>().ok()?, consumed.parse::<f64>().ok()?))
            })
            .and_then(|(index, consumed)| Some((applicable.get(index)?, consumed)));
        match refusal {
            Some((limit, consumed)) => Err(SpendError::LimitExceeded {
                limit: limit.key.clone(),
                max_micro_usd: limit.max_micro_usd,
                consumed_micro_usd: consumed as u64,
                requested_micro_usd: amount_micro_usd,
            }
            .into()),
            None => Err(unavailable(format!("unexpected reserve script reply '{outcome}'"))),
        }
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let amount = stored_amount(actual_micro_usd)?;
        let Some(org) = reservation_org(&id) else {
            return Err(SpendError::UnknownReservation(id).into());
        };
        let prefix = spend_prefix(&org);
        let keys = vec![
            format!("{prefix}:holds"),
            format!("{prefix}:lifetime"),
            day_key(&prefix, self.clock.now().date_naive()),
        ];
        let args =
            vec![id.as_str().to_string(), amount.to_string(), DAY_BUCKET_TTL_SECS.to_string()];
        let settled: i64 =
            self.client.eval(COMMIT_SCRIPT, keys, args).await.map_err(unavailable)?;
        if settled == 0 {
            return Err(SpendError::UnknownReservation(id).into());
        }
        Ok(())
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        let Some(org) = reservation_org(&id) else {
            return Err(SpendError::UnknownReservation(id).into());
        };
        let removed: i64 = self
            .client
            .hdel(format!("{}:holds", spend_prefix(&org)), id.as_str())
            .await
            .map_err(unavailable)?;
        if removed == 0 {
            return Err(SpendError::UnknownReservation(id).into());
        }
        Ok(())
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        let now = self.clock.now();
        let prefix = spend_prefix(&key.org);
        let buckets = match period {
            SpendPeriod::Day => vec![day_key(&prefix, now.date_naive())],
            SpendPeriod::Month => month_day_keys(&prefix, now),
            SpendPeriod::Lifetime => vec![format!("{prefix}:lifetime")],
        };
        let mut total = 0u64;
        for bucket in buckets {
            let fields: HashMap<String, i64> =
                self.client.hgetall(&bucket).await.map_err(unavailable)?;
            for (field, amount) in fields {
                let (agent, vendor): (Option<String>, Option<String>) =
                    serde_json::from_str(&field).map_err(|e| {
                        unavailable(format!("spend bucket '{bucket}' field is corrupt: {e}"))
                    })?;
                let entry = SpendKey { org: key.org.clone(), agent, vendor, period };
                if key.covers(&entry) {
                    total = total.saturating_add(u64::try_from(amount).unwrap_or(0));
                }
            }
        }
        Ok(total)
    }
}

// ---------------------------------------------------------------------------
// Action ledger
// ---------------------------------------------------------------------------

/// KEYS: the record. ARGV: field/value pairs. Returns 1 when created, 0 when the key exists.
const BEGIN_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 1 then return 0 end
redis.call('HSET', KEYS[1], unpack(ARGV))
return 1
"#;

/// KEYS: the record. ARGV: outcome JSON, completion time. Returns 0 when the record is
/// missing, 1 when it already has an outcome, 2 when the outcome was recorded.
const COMPLETE_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
if redis.call('HSETNX', KEYS[1], 'outcome', ARGV[1]) == 0 then return 1 end
redis.call('HSET', KEYS[1], 'completed_at', ARGV[2])
return 2
"#;

fn ledger_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.action_ledger.storage",
        message,
    )
}

fn to_json<T: serde::Serialize>(value: &T, what: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|e| ledger_error(format!("{what} serialize failed: {e}")))
}

fn parse_time(text: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|e| ledger_error(format!("action ledger field '{field}' is corrupt: {e}")))
}

/// Redis-backed [`ActionLedger`], so a non-idempotent tool call is never repeated
/// across a process restart.
///
/// Each record stores the idempotency key, tool name, argument digest, effect, start
/// time, and outcome — never the arguments or result themselves.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::RedisActionLedger;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let ledger = RedisActionLedger::new("redis://localhost:6379").await?;
/// ledger.migrate().await?;
/// let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisActionLedger {
    client: Client,
}

impl RedisActionLedger {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(ledger_error)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Loads the ledger's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &[BEGIN_SCRIPT, COMPLETE_SCRIPT]).await.map_err(ledger_error)
    }

    fn record_key(key: &str) -> String {
        format!("adk:action:{key}")
    }
}

#[async_trait]
impl ActionLedger for RedisActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let mut fields = vec![
            "tool_name".to_string(),
            record.tool_name.clone(),
            "args_digest".to_string(),
            record.args_digest.clone(),
            "effect".to_string(),
            to_json(&record.effect, "effect")?,
            "started_at".to_string(),
            record.started_at.to_rfc3339(),
        ];
        if let Some(outcome) = &record.outcome {
            fields.extend(["outcome".to_string(), to_json(outcome, "outcome")?]);
        }
        if let Some(completed_at) = record.completed_at {
            fields.extend(["completed_at".to_string(), completed_at.to_rfc3339()]);
        }
        let created: i64 = self
            .client
            .eval(BEGIN_SCRIPT, vec![Self::record_key(&record.key)], fields)
            .await
            .map_err(|e| ledger_error(format!("action ledger begin failed: {e}")))?;
        if created == 0 {
            return Err(duplicate_key_error(&record.key));
        }
        Ok(())
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let args = vec![to_json(&outcome, "outcome")?, Utc::now().to_rfc3339()];
        let status: i64 = self
            .client
            .eval(COMPLETE_SCRIPT, vec![Self::record_key(key)], args)
            .await
            .map_err(|e| ledger_error(format!("action ledger complete failed: {e}")))?;
        match status {
            0 => Err(missing_key_error(key)),
            1 => Err(already_completed_error(key)),
            _ => Ok(()),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        let mut fields: HashMap<String, String> = self
            .client
            .hgetall(Self::record_key(key))
            .await
            .map_err(|e| ledger_error(format!("action ledger read failed: {e}")))?;
        if fields.is_empty() {
            return Ok(None);
        }
        let mut required = |name: &str| {
            fields
                .remove(name)
                .ok_or_else(|| ledger_error(format!("action ledger field '{name}' is missing")))
        };
        let tool_name = required("tool_name")?;
        let args_digest = required("args_digest")?;
        let effect: ToolEffect = serde_json::from_str(&required("effect")?)
            .map_err(|e| ledger_error(format!("action ledger field 'effect' is corrupt: {e}")))?;
        let started_at = parse_time(&required("started_at")?, "started_at")?;
        let outcome = fields
            .remove("outcome")
            .map(|json| {
                serde_json::from_str::<ActionOutcome>(&json).map_err(|e| {
                    ledger_error(format!("action ledger field 'outcome' is corrupt: {e}"))
                })
            })
            .transpose()?;
        let completed_at = fields
            .remove("completed_at")
            .map(|text| parse_time(&text, "completed_at"))
            .transpose()?;
        Ok(Some(ActionRecord {
            key: key.to_string(),
            tool_name,
            args_digest,
            effect,
            started_at,
            outcome,
            completed_at,
        }))
    }
}

// ---------------------------------------------------------------------------
// Approval store
// ---------------------------------------------------------------------------

/// KEYS: pending, decisions. ARGV: fingerprint, approval JSON.
const DECIDE_SCRIPT: &str = r#"
redis.call('HDEL', KEYS[1], ARGV[1])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
return 1
"#;

/// KEYS: decisions. ARGV: fingerprint. Returns the approval JSON it removed, or nil.
const TAKE_SCRIPT: &str = r#"
local raw = redis.call('HGET', KEYS[1], ARGV[1])
if raw then redis.call('HDEL', KEYS[1], ARGV[1]) end
return raw
"#;

/// KEYS: record, versions, all, open, app, state. ARGV: id, version, record JSON, score
/// (creation time, µs), open (`1` or `0`). Returns the stored record JSON.
const CREATE_REQUEST_SCRIPT: &str = r#"
local existing = redis.call('GET', KEYS[1])
if existing then return existing end
redis.call('SET', KEYS[1], ARGV[3])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
redis.call('ZADD', KEYS[3], ARGV[4], ARGV[1])
if ARGV[5] == '1' then redis.call('ZADD', KEYS[4], ARGV[4], ARGV[1]) end
redis.call('ZADD', KEYS[5], ARGV[4], ARGV[1])
redis.call('ZADD', KEYS[6], ARGV[4], ARGV[1])
return ARGV[3]
"#;

/// KEYS: record, versions, all, open, app, then the six state indexes in
/// [`ApprovalState::ALL`] order. ARGV: id, expected version, new version, record JSON,
/// score, open (`1` or `0`), position of the new state (1-based). Returns 1 when written.
const REPLACE_REQUEST_SCRIPT: &str = r#"
local current = redis.call('HGET', KEYS[2], ARGV[1])
if (not current) or current ~= ARGV[2] then return 0 end
redis.call('SET', KEYS[1], ARGV[4])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
redis.call('ZADD', KEYS[3], ARGV[5], ARGV[1])
if ARGV[6] == '1' then
  redis.call('ZADD', KEYS[4], ARGV[5], ARGV[1])
else
  redis.call('ZREM', KEYS[4], ARGV[1])
end
redis.call('ZADD', KEYS[5], ARGV[5], ARGV[1])
for i = 6, 11 do redis.call('ZREM', KEYS[i], ARGV[1]) end
redis.call('ZADD', KEYS[5 + tonumber(ARGV[7])], ARGV[5], ARGV[1])
return 1
"#;

/// KEYS: delegation, new delegate index, previous delegate index. ARGV: id, JSON.
const PUT_DELEGATION_SCRIPT: &str = r#"
redis.call('ZREM', KEYS[3], ARGV[1])
redis.call('SET', KEYS[1], ARGV[2])
redis.call('ZADD', KEYS[2], 0, ARGV[1])
return 1
"#;

/// KEYS: delegation, delegate index. ARGV: id. Returns 1 when a delegation was removed.
const DELETE_DELEGATION_SCRIPT: &str = r#"
redis.call('ZREM', KEYS[2], ARGV[1])
return redis.call('DEL', KEYS[1])
"#;

const APPROVALS_PREFIX: &str = "adk:{approvals}";

/// How many index entries one `list_requests` round trip reads.
const LIST_BATCH: i64 = 200;

fn approval_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.approval_store.storage",
        message,
    )
}

fn request_key(id: &str) -> String {
    format!("{APPROVALS_PREFIX}:request:{}", encode_segment(id))
}

fn versions_key() -> String {
    format!("{APPROVALS_PREFIX}:versions")
}

fn index_key(index: &str) -> String {
    format!("{APPROVALS_PREFIX}:index:{index}")
}

fn state_index(state: ApprovalState) -> String {
    index_key(&format!("state:{}", state.as_str()))
}

fn app_index(app_name: &str) -> String {
    index_key(&format!("app:{}", encode_segment(app_name)))
}

fn delegation_key(id: &str) -> String {
    format!("{APPROVALS_PREFIX}:delegation:{}", encode_segment(id))
}

fn delegates_key(subject: &SubjectRef) -> Result<String> {
    let kind = match serde_json::to_value(subject.kind) {
        Ok(serde_json::Value::String(kind)) => kind,
        _ => return Err(approval_error("could not encode a subject kind".to_string())),
    };
    Ok(format!("{APPROVALS_PREFIX}:delegates:{kind}:{}", encode_segment(&subject.id)))
}

fn parse_json<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|e| approval_error(format!("approval store {what} is corrupt: {e}")))
}

fn encode_json<T: serde::Serialize>(value: &T, what: &str) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| approval_error(format!("approval store could not encode {what}: {e}")))
}

fn approval_prefix(scope: &ApprovalScope) -> String {
    format!(
        "adk:approval:{{{}:{}:{}}}",
        encode_segment(&scope.app_name),
        encode_segment(&scope.user_id),
        encode_segment(&scope.session_id)
    )
}

fn parse_approval(json: &str) -> Result<ToolApproval> {
    serde_json::from_str(json)
        .map_err(|e| approval_error(format!("approval store decision is corrupt: {e}")))
}

/// Redis-backed [`ApprovalStore`], so held calls and their decisions survive a
/// restart and are shared by every process that points at the same server.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::RedisApprovalStore;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = RedisApprovalStore::new("redis://localhost:6379").await?;
/// store.migrate().await?;
/// let config = RunConfig::builder().approval_store(Arc::new(store)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisApprovalStore {
    client: Client,
}

impl RedisApprovalStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(approval_error)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(
            &self.client,
            &[
                DECIDE_SCRIPT,
                TAKE_SCRIPT,
                CREATE_REQUEST_SCRIPT,
                REPLACE_REQUEST_SCRIPT,
                PUT_DELEGATION_SCRIPT,
                DELETE_DELEGATION_SCRIPT,
            ],
        )
        .await
        .map_err(approval_error)
    }

    async fn read_delegation(&self, id: &str) -> Result<Option<ApprovalDelegation>> {
        let stored: Option<String> = self
            .client
            .get(delegation_key(id))
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        stored.map(|json| parse_json(&json, "delegation")).transpose()
    }
}

#[async_trait]
impl ApprovalStore for RedisApprovalStore {
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
        let json = serde_json::to_string(&entry)
            .map_err(|e| approval_error(format!("pending approval serialize failed: {e}")))?;
        let _: bool = self
            .client
            .hsetnx(format!("{}:pending", approval_prefix(scope)), fingerprint, json)
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(())
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        let entries: HashMap<String, String> = self
            .client
            .hgetall(format!("{}:pending", approval_prefix(scope)))
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        let mut pending = entries
            .values()
            .map(|json| {
                serde_json::from_str::<PendingApproval>(json).map_err(|e| {
                    approval_error(format!("approval store pending entry is corrupt: {e}"))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        pending.sort_by(|a, b| {
            a.requested_at.cmp(&b.requested_at).then_with(|| a.fingerprint.cmp(&b.fingerprint))
        });
        Ok(pending)
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        let prefix = approval_prefix(scope);
        let json = serde_json::to_string(&approval)
            .map_err(|e| approval_error(format!("approval serialize failed: {e}")))?;
        let _: i64 = self
            .client
            .eval(
                DECIDE_SCRIPT,
                vec![format!("{prefix}:pending"), format!("{prefix}:decisions")],
                vec![fingerprint.to_string(), json],
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
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
        let taken: Option<String> = self
            .client
            .eval(
                TAKE_SCRIPT,
                vec![format!("{}:decisions", approval_prefix(scope))],
                vec![fingerprint.to_string()],
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        let Some(json) = taken else {
            return Ok(None);
        };
        let approval = parse_approval(&json)?;
        Ok(approval.is_live(Utc::now()).then_some(approval))
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        let stored: Option<String> = self
            .client
            .hget(format!("{}:decisions", approval_prefix(scope)), fingerprint)
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        match stored {
            Some(json) => Ok(parse_approval(&json)?.is_live(Utc::now())),
            None => Ok(false),
        }
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        let stored: String = self
            .client
            .eval(
                CREATE_REQUEST_SCRIPT,
                vec![
                    request_key(&request.id),
                    versions_key(),
                    index_key("all"),
                    index_key("open"),
                    app_index(&request.scope.app_name),
                    state_index(request.state),
                ],
                vec![
                    request.id.clone(),
                    request.version.to_string(),
                    encode_json(request, "request")?,
                    request.created_at.timestamp_micros().to_string(),
                    if request.is_open() { "1" } else { "0" }.to_string(),
                ],
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        parse_json(&stored, "request")
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        let stored: Option<String> = self
            .client
            .get(request_key(id))
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        stored.map(|json| parse_json(&json, "request")).transpose()
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        let mut keys = vec![
            request_key(&request.id),
            versions_key(),
            index_key("all"),
            index_key("open"),
            app_index(&request.scope.app_name),
        ];
        keys.extend(ApprovalState::ALL.into_iter().map(state_index));
        let position = ApprovalState::ALL
            .iter()
            .position(|state| *state == request.state)
            .map_or(1, |index| index + 1);
        let written: i64 = self
            .client
            .eval(
                REPLACE_REQUEST_SCRIPT,
                keys,
                vec![
                    request.id.clone(),
                    expected_version.to_string(),
                    request.version.to_string(),
                    encode_json(request, "request")?,
                    request.created_at.timestamp_micros().to_string(),
                    if request.is_open() { "1" } else { "0" }.to_string(),
                    position.to_string(),
                ],
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(written == 1)
    }

    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        // The most selective index the query names; `matches` applies the rest.
        let index = match (&query.app_name, query.open_only, query.states.as_slice()) {
            (Some(app), _, _) => app_index(app),
            (None, true, _) => index_key("open"),
            (None, false, [state]) => state_index(*state),
            (None, false, _) => index_key("all"),
        };
        let min =
            query.after.as_ref().map_or(i64::MIN, |cursor| cursor.created_at.timestamp_micros());
        let mut found = Vec::new();
        let mut offset = 0;
        while found.len() < query.limit {
            let ids: Vec<String> = self
                .client
                .zrangebyscore(&index, min, "+inf", false, Some((offset, LIST_BATCH)))
                .await
                .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
            if ids.is_empty() {
                break;
            }
            let keys: Vec<String> = ids.iter().map(|id| request_key(id)).collect();
            let records: Vec<Option<String>> = self
                .client
                .mget(keys)
                .await
                .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
            for json in records.into_iter().flatten() {
                let request: ApprovalRequest = parse_json(&json, "request")?;
                if query.matches(&request) && found.len() < query.limit {
                    found.push(request);
                }
            }
            if i64::try_from(ids.len()).unwrap_or(LIST_BATCH) < LIST_BATCH {
                break;
            }
            offset += LIST_BATCH;
        }
        Ok(found)
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        let index = delegates_key(&delegation.to)?;
        let previous = match self.read_delegation(&delegation.id).await? {
            Some(previous) => delegates_key(&previous.to)?,
            None => index.clone(),
        };
        let _: i64 = self
            .client
            .eval(
                PUT_DELEGATION_SCRIPT,
                vec![delegation_key(&delegation.id), index, previous],
                vec![delegation.id.clone(), encode_json(delegation, "delegation")?],
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(())
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        let ids: Vec<String> = self
            .client
            .zrange(delegates_key(to)?, 0, -1, None, false, None, false)
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = ids.iter().map(|id| delegation_key(id)).collect();
        let records: Vec<Option<String>> = self
            .client
            .mget(keys)
            .await
            .map_err(|e| approval_error(format!("approval store read failed: {e}")))?;
        let mut delegations = Vec::new();
        for json in records.into_iter().flatten() {
            let delegation: ApprovalDelegation = parse_json(&json, "delegation")?;
            if delegation.to == *to {
                delegations.push(delegation);
            }
        }
        Ok(delegations)
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        let Some(stored) = self.read_delegation(id).await? else {
            return Ok(false);
        };
        let removed: i64 = self
            .client
            .eval(
                DELETE_DELEGATION_SCRIPT,
                vec![delegation_key(id), delegates_key(&stored.to)?],
                vec![id.to_string()],
            )
            .await
            .map_err(|e| approval_error(format!("approval store write failed: {e}")))?;
        Ok(removed == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn segments_cannot_break_keys_or_hash_tags() {
        assert_eq!(encode_segment("a:b{c}%"), "a%3Ab%7Bc%7D%25");
        assert_eq!(spend_prefix("acme"), "adk:spend:{acme}");
        assert_eq!(spend_prefix("x}{y"), "adk:spend:{x%7D%7By}");
        let scope = ApprovalScope::new("app:1", "user", "s{1}");
        assert_eq!(approval_prefix(&scope), "adk:approval:{app%3A1:user:s%7B1%7D}");
    }

    #[test]
    fn reservation_ids_carry_their_organization() {
        let org = "conformance:org/é";
        let id = ReservationId::from(format!("uuid.{}", hex_encode(org.as_bytes())));
        assert_eq!(reservation_org(&id).as_deref(), Some(org));
        assert_eq!(reservation_org(&ReservationId::from("no-dot".to_string())), None);
        assert_eq!(reservation_org(&ReservationId::from("u.zz".to_string())), None);
    }

    #[test]
    fn month_buckets_run_from_the_first_to_today() {
        let at = Utc.with_ymd_and_hms(2026, 11, 3, 0, 10, 0).unwrap();
        assert_eq!(
            month_day_keys("p", at),
            ["p:day:2026-11-01", "p:day:2026-11-02", "p:day:2026-11-03"]
        );
    }

    #[test]
    fn attribution_fields_round_trip() {
        let field = attribution_field(None, Some("gemini"));
        assert_eq!(field, r#"[null,"gemini"]"#);
        let parsed: (Option<String>, Option<String>) = serde_json::from_str(&field).unwrap();
        assert_eq!(parsed, (None, Some("gemini".to_string())));
    }
}
