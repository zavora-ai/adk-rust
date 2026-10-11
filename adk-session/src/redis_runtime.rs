//! Redis-backed [`LeaseStore`](adk_core::LeaseStore), [`DurableRunStore`](adk_core::DurableRunStore), [`ScheduleStore`](adk_core::ScheduleStore), and
//! [`GovernanceStateStore`](adk_core::GovernanceStateStore).
//!
//! Every conditional change runs as one Lua script, which Redis executes atomically, so
//! any number of processes sharing a server see one lease winner, one claim per run and
//! epoch, one schedule advance per revision, and one governance change per epoch. Keys
//! carry no expiry: lease and claim deadlines are timestamps compared inside the
//! scripts against the caller's clock, so the durable truth never depends on Redis
//! evicting or expiring a key.
//!
//! # Key layout
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `adk:lease:{<resource>}` | Hash | `owner`, `epoch`, `expires` (µs), `released` |
//! | `{adk:runs}:run:<run id>` | Hash | One run: the enqueued record as JSON (`base`) plus its mutable fields (`state`, `attempt`, `lease_epoch`, `lease_owner`, `lease_expires`, `not_before`, `checkpoint`, `result`, `error`, `revision`, ...) |
//! | `{adk:runs}:seq` | String | The enqueue counter that orders runs |
//! | `{adk:runs}:ready:<queue>` | Sorted set | Runs that may be claimable now, scored by enqueue order |
//! | `{adk:runs}:timers:<queue>` | Sorted set | Runs that become claimable at a time (delays, waits, lease expiry), scored by that time in µs |
//! | `{adk:runs}:holders:<queue>:<key>` | Sorted set | Unfinished runs holding a concurrency key, by enqueue order |
//! | `{adk:runs}:idem:<queue>` | Hash | Idempotency key → run id |
//! | `{adk:runs}:queue:<queue>`, `{adk:runs}:all` | Sorted set | Every run of a queue, and every run, by enqueue order |
//! | `{adk:runs}:finished` | Sorted set | Finished runs by finish time, for `prune` |
//! | `{adk:schedules}:defs`, `{adk:schedules}:revs` | Hash | Schedule id → JSON definition, and → revision |
//! | `{adk:schedules}:due` | Sorted set | Unpaused schedules with a next fire time, scored by it in µs |
//! | `{adk:governance}:states`, `{adk:governance}:epochs` | Hash | Scope key → JSON state, and → epoch |
//!
//! Queue, key, and resource segments are hex-encoded, so no identifier can spill into the
//! next segment or move a key's hash slot. Each store keeps its keys under one hash tag,
//! so the scripts that compute key names from stored fields touch a single Redis Cluster
//! slot.
//!
//! # Atomicity
//!
//! | Operation | Script |
//! |-----------|--------|
//! | Lease `acquire`, `renew`, `release` | Reads the resource hash, compares owner, epoch, expiry, and the released flag, then writes |
//! | Run `enqueue` | Checks the idempotency index, reads the key holders, applies `Reject` or `Interrupt`, then writes the run and its indexes |
//! | Run `claim` | Moves due timers to the ready set, walks it oldest first re-checking each run's state and time, skips key-blocked runs, fails exhausted recoveries, then claims one run at the next epoch |
//! | `heartbeat`, `set_checkpoint`, `wait`, `finish` | Compares the state and `lease_epoch` before writing (fencing) |
//! | `wake`, `request_cancel` | Reads the state and writes the transition |
//! | `prune` | Deletes finished runs older than the cut-off in batches, with their index entries |
//! | Schedule `upsert`, `set_paused`, `advance` | Compare-and-swap on the stored revision |
//! | Governance `set` | Compare-and-swap on the stored epoch |
//!
//! The ready set re-checks every candidate against the caller's clock, so replicas whose
//! clocks disagree never claim a run early. Timestamps are stored as integer
//! microseconds, compared in Lua as double-precision numbers, which is exact for dates
//! before the year 2255.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use adk_core::governance_state::{
    GovernanceScope, GovernanceState, GovernanceStateStore, governance_conflict_error,
};
use adk_core::lease::{Lease, LeaseStore, lease_stale_epoch_error};
use adk_core::principal::SubjectRef;
use adk_core::run_store::{
    ClaimedRun, DurableRunStore, Enqueued, NewRun, OnConflict, RunFilter, RunOutcome, RunRecord,
    RunState, run_not_found_error, run_stale_epoch_error,
};
use adk_core::schedule::{Schedule, ScheduleStore, schedule_not_found_error};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::prelude::*;

use crate::durable_common::{
    GOVERNANCE_STORAGE, LEASE_STORAGE, RUN_STORAGE, SCHEDULE_STORAGE, deadline, hex,
    same_definition, state_name, storage_error, stored_now,
};
use crate::redis_stores::{connect, load_scripts};

/// Attempts at a compare-and-swap that loses to concurrent writers before giving up.
const CAS_ATTEMPTS: usize = 64;

/// Runs deleted per `prune` script call.
const PRUNE_BATCH: usize = 500;

fn micros(at: DateTime<Utc>) -> String {
    at.timestamp_micros().to_string()
}

fn from_micros(text: &str, code: &'static str, field: &str) -> Result<DateTime<Utc>> {
    text.parse::<i64>()
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| storage_error(code, format!("stored field '{field}' is corrupt: '{text}'")))
}

/// Flattens a script reply of scalars into strings.
fn strings(value: Value, code: &'static str) -> Result<Vec<String>> {
    value
        .into_array()
        .into_iter()
        .map(|item| {
            item.into_string()
                .ok_or_else(|| storage_error(code, "redis script returned a non-text value"))
        })
        .collect()
}

/// Splits a `{status, payload}` script reply.
fn status_reply(value: Value, code: &'static str) -> Result<(String, Value)> {
    let mut items = value.into_array().into_iter();
    let status = items
        .next()
        .and_then(Value::into_string)
        .ok_or_else(|| storage_error(code, "redis script returned no status"))?;
    Ok((status, items.next().unwrap_or(Value::Null)))
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/// KEYS: the resource hash. ARGV: owner, now (µs), expiry (µs). Returns the new epoch,
/// or -1 when another owner holds a live lease.
const LEASE_ACQUIRE_SCRIPT: &str = r#"
local h = redis.call('HMGET', KEYS[1], 'owner', 'epoch', 'expires', 'released')
if h[2] and h[4] ~= '1' and tonumber(h[3]) > tonumber(ARGV[2]) and h[1] ~= ARGV[1] then
  return -1
end
local epoch = redis.call('HINCRBY', KEYS[1], 'epoch', 1)
redis.call('HSET', KEYS[1], 'owner', ARGV[1], 'expires', ARGV[3], 'released', '0')
return epoch
"#;

/// KEYS: the resource hash. ARGV: owner, epoch, expiry (µs). Returns 1 when renewed.
const LEASE_RENEW_SCRIPT: &str = r#"
local h = redis.call('HMGET', KEYS[1], 'owner', 'epoch', 'released')
if h[1] ~= ARGV[1] or h[2] ~= ARGV[2] or h[3] == '1' then return 0 end
redis.call('HSET', KEYS[1], 'expires', ARGV[3])
return 1
"#;

/// KEYS: the resource hash. ARGV: owner, epoch. Returns 1 when released.
const LEASE_RELEASE_SCRIPT: &str = r#"
local h = redis.call('HMGET', KEYS[1], 'owner', 'epoch')
if h[1] ~= ARGV[1] or h[2] ~= ARGV[2] then return 0 end
redis.call('HSET', KEYS[1], 'released', '1')
return 1
"#;

fn lease_key(resource: &str) -> String {
    format!("adk:lease:{{{}}}", hex(resource))
}

/// Redis-backed [`LeaseStore`].
///
/// One hash per resource; every grant, renewal, and release is one Lua script, so
/// concurrent acquirers in any number of processes see one winner. The hash is never
/// deleted, so epochs keep counting after a release. See the
/// [module documentation](self) for the layout.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::LeaseStore;
/// use adk_session::RedisLeaseStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let leases = RedisLeaseStore::new("redis://localhost:6379").await?;
/// leases.migrate().await?;
/// if let Some(lease) = leases.acquire("scheduler", "replica-1", Duration::from_secs(30)).await? {
///     leases.renew(&lease, Duration::from_secs(30)).await?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisLeaseStore {
    client: Client,
    clock: Arc<dyn Clock>,
}

impl RedisLeaseStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        let client = connect(url).await.map_err(|e| storage_error(LEASE_STORAGE, e))?;
        Ok(Self::from_client(client))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `session.lease_store.storage` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(
            &self.client,
            &[LEASE_ACQUIRE_SCRIPT, LEASE_RENEW_SCRIPT, LEASE_RELEASE_SCRIPT],
        )
        .await
        .map_err(|e| storage_error(LEASE_STORAGE, e))
    }
}

#[async_trait]
impl LeaseStore for RedisLeaseStore {
    async fn acquire(&self, resource: &str, owner: &str, ttl: Duration) -> Result<Option<Lease>> {
        let now = stored_now(self.clock.now());
        let expires_at = deadline(now, ttl, "lease.invalid_ttl")?;
        let epoch: i64 = self
            .client
            .eval(
                LEASE_ACQUIRE_SCRIPT,
                vec![lease_key(resource)],
                vec![owner.to_string(), micros(now), micros(expires_at)],
            )
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease acquire failed: {e}")))?;
        Ok(u64::try_from(epoch).ok().map(|epoch| Lease {
            resource: resource.to_string(),
            owner: owner.to_string(),
            epoch,
            expires_at,
        }))
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
        let expires_at = deadline(stored_now(self.clock.now()), ttl, "lease.invalid_ttl")?;
        let renewed: i64 = self
            .client
            .eval(
                LEASE_RENEW_SCRIPT,
                vec![lease_key(&lease.resource)],
                vec![lease.owner.clone(), lease.epoch.to_string(), micros(expires_at)],
            )
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease renew failed: {e}")))?;
        if renewed == 0 {
            return Err(lease_stale_epoch_error(&lease.resource, lease.epoch));
        }
        Ok(Lease { expires_at, ..lease.clone() })
    }

    async fn release(&self, lease: &Lease) -> Result<()> {
        let _: i64 = self
            .client
            .eval(
                LEASE_RELEASE_SCRIPT,
                vec![lease_key(&lease.resource)],
                vec![lease.owner.clone(), lease.epoch.to_string()],
            )
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease release failed: {e}")))?;
        Ok(())
    }

    async fn current(&self, resource: &str) -> Result<Option<Lease>> {
        let now = self.clock.now();
        let fields: Vec<Option<String>> = self
            .client
            .hmget(lease_key(resource), vec!["owner", "epoch", "expires", "released"])
            .await
            .map_err(|e| storage_error(LEASE_STORAGE, format!("lease read failed: {e}")))?;
        let [Some(owner), Some(epoch), Some(expires), released] =
            <[Option<String>; 4]>::try_from(fields).map_err(|_| {
                storage_error(LEASE_STORAGE, "lease read returned the wrong field count")
            })?
        else {
            return Ok(None);
        };
        let expires_at = from_micros(&expires, LEASE_STORAGE, "expires")?;
        if released.as_deref() == Some("1") || expires_at <= now {
            return Ok(None);
        }
        let epoch = epoch.parse().map_err(|_| {
            storage_error(LEASE_STORAGE, format!("stored epoch '{epoch}' is corrupt"))
        })?;
        Ok(Some(Lease { resource: resource.to_string(), owner, epoch, expires_at }))
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// Helpers every run script starts with. A run hash stores its queue (`qk`) and
/// concurrency key (`ck`) in hex, which is how the scripts find its index keys.
const RUN_PRELUDE: &str = r#"
local PFX = '{adk:runs}:'
local function run_key(id) return PFX .. 'run:' .. id end
local function bump(key, now)
  redis.call('HINCRBY', key, 'revision', 1)
  redis.call('HSET', key, 'updated', now)
end
local function settle(id, key, now)
  local h = redis.call('HMGET', key, 'qk', 'ck')
  redis.call('ZREM', PFX .. 'ready:' .. h[1], id)
  redis.call('ZREM', PFX .. 'timers:' .. h[1], id)
  if h[2] then redis.call('ZREM', PFX .. 'holders:' .. h[1] .. ':' .. h[2], id) end
  redis.call('ZADD', PFX .. 'finished', now, id)
end
local function cancel(id, key, now)
  local state = redis.call('HGET', key, 'state')
  if state == 'queued' or state == 'waiting' then
    redis.call('HSET', key, 'state', 'cancelled')
    redis.call('HDEL', key, 'lease_owner', 'lease_expires')
    bump(key, now)
    settle(id, key, now)
  elseif state == 'running' then
    redis.call('HSET', key, 'cancel_requested', '1')
    bump(key, now)
  end
end
local function fence(key, epoch)
  local h = redis.call('HMGET', key, 'state', 'lease_epoch')
  if not h[1] then return 'missing' end
  if h[1] ~= 'running' or h[2] ~= epoch then return 'stale' end
  return nil
end
"#;

/// ARGV: run id, queue (hex), idempotency key or '', concurrency key (hex) or '',
/// policy, now (µs), not-before (µs) or '', then field/value pairs for the new hash.
/// Returns `{'existing', fields}`, `{'rejected', blocking id}`, or `{'created', fields}`.
const RUN_ENQUEUE_BODY: &str = r#"
local id, qk, ikey, ck, policy, now, nb = ARGV[1], ARGV[2], ARGV[3], ARGV[4], ARGV[5], ARGV[6], ARGV[7]
if ikey ~= '' then
  local existing = redis.call('HGET', PFX .. 'idem:' .. qk, ikey)
  if existing then
    local fields = redis.call('HGETALL', run_key(existing))
    if #fields > 0 then return {'existing', fields} end
  end
end
local holders_key = PFX .. 'holders:' .. qk .. ':' .. ck
if ck ~= '' then
  local holders = redis.call('ZRANGE', holders_key, 0, -1)
  if policy == 'reject' and #holders > 0 then return {'rejected', holders[1]} end
  if policy == 'interrupt' then
    for _, holder in ipairs(holders) do cancel(holder, run_key(holder), now) end
  end
end
local seq = redis.call('INCR', PFX .. 'seq')
local key = run_key(id)
redis.call('HSET', key, 'seq', seq, 'qk', qk, 'state', 'queued', 'attempt', '0',
  'lease_epoch', '0', 'revision', '1', 'updated', now, 'cancel_requested', '0', 'wake_pending', '0')
for i = 8, #ARGV, 2 do redis.call('HSET', key, ARGV[i], ARGV[i + 1]) end
if ck ~= '' then
  redis.call('HSET', key, 'ck', ck)
  redis.call('ZADD', holders_key, seq, id)
end
if ikey ~= '' then redis.call('HSET', PFX .. 'idem:' .. qk, ikey, id) end
redis.call('ZADD', PFX .. 'all', seq, id)
redis.call('ZADD', PFX .. 'queue:' .. qk, seq, id)
if nb ~= '' then
  redis.call('HSET', key, 'not_before', nb)
  redis.call('ZADD', PFX .. 'timers:' .. qk, nb, id)
else
  redis.call('ZADD', PFX .. 'ready:' .. qk, seq, id)
end
return {'created', redis.call('HGETALL', key)}
"#;

/// ARGV: queue (hex), worker, now (µs), lease expiry (µs). Returns the claimed run's
/// fields, or nil when nothing is claimable.
const RUN_CLAIM_BODY: &str = r#"
local qk, worker, now, expires = ARGV[1], ARGV[2], ARGV[3], ARGV[4]
local nowv = tonumber(now)
local ready, timers = PFX .. 'ready:' .. qk, PFX .. 'timers:' .. qk
for _, id in ipairs(redis.call('ZRANGEBYSCORE', timers, '-inf', now)) do
  local seq = redis.call('HGET', run_key(id), 'seq')
  redis.call('ZREM', timers, id)
  if seq then redis.call('ZADD', ready, seq, id) end
end
local offset = 0
while true do
  local batch = redis.call('ZRANGE', ready, offset, offset + 63)
  if #batch == 0 then return false end
  local kept = 0
  for _, id in ipairs(batch) do
    local key = run_key(id)
    local h = redis.call('HMGET', key, 'state', 'not_before', 'lease_expires', 'attempt',
      'max_attempts', 'ck')
    local state, verdict, at = h[1], 'drop', nil
    if state == 'queued' then
      if h[2] and tonumber(h[2]) > nowv then at = h[2] else verdict = 'claim' end
    elseif state == 'waiting' then
      if h[2] and tonumber(h[2]) <= nowv then verdict = 'claim' else at = h[2] end
    elseif state == 'running' then
      if h[3] and tonumber(h[3]) > nowv then
        at = h[3]
      elseif tonumber(h[4]) >= tonumber(h[5]) then
        redis.call('HSET', key, 'state', 'failed', 'error',
          'attempts exhausted: the lease of attempt ' .. h[4] .. ' of ' .. h[5] .. ' expired')
        redis.call('HDEL', key, 'lease_owner', 'lease_expires')
        bump(key, now)
        settle(id, key, now)
      else
        verdict = 'claim'
      end
    end
    if verdict == 'claim' and state == 'queued' and h[6] then
      local first = redis.call('ZRANGE', PFX .. 'holders:' .. qk .. ':' .. h[6], 0, 0)
      if first[1] ~= id then verdict = 'keep' end
    end
    if verdict == 'claim' then
      if state ~= 'waiting' then redis.call('HINCRBY', key, 'attempt', 1) end
      redis.call('HINCRBY', key, 'lease_epoch', 1)
      redis.call('HSET', key, 'state', 'running', 'lease_owner', worker, 'lease_expires', expires,
        'heartbeat', now)
      redis.call('HDEL', key, 'not_before')
      bump(key, now)
      redis.call('ZREM', ready, id)
      redis.call('ZADD', timers, expires, id)
      return redis.call('HGETALL', key)
    elseif verdict == 'keep' then
      kept = kept + 1
    else
      redis.call('ZREM', ready, id)
      if at then redis.call('ZADD', timers, at, id) end
    end
  end
  offset = offset + kept
end
"#;

/// ARGV: run id, epoch, now (µs), lease expiry (µs). Returns 'missing', 'stale', or the
/// `cancel_requested` flag.
const RUN_HEARTBEAT_BODY: &str = r#"
local key = run_key(ARGV[1])
local refused = fence(key, ARGV[2])
if refused then return refused end
redis.call('HSET', key, 'lease_expires', ARGV[4], 'heartbeat', ARGV[3])
bump(key, ARGV[3])
redis.call('ZADD', PFX .. 'timers:' .. redis.call('HGET', key, 'qk'), ARGV[4], ARGV[1])
return redis.call('HGET', key, 'cancel_requested')
"#;

/// ARGV: run id, epoch, now (µs), checkpoint JSON. Returns 'missing', 'stale', or 'ok'.
const RUN_CHECKPOINT_BODY: &str = r#"
local key = run_key(ARGV[1])
local refused = fence(key, ARGV[2])
if refused then return refused end
redis.call('HSET', key, 'checkpoint', ARGV[4])
bump(key, ARGV[3])
return 'ok'
"#;

/// ARGV: run id, epoch, now (µs), wait end (µs) or ''. Returns 'missing', 'stale', or 'ok'.
const RUN_WAIT_BODY: &str = r#"
local id, now = ARGV[1], ARGV[3]
local key = run_key(id)
local refused = fence(key, ARGV[2])
if refused then return refused end
local nb = ARGV[4]
if redis.call('HGET', key, 'wake_pending') == '1' then nb = now end
redis.call('HSET', key, 'state', 'waiting', 'wake_pending', '0')
redis.call('HDEL', key, 'lease_owner', 'lease_expires')
local timers = PFX .. 'timers:' .. redis.call('HGET', key, 'qk')
if nb ~= '' then
  redis.call('HSET', key, 'not_before', nb)
  redis.call('ZADD', timers, nb, id)
else
  redis.call('HDEL', key, 'not_before')
  redis.call('ZREM', timers, id)
end
bump(key, now)
return 'ok'
"#;

/// ARGV: run id, now (µs). Returns 'missing' or 'ok'.
const RUN_WAKE_BODY: &str = r#"
local id, now = ARGV[1], ARGV[2]
local key = run_key(id)
local state = redis.call('HGET', key, 'state')
if not state then return 'missing' end
if state == 'waiting' then
  redis.call('HSET', key, 'not_before', now)
  redis.call('ZADD', PFX .. 'timers:' .. redis.call('HGET', key, 'qk'), now, id)
  bump(key, now)
elseif state == 'running' then
  redis.call('HSET', key, 'wake_pending', '1')
  bump(key, now)
end
return 'ok'
"#;

/// ARGV: run id, epoch, now (µs), outcome (`completed`, `failed`, `cancelled`), result
/// JSON or error text, retryable flag. Returns `{'missing'}`, `{'stale'}`, or
/// `{'ok', fields}`.
const RUN_FINISH_BODY: &str = r#"
local id, now, kind = ARGV[1], ARGV[3], ARGV[4]
local key = run_key(id)
local refused = fence(key, ARGV[2])
if refused then return {refused} end
local h = redis.call('HMGET', key, 'attempt', 'max_attempts', 'cancel_requested', 'qk', 'seq')
local state = 'cancelled'
if kind == 'completed' then
  state = 'completed'
  redis.call('HSET', key, 'result', ARGV[5])
elseif kind == 'failed' then
  redis.call('HSET', key, 'error', ARGV[5])
  if ARGV[6] == '1' and tonumber(h[1]) < tonumber(h[2]) and h[3] ~= '1' then
    state = 'queued'
  else
    state = 'failed'
  end
end
redis.call('HSET', key, 'state', state, 'wake_pending', '0')
redis.call('HDEL', key, 'lease_owner', 'lease_expires')
bump(key, now)
redis.call('ZREM', PFX .. 'timers:' .. h[4], id)
if state == 'queued' then
  redis.call('ZADD', PFX .. 'ready:' .. h[4], h[5], id)
else
  settle(id, key, now)
end
return {'ok', redis.call('HGETALL', key)}
"#;

/// ARGV: run id, now (µs). Returns 'missing' or 'ok'.
const RUN_CANCEL_BODY: &str = r#"
local key = run_key(ARGV[1])
if redis.call('EXISTS', key) == 0 then return 'missing' end
cancel(ARGV[1], key, ARGV[2])
return 'ok'
"#;

/// ARGV: index (`all` or `queue:<hex>`), limit (0 for none), states (comma separated, ''
/// for any), then app, user, session, and concurrency key, each `=value` or '' for any.
/// Returns the matching runs' fields, oldest first.
const RUN_LIST_BODY: &str = r#"
local limit = tonumber(ARGV[2])
local states = {}
for state in string.gmatch(ARGV[3], '[^,]+') do states[state] = true end
local wanted = {ARGV[4], ARGV[5], ARGV[6], ARGV[7]}
local out = {}
for _, id in ipairs(redis.call('ZRANGE', PFX .. ARGV[1], 0, -1)) do
  local key = run_key(id)
  local h = redis.call('HMGET', key, 'state', 'app', 'user', 'session', 'ckey')
  local matches = h[1] and (ARGV[3] == '' or states[h[1]])
  for i = 1, 4 do
    if matches and wanted[i] ~= '' and (not h[i + 1] or '=' .. h[i + 1] ~= wanted[i]) then
      matches = false
    end
  end
  if matches then
    table.insert(out, redis.call('HGETALL', key))
    if limit > 0 and #out >= limit then break end
  end
end
return out
"#;

/// ARGV: cut-off (µs), batch size. Returns `{deleted, examined}`.
const RUN_PRUNE_BODY: &str = r#"
local ids = redis.call('ZRANGEBYSCORE', PFX .. 'finished', '-inf', '(' .. ARGV[1], 'LIMIT', 0,
  tonumber(ARGV[2]))
local deleted = 0
for _, id in ipairs(ids) do
  local key = run_key(id)
  local h = redis.call('HMGET', key, 'state', 'qk', 'ikey')
  if h[1] == 'queued' or h[1] == 'running' or h[1] == 'waiting' then
    redis.call('ZREM', PFX .. 'finished', id)
  else
    if h[2] and h[3] and redis.call('HGET', PFX .. 'idem:' .. h[2], h[3]) == id then
      redis.call('HDEL', PFX .. 'idem:' .. h[2], h[3])
    end
    if h[2] then redis.call('ZREM', PFX .. 'queue:' .. h[2], id) end
    redis.call('ZREM', PFX .. 'all', id)
    redis.call('ZREM', PFX .. 'finished', id)
    if h[1] then
      redis.call('DEL', key)
      deleted = deleted + 1
    end
  end
end
return {deleted, #ids}
"#;

/// The run scripts, each prefixed with [`RUN_PRELUDE`], built once.
static RUN_SCRIPTS: LazyLock<RunScripts> = LazyLock::new(RunScripts::new);

struct RunScripts {
    enqueue: String,
    claim: String,
    heartbeat: String,
    checkpoint: String,
    wait: String,
    wake: String,
    finish: String,
    cancel: String,
    list: String,
    prune: String,
}

impl RunScripts {
    fn new() -> Self {
        let script = |body: &str| format!("{RUN_PRELUDE}{body}");
        Self {
            enqueue: script(RUN_ENQUEUE_BODY),
            claim: script(RUN_CLAIM_BODY),
            heartbeat: script(RUN_HEARTBEAT_BODY),
            checkpoint: script(RUN_CHECKPOINT_BODY),
            wait: script(RUN_WAIT_BODY),
            wake: script(RUN_WAKE_BODY),
            finish: script(RUN_FINISH_BODY),
            cancel: script(RUN_CANCEL_BODY),
            list: script(RUN_LIST_BODY),
            prune: script(RUN_PRUNE_BODY),
        }
    }

    fn all(&self) -> [&str; 10] {
        [
            &self.enqueue,
            &self.claim,
            &self.heartbeat,
            &self.checkpoint,
            &self.wait,
            &self.wake,
            &self.finish,
            &self.cancel,
            &self.list,
            &self.prune,
        ]
    }
}

fn run_error(message: impl std::fmt::Display) -> AdkError {
    storage_error(RUN_STORAGE, message)
}

fn parse_state(text: &str) -> Result<RunState> {
    [
        RunState::Queued,
        RunState::Running,
        RunState::Waiting,
        RunState::Completed,
        RunState::Failed,
        RunState::Cancelled,
    ]
    .into_iter()
    .find(|state| state_name(*state) == text)
    .ok_or_else(|| run_error(format!("stored run state '{text}' is unknown")))
}

/// Rebuilds a [`RunRecord`] from a run hash's field/value pairs.
fn record_from_fields(pairs: Vec<String>) -> Result<RunRecord> {
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut pairs = pairs.into_iter();
    while let (Some(field), Some(value)) = (pairs.next(), pairs.next()) {
        fields.insert(field, value);
    }
    let base = fields.remove("base").ok_or_else(|| run_error("stored run has no 'base' field"))?;
    let mut record: RunRecord = serde_json::from_str(&base)
        .map_err(|e| run_error(format!("stored run 'base' is corrupt: {e}")))?;
    let mut take = |name: &str| fields.remove(name);
    let number = |text: Option<String>, name: &str| -> Result<u64> {
        let text = text.ok_or_else(|| run_error(format!("stored run has no '{name}' field")))?;
        text.parse().map_err(|_| run_error(format!("stored run '{name}' is corrupt: '{text}'")))
    };
    let time = |text: Option<String>, name: &str| -> Result<Option<DateTime<Utc>>> {
        text.map(|text| from_micros(&text, RUN_STORAGE, name)).transpose()
    };
    let json = |text: Option<String>, name: &str| -> Result<Option<serde_json::Value>> {
        text.map(|text| {
            serde_json::from_str(&text)
                .map_err(|e| run_error(format!("stored run '{name}' is corrupt: {e}")))
        })
        .transpose()
    };

    record.state = parse_state(&take("state").unwrap_or_default())?;
    record.attempt = u32::try_from(number(take("attempt"), "attempt")?)
        .map_err(|_| run_error("stored run 'attempt' is out of range"))?;
    record.lease_epoch = number(take("lease_epoch"), "lease_epoch")?;
    record.revision = number(take("revision"), "revision")?;
    record.lease_owner = take("lease_owner");
    record.lease_expires_at = time(take("lease_expires"), "lease_expires")?;
    record.heartbeat_at = time(take("heartbeat"), "heartbeat")?;
    record.not_before = time(take("not_before"), "not_before")?;
    record.updated_at = time(take("updated"), "updated")?
        .ok_or_else(|| run_error("stored run has no 'updated' field"))?;
    record.checkpoint = json(take("checkpoint"), "checkpoint")?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| run_error(format!("stored run 'checkpoint' is corrupt: {e}")))?;
    record.result = json(take("result"), "result")?;
    record.error = take("error");
    record.cancel_requested = take("cancel_requested").as_deref() == Some("1");
    record.wake_pending = take("wake_pending").as_deref() == Some("1");
    Ok(record)
}

fn policy_name(policy: OnConflict) -> &'static str {
    match policy {
        OnConflict::Reject => "reject",
        OnConflict::Interrupt => "interrupt",
        _ => "enqueue",
    }
}

/// Redis-backed [`DurableRunStore`].
///
/// Each run is one hash; ready runs, timers, key holders, and idempotency keys are
/// indexes kept by the same scripts that change the run. See the
/// [module documentation](self) for the layout and the script behind each operation.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::run_store::{DurableRunStore, NewRun, RunOutcome};
/// use adk_session::RedisRunStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let runs = RedisRunStore::new("redis://localhost:6379").await?;
/// runs.migrate().await?;
/// runs.enqueue(NewRun::new("default", "app", "user", "agent", serde_json::json!({}))).await?;
/// if let Some(claimed) = runs.claim("default", "worker-1", Duration::from_secs(30)).await? {
///     let id = &claimed.record.run_id;
///     runs.finish(id, claimed.epoch, RunOutcome::Completed(serde_json::json!("done"))).await?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisRunStore {
    client: Client,
    clock: Arc<dyn Clock>,
}

impl RedisRunStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(run_error)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides delays, waits, and lease expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `session.run_store.storage` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &RUN_SCRIPTS.all()).await.map_err(run_error)
    }

    fn now(&self) -> DateTime<Utc> {
        stored_now(self.clock.now())
    }

    async fn eval(&self, script: &str, args: Vec<String>) -> Result<Value> {
        self.client
            .eval(script, Vec::<String>::new(), args)
            .await
            .map_err(|e| run_error(format!("run store script failed: {e}")))
    }

    /// Maps a fenced script's `missing` or `stale` verdict to the contract error.
    fn refusal(verdict: &str, run_id: &str, epoch: u64) -> Option<AdkError> {
        match verdict {
            "missing" => Some(run_not_found_error(run_id)),
            "stale" => Some(run_stale_epoch_error(run_id, epoch)),
            _ => None,
        }
    }

    async fn fenced(
        &self,
        script: &str,
        run_id: &str,
        epoch: u64,
        extra: Vec<String>,
    ) -> Result<String> {
        let mut args = vec![run_id.to_string(), epoch.to_string(), micros(self.now())];
        args.extend(extra);
        let verdict = self
            .eval(script, args)
            .await?
            .into_string()
            .ok_or_else(|| run_error("run store script returned no verdict"))?;
        match Self::refusal(&verdict, run_id, epoch) {
            Some(error) => Err(error),
            None => Ok(verdict),
        }
    }
}

#[async_trait]
impl DurableRunStore for RedisRunStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        run.validate()?;
        let now = self.now();
        let run_id = uuid::Uuid::new_v4().to_string();
        let not_before = run.not_before.map(stored_now);
        let record = RunRecord::queued(run_id.clone(), NewRun { not_before, ..run }, now);
        let base = serde_json::to_string(&record)
            .map_err(|e| run_error(format!("run serialize failed: {e}")))?;

        let mut args = vec![
            run_id,
            hex(&record.queue),
            record.idempotency_key.clone().unwrap_or_default(),
            record.concurrency_key.as_deref().map(hex).unwrap_or_default(),
            policy_name(record.on_conflict).to_string(),
            micros(now),
            not_before.filter(|at| *at > now).map(micros).unwrap_or_default(),
        ];
        let mut field = |name: &str, value: String| args.extend([name.to_string(), value]);
        field("base", base);
        field("app", record.app_name.clone());
        field("user", record.user_id.clone());
        field("max_attempts", record.max_attempts.to_string());
        if let Some(session) = &record.session_id {
            field("session", session.clone());
        }
        if let Some(key) = &record.concurrency_key {
            field("ckey", key.clone());
        }
        if let Some(key) = &record.idempotency_key {
            field("ikey", key.clone());
        }
        if let Some(at) = not_before.filter(|at| *at <= now) {
            field("not_before", micros(at));
        }

        let (status, payload) =
            status_reply(self.eval(&RUN_SCRIPTS.enqueue, args).await?, RUN_STORAGE)?;
        match status.as_str() {
            "created" => Ok(Enqueued::Created(record_from_fields(strings(payload, RUN_STORAGE)?)?)),
            "existing" => {
                Ok(Enqueued::Existing(record_from_fields(strings(payload, RUN_STORAGE)?)?))
            }
            "rejected" => Ok(Enqueued::Rejected {
                blocking_run_id: payload
                    .into_string()
                    .ok_or_else(|| run_error("enqueue script named no blocking run"))?,
            }),
            other => Err(run_error(format!("unexpected enqueue script reply '{other}'"))),
        }
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        let now = self.now();
        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
        let reply = self
            .eval(
                &RUN_SCRIPTS.claim,
                vec![hex(queue), worker.to_string(), micros(now), micros(expires_at)],
            )
            .await?;
        if reply.is_null() {
            return Ok(None);
        }
        let record = record_from_fields(strings(reply, RUN_STORAGE)?)?;
        Ok(Some(ClaimedRun { epoch: record.lease_epoch, record }))
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        let expires_at = deadline(self.now(), ttl, "run_store.invalid_ttl")?;
        let flag =
            self.fenced(&RUN_SCRIPTS.heartbeat, run_id, epoch, vec![micros(expires_at)]).await?;
        Ok(flag == "1")
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: adk_core::run_store::RunCheckpointRef,
    ) -> Result<()> {
        let json = serde_json::to_string(&checkpoint)
            .map_err(|e| run_error(format!("checkpoint serialize failed: {e}")))?;
        self.fenced(&RUN_SCRIPTS.checkpoint, run_id, epoch, vec![json]).await.map(drop)
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        let until = until.map(stored_now).map(micros).unwrap_or_default();
        self.fenced(&RUN_SCRIPTS.wait, run_id, epoch, vec![until]).await.map(drop)
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        let verdict = self
            .eval(&RUN_SCRIPTS.wake, vec![run_id.to_string(), micros(self.now())])
            .await?
            .into_string();
        match verdict.as_deref() {
            Some("missing") => Err(run_not_found_error(run_id)),
            _ => Ok(()),
        }
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        let (kind, payload, retryable) = match outcome {
            RunOutcome::Completed(result) => (
                "completed",
                serde_json::to_string(&result)
                    .map_err(|e| run_error(format!("result serialize failed: {e}")))?,
                false,
            ),
            RunOutcome::Failed { error, retryable } => ("failed", error, retryable),
            RunOutcome::Cancelled => ("cancelled", String::new(), false),
        };
        let args = vec![
            run_id.to_string(),
            epoch.to_string(),
            micros(self.now()),
            kind.to_string(),
            payload,
            if retryable { "1" } else { "0" }.to_string(),
        ];
        let (status, fields) =
            status_reply(self.eval(&RUN_SCRIPTS.finish, args).await?, RUN_STORAGE)?;
        if let Some(error) = Self::refusal(&status, run_id, epoch) {
            return Err(error);
        }
        record_from_fields(strings(fields, RUN_STORAGE)?)
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        let verdict = self
            .eval(&RUN_SCRIPTS.cancel, vec![run_id.to_string(), micros(self.now())])
            .await?
            .into_string();
        match verdict.as_deref() {
            Some("missing") => Err(run_not_found_error(run_id)),
            _ => Ok(()),
        }
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        let fields: Vec<String> = self
            .client
            .hgetall::<HashMap<String, String>, _>(format!("{{adk:runs}}:run:{run_id}"))
            .await
            .map_err(|e| run_error(format!("run read failed: {e}")))?
            .into_iter()
            .flat_map(|(field, value)| [field, value])
            .collect();
        if fields.is_empty() {
            return Ok(None);
        }
        record_from_fields(fields).map(Some)
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        let wanted =
            |value: &Option<String>| value.as_ref().map(|v| format!("={v}")).unwrap_or_default();
        let index =
            filter.queue.as_deref().map_or("all".to_string(), |q| format!("queue:{}", hex(q)));
        let states: Vec<_> = filter.states.iter().map(|state| state_name(*state)).collect();
        let args = vec![
            index,
            filter.limit.unwrap_or(0).to_string(),
            states.join(","),
            wanted(&filter.app_name),
            wanted(&filter.user_id),
            wanted(&filter.session_id),
            wanted(&filter.concurrency_key),
        ];
        let reply = self.eval(&RUN_SCRIPTS.list, args).await?;
        reply
            .into_array()
            .into_iter()
            .map(|run| strings(run, RUN_STORAGE).and_then(record_from_fields))
            .collect()
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        let mut total = 0u64;
        loop {
            let reply = self
                .eval(&RUN_SCRIPTS.prune, vec![micros(finished_before), PRUNE_BATCH.to_string()])
                .await?;
            let counts = strings(reply, RUN_STORAGE)?;
            let count = |index: usize| counts.get(index).and_then(|text| text.parse::<u64>().ok());
            let (Some(deleted), Some(examined)) = (count(0), count(1)) else {
                return Err(run_error("prune script returned no counts"));
            };
            total += deleted;
            if examined < PRUNE_BATCH as u64 {
                return Ok(total);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

const SCHEDULE_DEFS: &str = "{adk:schedules}:defs";
const SCHEDULE_REVS: &str = "{adk:schedules}:revs";
const SCHEDULE_DUE: &str = "{adk:schedules}:due";

/// KEYS: defs, revs, due. ARGV: id, expected revision (0 when absent), new revision,
/// definition JSON, due score (µs) or ''. Returns 1 when written, 0 when the revision moved.
const SCHEDULE_CAS_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[2], ARGV[1])) or 0
if current ~= tonumber(ARGV[2]) then return 0 end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[4])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
if ARGV[5] ~= '' then
  redis.call('ZADD', KEYS[3], ARGV[5], ARGV[1])
else
  redis.call('ZREM', KEYS[3], ARGV[1])
end
return 1
"#;

/// KEYS: defs, revs, due. ARGV: id. Returns 1 when deleted, 0 when absent.
const SCHEDULE_DELETE_SCRIPT: &str = r#"
if redis.call('HDEL', KEYS[1], ARGV[1]) == 0 then return 0 end
redis.call('HDEL', KEYS[2], ARGV[1])
redis.call('ZREM', KEYS[3], ARGV[1])
return 1
"#;

fn schedule_error(message: impl std::fmt::Display) -> AdkError {
    storage_error(SCHEDULE_STORAGE, message)
}

fn schedule_keys() -> Vec<String> {
    vec![SCHEDULE_DEFS.to_string(), SCHEDULE_REVS.to_string(), SCHEDULE_DUE.to_string()]
}

fn parse_schedule(json: &str) -> Result<Schedule> {
    serde_json::from_str(json)
        .map_err(|e| schedule_error(format!("stored schedule is corrupt: {e}")))
}

/// Redis-backed [`ScheduleStore`].
///
/// Definitions and revisions live in two hashes keyed by schedule id, and unpaused
/// schedules with a next fire time sit in a sorted set by that time, which `due` reads.
/// Every change is a compare-and-swap on the revision inside one Lua script.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_core::run_store::NewRun;
/// use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec, ScheduleStore};
/// use adk_session::RedisScheduleStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let schedules = RedisScheduleStore::new("redis://localhost:6379").await?;
/// schedules.migrate().await?;
/// let heartbeat = Schedule::new(
///     "heartbeat",
///     ScheduleSpec::Every(Duration::from_secs(60)),
///     NewRun::new("default", "ops", "system", "probe", serde_json::json!({})),
///     MissedFirePolicy::Skip,
/// )
/// .with_next_fire_at(chrono::Utc::now());
/// schedules.upsert(heartbeat).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisScheduleStore {
    client: Client,
}

impl RedisScheduleStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(schedule_error)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `session.schedule_store.storage` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &[SCHEDULE_CAS_SCRIPT, SCHEDULE_DELETE_SCRIPT])
            .await
            .map_err(schedule_error)
    }

    /// Reads a schedule. Its JSON carries the revision, written by the same script that
    /// writes the revision hash.
    async fn read(&self, id: &str) -> Result<Option<Schedule>> {
        let json: Option<String> = self
            .client
            .hget(SCHEDULE_DEFS, id)
            .await
            .map_err(|e| schedule_error(format!("schedule read failed: {e}")))?;
        json.as_deref().map(parse_schedule).transpose()
    }

    /// Writes `schedule` (with its new revision) when the stored revision is `expected`.
    async fn write(&self, schedule: &Schedule, expected: u64) -> Result<bool> {
        let json = serde_json::to_string(schedule)
            .map_err(|e| schedule_error(format!("schedule serialize failed: {e}")))?;
        let due = if schedule.paused {
            String::new()
        } else {
            schedule.next_fire_at.map(micros).unwrap_or_default()
        };
        let written: i64 = self
            .client
            .eval(
                SCHEDULE_CAS_SCRIPT,
                schedule_keys(),
                vec![
                    schedule.id.clone(),
                    expected.to_string(),
                    schedule.revision.to_string(),
                    json,
                    due,
                ],
            )
            .await
            .map_err(|e| schedule_error(format!("schedule write failed: {e}")))?;
        Ok(written == 1)
    }

    /// Applies `change` to the stored schedule with a revision compare-and-swap, retrying
    /// when another writer changed it first. `change` returns `false` for no change.
    async fn update(&self, id: &str, change: impl Fn(&mut Schedule) -> bool) -> Result<Schedule> {
        for _ in 0..CAS_ATTEMPTS {
            let mut schedule = self.read(id).await?.ok_or_else(|| schedule_not_found_error(id))?;
            let expected = schedule.revision;
            if !change(&mut schedule) {
                return Ok(schedule);
            }
            schedule.revision = expected + 1;
            if self.write(&schedule, expected).await? {
                return Ok(schedule);
            }
        }
        Err(schedule_error(format!(
            "schedule '{id}' lost {CAS_ATTEMPTS} compare-and-swap attempts to concurrent writers"
        )))
    }
}

#[async_trait]
impl ScheduleStore for RedisScheduleStore {
    async fn upsert(&self, schedule: Schedule) -> Result<Schedule> {
        schedule.validate()?;
        let schedule = Schedule { next_fire_at: schedule.next_fire_at.map(stored_now), ..schedule };
        for _ in 0..CAS_ATTEMPTS {
            let stored = self.read(&schedule.id).await?;
            if let Some(stored) = &stored
                && same_definition(stored, &schedule)
            {
                return Ok(stored.clone());
            }
            let expected = stored.map_or(0, |stored| stored.revision);
            let next = Schedule { revision: expected + 1, ..schedule.clone() };
            if self.write(&next, expected).await? {
                return Ok(next);
            }
        }
        Err(schedule_error(format!(
            "schedule '{}' lost {CAS_ATTEMPTS} compare-and-swap attempts to concurrent writers",
            schedule.id
        )))
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        self.read(id).await
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        let all: HashMap<String, String> = self
            .client
            .hgetall(SCHEDULE_DEFS)
            .await
            .map_err(|e| schedule_error(format!("schedule read failed: {e}")))?;
        let mut schedules =
            all.values().map(|json| parse_schedule(json)).collect::<Result<Vec<_>>>()?;
        schedules.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(schedules)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let deleted: i64 = self
            .client
            .eval(SCHEDULE_DELETE_SCRIPT, schedule_keys(), vec![id.to_string()])
            .await
            .map_err(|e| schedule_error(format!("schedule delete failed: {e}")))?;
        if deleted == 0 {
            return Err(schedule_not_found_error(id));
        }
        Ok(())
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
        self.update(id, |schedule| {
            let changed = schedule.paused != paused;
            schedule.paused = paused;
            changed
        })
        .await
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = self
            .client
            .zrangebyscore(
                SCHEDULE_DUE,
                "-inf",
                now.timestamp_micros(),
                false,
                Some((0, i64::try_from(limit).unwrap_or(i64::MAX))),
            )
            .await
            .map_err(|e| schedule_error(format!("schedule due read failed: {e}")))?;
        let mut due = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(schedule) = self.read(&id).await? {
                due.push(schedule);
            }
        }
        Ok(due)
    }

    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let mut schedule = self.read(id).await?.ok_or_else(|| schedule_not_found_error(id))?;
        if schedule.revision != expected_revision {
            return Ok(false);
        }
        schedule.next_fire_at = next_fire_at.map(stored_now);
        schedule.revision = expected_revision + 1;
        self.write(&schedule, expected_revision).await
    }
}

// ---------------------------------------------------------------------------
// Governance state
// ---------------------------------------------------------------------------

const GOVERNANCE_STATES: &str = "{adk:governance}:states";
const GOVERNANCE_EPOCHS: &str = "{adk:governance}:epochs";

/// KEYS: states, epochs. ARGV: scope key, expected epoch, state JSON at the next epoch.
/// Returns `{1, epoch}` when written, or `{0, current epoch}` when the epoch moved.
const GOVERNANCE_SET_SCRIPT: &str = r#"
local current = tonumber(redis.call('HGET', KEYS[2], ARGV[1])) or 0
if current ~= tonumber(ARGV[2]) then return {0, current} end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
redis.call('HINCRBY', KEYS[2], ARGV[1], 1)
return {1, current + 1}
"#;

fn governance_error(message: impl std::fmt::Display) -> AdkError {
    storage_error(GOVERNANCE_STORAGE, message)
}

fn parse_governance(json: &str) -> Result<GovernanceState> {
    serde_json::from_str(json)
        .map_err(|e| governance_error(format!("stored governance state is corrupt: {e}")))
}

/// Redis-backed [`GovernanceStateStore`].
///
/// States live in one hash keyed by [`GovernanceScope::key`], with each scope's epoch in a
/// second hash; `set` compares and bumps the epoch inside one Lua script.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::governance_state::{GovernanceScope, GovernanceStateStore};
/// use adk_core::principal::SubjectRef;
/// use adk_session::RedisGovernanceStateStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let governance = RedisGovernanceStateStore::new("redis://localhost:6379").await?;
/// governance.migrate().await?;
/// let epoch = governance.get(&GovernanceScope::Org).await?.map_or(0, |state| state.epoch);
/// governance
///     .set(GovernanceScope::Org, epoch, true, Some("incident".into()), Some(SubjectRef::human("oncall")))
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisGovernanceStateStore {
    client: Client,
    clock: Arc<dyn Clock>,
}

impl RedisGovernanceStateStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect(url).await.map_err(governance_error)?))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps `issued_at`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Loads the store's script into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `session.governance_store.storage` when the server rejects the script.
    pub async fn migrate(&self) -> Result<()> {
        load_scripts(&self.client, &[GOVERNANCE_SET_SCRIPT]).await.map_err(governance_error)
    }
}

#[async_trait]
impl GovernanceStateStore for RedisGovernanceStateStore {
    async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
        let json: Option<String> = self
            .client
            .hget(GOVERNANCE_STATES, scope.key())
            .await
            .map_err(|e| governance_error(format!("governance read failed: {e}")))?;
        json.as_deref().map(parse_governance).transpose()
    }

    async fn set(
        &self,
        scope: GovernanceScope,
        expected_epoch: u64,
        frozen: bool,
        reason: Option<String>,
        issued_by: Option<SubjectRef>,
    ) -> Result<GovernanceState> {
        let state = GovernanceState {
            scope,
            frozen,
            reason,
            issued_by,
            issued_at: stored_now(self.clock.now()),
            epoch: expected_epoch + 1,
        };
        let json = serde_json::to_string(&state)
            .map_err(|e| governance_error(format!("governance serialize failed: {e}")))?;
        let reply: Vec<i64> = self
            .client
            .eval(
                GOVERNANCE_SET_SCRIPT,
                vec![GOVERNANCE_STATES.to_string(), GOVERNANCE_EPOCHS.to_string()],
                vec![state.scope.key(), expected_epoch.to_string(), json],
            )
            .await
            .map_err(|e| governance_error(format!("governance write failed: {e}")))?;
        match reply.as_slice() {
            [1, _] => Ok(state),
            [0, current] => Err(governance_conflict_error(
                &state.scope,
                expected_epoch,
                u64::try_from(*current).unwrap_or(0),
            )),
            other => Err(governance_error(format!("unexpected governance script reply {other:?}"))),
        }
    }

    async fn list(&self) -> Result<Vec<GovernanceState>> {
        let all: HashMap<String, String> = self
            .client
            .hgetall(GOVERNANCE_STATES)
            .await
            .map_err(|e| governance_error(format!("governance read failed: {e}")))?;
        let mut states: Vec<(String, GovernanceState)> = all
            .into_iter()
            .map(|(key, json)| parse_governance(&json).map(|state| (key, state)))
            .collect::<Result<_>>()?;
        states.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(states.into_iter().map(|(_, state)| state).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn records_round_trip_through_hash_fields() {
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let mut record = RunRecord::queued(
            "run-1",
            NewRun::new("q/ü", "app", "user", "agent", serde_json::json!({ "x": {} })),
            now,
        );
        let base = serde_json::to_string(&record).unwrap();
        record.state = RunState::Running;
        record.attempt = 2;
        record.lease_epoch = 3;
        record.lease_owner = Some("w".into());
        record.lease_expires_at = Some(now + chrono::Duration::seconds(30));
        record.revision = 4;
        record.result = Some(serde_json::json!({ "empty": {}, "list": [] }));
        let fields = [
            ("base", base),
            ("state", "running".to_string()),
            ("attempt", "2".to_string()),
            ("lease_epoch", "3".to_string()),
            ("revision", "4".to_string()),
            ("lease_owner", "w".to_string()),
            ("lease_expires", micros(now + chrono::Duration::seconds(30))),
            ("updated", micros(now)),
            ("result", r#"{"empty":{},"list":[]}"#.to_string()),
            ("cancel_requested", "0".to_string()),
        ]
        .into_iter()
        .flat_map(|(field, value)| [field.to_string(), value])
        .collect();
        assert_eq!(record_from_fields(fields).unwrap(), record);
    }

    #[test]
    fn keys_cannot_move_hash_slots() {
        assert_eq!(lease_key("a}{b"), "adk:lease:{617d7b62}");
        assert_eq!(parse_state("waiting").unwrap(), RunState::Waiting);
        assert!(parse_state("paused").is_err());
        assert_eq!(policy_name(OnConflict::Interrupt), "interrupt");
    }
}
