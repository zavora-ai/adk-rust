//! The Redis edge stores (spec 3.5 §7.9, feature `awp-redis`).
//!
//! Keys sit under `adk:awp:{<site>}`, so one site's keys share a Redis Cluster slot; braces in
//! the site are percent-encoded.
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `…:subs` | Hash | Each subscription record as JSON, by id |
//! | `…:consent` | Hash | Each consent record (receipt and revocation time) as JSON, by id |
//! | `…:subject:<subject>` | Set | The receipt ids of one subject |
//! | `…:status` | String | The current signed status |
//! | `…:held` | Hash | Each held call as JSON, by id |
//! | `…:leads` | Hash | Each lead as JSON, by id |
//! | `…:idem` | Hash | Each idempotency entry (digest, start, outcome) as JSON, by key |
//! | `…:events`, `…:event` | Sorted set, Hash | Retained event ids by time in microseconds; each event and its audience as JSON, by id |
//! | `…:pages` | Hash | Each further `catalog.changed` page with its time, by `{seq}:{page}` |
//! | `…:consent-refs:<receipt>` | Set | The requesters that referenced one consent receipt |
//! | `…:deliveries`, `…:delivery` | Sorted set, Hash | Pending deliveries by due time; their failures so far |
//! | `…:beacon:<day>`, `…:beacon-days` | Hash, Sorted set | One day's counts by metric; the days that have counts |
//! | `…:visitors:<day>`, `…:visitor-days` | Set, Sorted set | One day's consenting visitors; the days that have them |
//! | `adk:awp:layout` | String | The layout version `migrate` recorded |
//!
//! Creates are `HSETNX`; every change is a compare-and-set Lua script that writes only when
//! the stored JSON is the one read, retried under contention; the status is `SET … GET`.
//! Listings read the hash and filter, which suits one site's edge.

use adk_awp::v2::store::{
    ConsentReceiptStore, ConsentRecord, HeldCallRecord, HeldCallStore, HeldCallTransition,
    IdempotencyBegin, IdempotencyStore, Lead, LeadStore, RecordedOutcome, StatusStore, StoreError,
    StoreResult, SubscriptionRecord, SubscriptionStore, SubscriptionUpdate,
};
use async_trait::async_trait;
use awp_types::{
    CatalogChangesPage, ConsentReceipt, ConsentScope, EventType, HeldCallState, LeadRef,
    RequesterId, SiteStatus, SubscriptionState,
};
use chrono::{DateTime, Datelike as _, NaiveDate, Utc};
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, KeysInterface as _,
    LuaInterface as _, SetsInterface as _,
};
use uuid::Uuid;

use super::{
    CAS_ATTEMPTS, DEFAULT_SITE, IdempotencyEntry, apply_delivery, apply_transition, apply_update,
    backend, decode, encode,
};
use crate::awp::beacon::BeaconStore;
use crate::awp::events::{EventLog, PendingDelivery, RetainedEvent};

const NAME: &str = "redis";
/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// KEYS: the hash. ARGV: field, expected JSON, new JSON. Writes when the field holds the
/// expected JSON; returns 1 when written, 0 when it changed, -1 when it is absent.
const CAS_SCRIPT: &str = r"
local current = redis.call('HGET', KEYS[1], ARGV[1])
if not current then return -1 end
if current ~= ARGV[2] then return 0 end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
return 1
";

/// KEYS: the hash. ARGV: field, entry. Stores the entry when the field is absent and returns
/// nil; otherwise returns the stored entry.
const BEGIN_SCRIPT: &str = r"
local current = redis.call('HGET', KEYS[1], ARGV[1])
if current then return current end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
return false
";

/// KEYS: event times, events. ARGV: id, time, entry.
const APPEND_SCRIPT: &str = r"
if redis.call('HSETNX', KEYS[2], ARGV[1], ARGV[3]) == 1 then
  redis.call('ZADD', KEYS[1], ARGV[2], ARGV[1])
end
return 1
";

/// KEYS: a sorted set and a hash. ARGV: min and max score. The hash's values for the members
/// in the score range, in score then member order.
const RANGE_VALUES_SCRIPT: &str = r"
local ids = redis.call('ZRANGEBYSCORE', KEYS[1], ARGV[1], ARGV[2])
local out = {}
for _, id in ipairs(ids) do
  local value = redis.call('HGET', KEYS[2], id)
  if value then table.insert(out, value) end
end
return out
";

/// KEYS: a sorted set. ARGV: min and max score. The members in the range.
const RANGE_SCRIPT: &str = r"
return redis.call('ZRANGEBYSCORE', KEYS[1], ARGV[1], ARGV[2])
";

/// KEYS: event times, events, pages. ARGV: the exclusive cutoff (`(<micros>`), the cutoff.
/// Drops older events and pages; returns how many events.
const PURGE_SCRIPT: &str = r"
local ids = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
for i = 1, #ids, 500 do
  local chunk = {unpack(ids, i, math.min(i + 499, #ids))}
  redis.call('ZREM', KEYS[1], unpack(chunk))
  redis.call('HDEL', KEYS[2], unpack(chunk))
end
local pages = redis.call('HGETALL', KEYS[3])
for i = 1, #pages, 2 do
  if tonumber(cjson.decode(pages[i + 1]).atUs) < tonumber(ARGV[2]) then
    redis.call('HDEL', KEYS[3], pages[i])
  end
end
return #ids
";

/// KEYS: pages. ARGV: the release, then field and entry pairs. Replaces the release's pages.
const PAGES_SCRIPT: &str = r"
local prefix = ARGV[1] .. ':'
for _, field in ipairs(redis.call('HKEYS', KEYS[1])) do
  if string.sub(field, 1, #prefix) == prefix then redis.call('HDEL', KEYS[1], field) end
end
for i = 2, #ARGV, 2 do redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1]) end
return 1
";

/// KEYS: due times, failures. ARGV: member, due, failures.
const SAVE_DELIVERY_SCRIPT: &str = r"
redis.call('ZADD', KEYS[1], ARGV[2], ARGV[1])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
return 1
";

/// KEYS: due times, failures. ARGV: now, the new due time. Leases every due delivery and
/// returns member and failures pairs.
const CLAIM_SCRIPT: &str = r"
local due = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
local out = {}
for _, member in ipairs(due) do
  redis.call('ZADD', KEYS[1], ARGV[2], member)
  table.insert(out, member)
  table.insert(out, redis.call('HGET', KEYS[2], member) or '0')
end
return out
";

/// KEYS: due times, failures. ARGV: member.
const FINISH_DELIVERY_SCRIPT: &str = r"
redis.call('ZREM', KEYS[1], ARGV[1])
redis.call('HDEL', KEYS[2], ARGV[1])
return 1
";

/// KEYS: the day's counts, the days. ARGV: metric, count, day.
const BEACON_ADD_SCRIPT: &str = r"
redis.call('HINCRBY', KEYS[1], ARGV[1], ARGV[2])
redis.call('ZADD', KEYS[2], ARGV[3], ARGV[3])
return 1
";

/// KEYS: the day's visitors, the days. ARGV: visitor, day.
const VISITOR_ADD_SCRIPT: &str = r"
redis.call('SADD', KEYS[1], ARGV[1])
redis.call('ZADD', KEYS[2], ARGV[2], ARGV[2])
return 1
";

/// KEYS: layout. ARGV: this release's layout.
const MIGRATE_SCRIPT: &str = r"
local stored = tonumber(redis.call('GET', KEYS[1]) or '0')
local mine = tonumber(ARGV[1])
if stored > mine then return stored end
if stored < mine then redis.call('SET', KEYS[1], ARGV[1]) end
return mine
";

/// The six edge stores, the event log and the beacon store in Redis, shared by every edge
/// replica (spec 3.5 §7.9, feature
/// `awp-redis`). See the module documentation for its keys.
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::awp::stores::RedisEdgeStores;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stores = RedisEdgeStores::connect("redis://localhost:6379").await?.with_site("example.com");
/// stores.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisEdgeStores {
    client: Client,
    site: String,
}

fn failed(action: &str) -> impl FnOnce(fred::error::Error) -> StoreError + '_ {
    move |error| backend(NAME, action, &error)
}

impl RedisEdgeStores {
    /// The stores over an initialized client, in the default site namespace.
    pub fn from_client(client: Client) -> Self {
        Self { client, site: DEFAULT_SITE.to_owned() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] when the URL is invalid or the server cannot be reached.
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let config = Config::from_url(url).map_err(failed("parse the Redis URL"))?;
        let client = Builder::from_config(config).build().map_err(failed("build the client"))?;
        client.init().await.map_err(failed("connect"))?;
        Ok(Self::from_client(client))
    }

    /// Keeps the records under `site`.
    #[must_use]
    pub fn with_site(mut self, site: impl Into<String>) -> Self {
        self.site = site.into();
        self
    }

    /// Records the layout version, refusing keys a newer release wrote.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] when the server fails or holds a newer layout.
    pub async fn migrate(&self) -> StoreResult<()> {
        let stored: i64 = self
            .client
            .eval(MIGRATE_SCRIPT, vec!["adk:awp:layout".to_owned()], vec![LAYOUT.to_string()])
            .await
            .map_err(failed("record the layout version"))?;
        if stored > LAYOUT {
            return Err(backend(
                NAME,
                "open its keys",
                &format!("they have layout v{stored}, newer than v{LAYOUT}; upgrade ADK-Rust"),
            ));
        }
        Ok(())
    }

    /// The stores as the edge takes them: the six edge stores, the event log and the beacon
    /// store.
    pub fn edge_stores(&self) -> crate::awp::EdgeStores {
        let this = std::sync::Arc::new(self.clone());
        crate::awp::EdgeStores::new(this.clone(), this.clone(), this.clone(), this.clone())
            .with_subscriptions(this.clone())
            .with_leads(this.clone())
            .with_event_log(this.clone())
            .with_beacon(this)
    }

    fn key(&self, suffix: &str) -> String {
        let site = self.site.replace('%', "%25").replace('{', "%7B").replace('}', "%7D");
        format!("adk:awp:{{{site}}}:{suffix}")
    }

    async fn field(&self, hash: &str, field: &str) -> StoreResult<Option<String>> {
        self.client.hget(self.key(hash), field).await.map_err(failed("read a record"))
    }

    async fn values(&self, hash: &str) -> StoreResult<Vec<String>> {
        self.client.hvals(self.key(hash)).await.map_err(failed("list records"))
    }

    async fn insert_new(
        &self,
        hash: &str,
        field: &str,
        value: String,
        what: &str,
    ) -> StoreResult<()> {
        let stored: bool = self
            .client
            .hsetnx(self.key(hash), field, value)
            .await
            .map_err(failed("store a record"))?;
        if stored {
            Ok(())
        } else {
            Err(StoreError::Conflict(format!("{what} {field} already exists")))
        }
    }

    /// Changes the JSON record `field` of `hash` with `change`, by compare-and-set.
    async fn change<T, F>(&self, hash: &str, field: &str, what: &str, change: F) -> StoreResult<T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: Fn(T) -> StoreResult<Option<T>>,
    {
        for _ in 0..CAS_ATTEMPTS {
            let Some(current) = self.field(hash, field).await? else {
                return Err(StoreError::NotFound(format!("{what} {field}")));
            };
            let Some(next) = change(decode(NAME, &current)?)? else {
                return Err(StoreError::NotFound(format!("{what} {field}")));
            };
            let written: i64 = self
                .client
                .eval(
                    CAS_SCRIPT,
                    vec![self.key(hash)],
                    vec![field.to_owned(), current, encode(NAME, &next)?],
                )
                .await
                .map_err(failed("change a record"))?;
            match written {
                1 => return Ok(next),
                -1 => return Err(StoreError::NotFound(format!("{what} {field}"))),
                _ => {}
            }
        }
        Err(StoreError::Conflict(format!("{what} {field} kept changing; retry")))
    }

    async fn all<T: serde::de::DeserializeOwned>(&self, hash: &str) -> StoreResult<Vec<T>> {
        self.values(hash).await?.iter().map(|value| decode(NAME, value)).collect()
    }

    async fn script<R: fred::types::FromValue>(
        &self,
        script: &str,
        keys: &[&str],
        args: Vec<String>,
        action: &str,
    ) -> StoreResult<R> {
        let keys: Vec<String> = keys.iter().map(|suffix| self.key(suffix)).collect();
        self.client.eval(script, keys, args).await.map_err(failed(action))
    }

    /// The days in `[from, to)` that `days` lists.
    async fn days(&self, days: &str, from: NaiveDate, to: NaiveDate) -> StoreResult<Vec<i64>> {
        let found: Vec<String> = self
            .script(
                RANGE_SCRIPT,
                &[days],
                vec![day_number(from).to_string(), format!("({}", day_number(to))],
                "list counted days",
            )
            .await?;
        Ok(found.iter().filter_map(|day| day.parse().ok()).collect())
    }
}

/// A day as stored: days from the common era.
fn day_number(day: NaiveDate) -> i64 {
    i64::from(day.num_days_from_ce())
}

/// A retained event as stored.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEvent {
    event: awp_types::EventEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audience: Option<std::collections::BTreeSet<RequesterId>>,
}

/// A catalogue page as stored, with when it was.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPage {
    at_us: i64,
    page: CatalogChangesPage,
}

/// The member a pending delivery is kept under.
fn delivery_member(subscription: &str, event_id: &str) -> String {
    format!("{subscription}\u{1f}{event_id}")
}

#[async_trait]
impl SubscriptionStore for RedisEdgeStores {
    async fn create(&self, record: SubscriptionRecord) -> StoreResult<()> {
        let id = record.subscription.id.clone();
        self.insert_new("subs", &id, encode(NAME, &record)?, "subscription").await
    }

    async fn get(&self, owner: &RequesterId, id: &str) -> StoreResult<Option<SubscriptionRecord>> {
        let record: Option<SubscriptionRecord> =
            self.field("subs", id).await?.map(|text| decode(NAME, &text)).transpose()?;
        Ok(record.filter(|record| &record.owner == owner))
    }

    async fn list(&self, owner: &RequesterId) -> StoreResult<Vec<SubscriptionRecord>> {
        let mut records: Vec<SubscriptionRecord> = self
            .all::<SubscriptionRecord>("subs")
            .await?
            .into_iter()
            .filter(|record| &record.owner == owner)
            .collect();
        records.sort_by(|a, b| a.subscription.id.cmp(&b.subscription.id));
        Ok(records)
    }

    async fn active_for(&self, event_type: &EventType) -> StoreResult<Vec<SubscriptionRecord>> {
        let mut records: Vec<SubscriptionRecord> = self
            .all::<SubscriptionRecord>("subs")
            .await?
            .into_iter()
            .filter(|record| {
                record.subscription.state == SubscriptionState::Active
                    && record.subscription.types.contains(event_type)
            })
            .collect();
        records.sort_by(|a, b| a.subscription.id.cmp(&b.subscription.id));
        Ok(records)
    }

    async fn update(
        &self,
        owner: &RequesterId,
        id: &str,
        update: SubscriptionUpdate,
    ) -> StoreResult<SubscriptionRecord> {
        self.change("subs", id, "subscription", |mut record: SubscriptionRecord| {
            if &record.owner != owner {
                return Ok(None);
            }
            apply_update(&mut record, update.clone());
            Ok(Some(record))
        })
        .await
    }

    async fn record_delivery(&self, id: &str, delivered: bool) -> StoreResult<SubscriptionRecord> {
        self.change("subs", id, "subscription", |mut record: SubscriptionRecord| {
            apply_delivery(&mut record, delivered);
            Ok(Some(record))
        })
        .await
    }

    async fn delete(&self, owner: &RequesterId, id: &str) -> StoreResult<bool> {
        if SubscriptionStore::get(self, owner, id).await?.is_none() {
            return Ok(false);
        }
        let removed: i64 = self
            .client
            .hdel(self.key("subs"), id)
            .await
            .map_err(failed("delete a subscription"))?;
        Ok(removed > 0)
    }
}

#[async_trait]
impl ConsentReceiptStore for RedisEdgeStores {
    async fn insert(&self, receipt: ConsentReceipt) -> StoreResult<()> {
        let id = receipt.id.to_string();
        let subject = self.key(&format!("subject:{}", receipt.subject_ref));
        let record = ConsentRecord { receipt, revoked_at: None };
        self.insert_new("consent", &id, encode(NAME, &record)?, "consent receipt").await?;
        let _: i64 = self.client.sadd(subject, id).await.map_err(failed("index a receipt"))?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> StoreResult<Option<ConsentRecord>> {
        self.field("consent", &id.to_string()).await?.map(|text| decode(NAME, &text)).transpose()
    }

    async fn for_subject(&self, subject_ref: &str) -> StoreResult<Vec<ConsentRecord>> {
        let ids: Vec<String> = self
            .client
            .smembers(self.key(&format!("subject:{subject_ref}")))
            .await
            .map_err(failed("list a subject's receipts"))?;
        let mut records = Vec::new();
        for id in ids {
            if let Some(text) = self.field("consent", &id).await? {
                records.push(decode::<ConsentRecord>(NAME, &text)?);
            }
        }
        records.sort_by(|a, b| {
            (a.receipt.granted_at, a.receipt.id).cmp(&(b.receipt.granted_at, b.receipt.id))
        });
        Ok(records)
    }

    async fn revoke(&self, id: Uuid, at: DateTime<Utc>) -> StoreResult<Option<ConsentRecord>> {
        match self
            .change("consent", &id.to_string(), "consent receipt", |mut record: ConsentRecord| {
                record.revoked_at.get_or_insert(at);
                Ok(Some(record))
            })
            .await
        {
            Ok(record) => Ok(Some(record)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn revoke_scope(
        &self,
        subject_ref: &str,
        scope: &ConsentScope,
        at: DateTime<Utc>,
    ) -> StoreResult<Vec<ConsentRecord>> {
        let mut revoked = Vec::new();
        for record in self.for_subject(subject_ref).await? {
            if record.revoked_at.is_some() || &record.receipt.scope != scope {
                continue;
            }
            let id = record.receipt.id.to_string();
            let changed = self
                .change("consent", &id, "consent receipt", |mut record: ConsentRecord| {
                    if record.revoked_at.is_some() {
                        return Ok(Some(record));
                    }
                    record.revoked_at = Some(at);
                    Ok(Some(record))
                })
                .await?;
            if changed.revoked_at == Some(at) {
                revoked.push(changed);
            }
        }
        revoked.sort_by_key(|record| record.receipt.id);
        Ok(revoked)
    }

    async fn erase(&self, subject_ref: &str) -> StoreResult<usize> {
        let subject = self.key(&format!("subject:{subject_ref}"));
        let ids: Vec<String> =
            self.client.smembers(&subject).await.map_err(failed("list a subject's receipts"))?;
        let mut erased = 0;
        for id in ids {
            let removed: i64 = self
                .client
                .hdel(self.key("consent"), id)
                .await
                .map_err(failed("erase a receipt"))?;
            erased += usize::try_from(removed).unwrap_or(0);
        }
        let _: i64 = self.client.del(subject).await.map_err(failed("erase a subject"))?;
        Ok(erased)
    }
}

#[async_trait]
impl StatusStore for RedisEdgeStores {
    async fn current(&self) -> StoreResult<Option<SiteStatus>> {
        let text: Option<String> =
            self.client.get(self.key("status")).await.map_err(failed("read the status"))?;
        text.map(|text| decode(NAME, &text)).transpose()
    }

    async fn set(&self, status: SiteStatus) -> StoreResult<Option<SiteStatus>> {
        let previous: Option<String> = self
            .client
            .set(self.key("status"), encode(NAME, &status)?, None, None, true)
            .await
            .map_err(failed("store the status"))?;
        previous.map(|text| decode(NAME, &text)).transpose()
    }
}

#[async_trait]
impl HeldCallStore for RedisEdgeStores {
    async fn create(&self, record: HeldCallRecord) -> StoreResult<()> {
        if record.call.state != HeldCallState::Pending {
            return Err(StoreError::Conflict("a held call starts pending".to_owned()));
        }
        let id = record.call.id.clone();
        self.insert_new("held", &id, encode(NAME, &record)?, "held call").await
    }

    async fn get(&self, id: &str) -> StoreResult<Option<HeldCallRecord>> {
        self.field("held", id).await?.map(|text| decode(NAME, &text)).transpose()
    }

    async fn pending(&self) -> StoreResult<Vec<HeldCallRecord>> {
        let mut records: Vec<HeldCallRecord> = self
            .all::<HeldCallRecord>("held")
            .await?
            .into_iter()
            .filter(|record| record.call.state == HeldCallState::Pending)
            .collect();
        records
            .sort_by(|a, b| (a.call.created_at, &a.call.id).cmp(&(b.call.created_at, &b.call.id)));
        Ok(records)
    }

    async fn transition(
        &self,
        id: &str,
        transition: HeldCallTransition,
    ) -> StoreResult<HeldCallRecord> {
        self.change("held", id, "held call", |record: HeldCallRecord| {
            apply_transition(record, transition.clone()).map(Some)
        })
        .await
    }
}

#[async_trait]
impl LeadStore for RedisEdgeStores {
    async fn insert(&self, lead: Lead) -> StoreResult<()> {
        let id = lead.id.0.clone();
        self.insert_new("leads", &id, encode(NAME, &lead)?, "lead").await
    }

    async fn get(&self, id: &LeadRef) -> StoreResult<Option<Lead>> {
        self.field("leads", &id.0).await?.map(|text| decode(NAME, &text)).transpose()
    }

    async fn since(&self, since: DateTime<Utc>) -> StoreResult<Vec<Lead>> {
        let mut leads: Vec<Lead> = self
            .all::<Lead>("leads")
            .await?
            .into_iter()
            .filter(|lead| lead.captured_at >= since)
            .collect();
        leads.sort_by(|a, b| (a.captured_at, &a.id).cmp(&(b.captured_at, &b.id)));
        Ok(leads)
    }

    async fn set_score(&self, id: &LeadRef, score: f64) -> StoreResult<()> {
        self.change("leads", &id.0, "lead", |mut lead: Lead| {
            lead.score = Some(score);
            Ok(Some(lead))
        })
        .await
        .map(|_| ())
    }

    async fn delete(&self, id: &LeadRef) -> StoreResult<bool> {
        let removed: i64 =
            self.client.hdel(self.key("leads"), &id.0).await.map_err(failed("delete a lead"))?;
        Ok(removed > 0)
    }
}

#[async_trait]
impl IdempotencyStore for RedisEdgeStores {
    async fn begin(
        &self,
        key: &str,
        input_digest: &str,
        at: DateTime<Utc>,
    ) -> StoreResult<IdempotencyBegin> {
        let entry = IdempotencyEntry {
            digest: input_digest.to_owned(),
            began_us: at.timestamp_micros(),
            outcome: None,
        };
        let stored: Option<String> = self
            .client
            .eval(BEGIN_SCRIPT, vec![self.key("idem")], vec![key.to_owned(), encode(NAME, &entry)?])
            .await
            .map_err(failed("begin an idempotent call"))?;
        let Some(stored) = stored else { return Ok(IdempotencyBegin::Started) };
        let stored: IdempotencyEntry = decode(NAME, &stored)?;
        Ok(match stored {
            IdempotencyEntry { digest, .. } if digest != input_digest => IdempotencyBegin::Conflict,
            IdempotencyEntry { outcome: Some(outcome), .. } => IdempotencyBegin::Recorded(outcome),
            IdempotencyEntry { outcome: None, .. } => IdempotencyBegin::InFlight,
        })
    }

    async fn complete(&self, key: &str, outcome: RecordedOutcome) -> StoreResult<()> {
        let result = self
            .change("idem", key, "idempotency key", |mut entry: IdempotencyEntry| {
                if entry.outcome.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "idempotency key {key} already has an outcome; outcomes are write-once"
                    )));
                }
                entry.outcome = Some(outcome.clone());
                Ok(Some(entry))
            })
            .await;
        result.map(|_| ())
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
        let entries: std::collections::HashMap<String, String> = self
            .client
            .hgetall(self.key("idem"))
            .await
            .map_err(failed("list idempotency records"))?;
        let stale: Vec<String> = entries
            .into_iter()
            .filter_map(|(key, text)| {
                let entry: IdempotencyEntry = decode(NAME, &text).ok()?;
                (entry.began_us < cutoff.timestamp_micros()).then_some(key)
            })
            .collect();
        if stale.is_empty() {
            return Ok(0);
        }
        let removed: i64 = self
            .client
            .hdel(self.key("idem"), stale)
            .await
            .map_err(failed("purge idempotency records"))?;
        Ok(usize::try_from(removed).unwrap_or(0))
    }
}

#[async_trait]
impl EventLog for RedisEdgeStores {
    async fn append(&self, event: RetainedEvent) -> StoreResult<()> {
        let id = event.event.id.clone();
        let at = event.event.timestamp.timestamp_micros().to_string();
        let entry = encode(NAME, &StoredEvent { event: event.event, audience: event.audience })?;
        let _: i64 = self
            .script(APPEND_SCRIPT, &["events", "event"], vec![id, at, entry], "retain an event")
            .await?;
        Ok(())
    }

    async fn between(
        &self,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> StoreResult<Vec<RetainedEvent>> {
        let max = until
            .map_or_else(|| "+inf".to_owned(), |until| format!("({}", until.timestamp_micros()));
        let entries: Vec<String> = self
            .script(
                RANGE_VALUES_SCRIPT,
                &["events", "event"],
                vec![since.timestamp_micros().to_string(), max],
                "read events",
            )
            .await?;
        entries
            .iter()
            .map(|entry| {
                let stored: StoredEvent = decode(NAME, entry)?;
                Ok(RetainedEvent { event: stored.event, audience: stored.audience })
            })
            .collect()
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
        let cutoff = cutoff.timestamp_micros();
        let purged: i64 = self
            .script(
                PURGE_SCRIPT,
                &["events", "event", "pages"],
                vec![format!("({cutoff}"), cutoff.to_string()],
                "purge events",
            )
            .await?;
        Ok(usize::try_from(purged).unwrap_or(0))
    }

    async fn event(&self, id: &str) -> StoreResult<Option<RetainedEvent>> {
        let Some(entry) = self.field("event", id).await? else { return Ok(None) };
        let stored: StoredEvent = decode(NAME, &entry)?;
        Ok(Some(RetainedEvent { event: stored.event, audience: stored.audience }))
    }

    async fn put_catalog_pages(
        &self,
        seq: u64,
        at: DateTime<Utc>,
        pages: &[CatalogChangesPage],
    ) -> StoreResult<bool> {
        let mut args = vec![seq.to_string()];
        for (index, page) in pages.iter().enumerate() {
            args.push(format!("{seq}:{}", index + 2));
            args.push(encode(
                NAME,
                &StoredPage { at_us: at.timestamp_micros(), page: page.clone() },
            )?);
        }
        let _: i64 = self.script(PAGES_SCRIPT, &["pages"], args, "store catalogue pages").await?;
        Ok(true)
    }

    async fn catalog_page(&self, seq: u64, page: usize) -> StoreResult<Option<CatalogChangesPage>> {
        let Some(entry) = self.field("pages", &format!("{seq}:{page}")).await? else {
            return Ok(None);
        };
        Ok(Some(decode::<StoredPage>(NAME, &entry)?.page))
    }

    async fn note_consent_reference(
        &self,
        receipt: Uuid,
        requester: &RequesterId,
    ) -> StoreResult<bool> {
        let _: i64 = self
            .client
            .sadd(self.key(&format!("consent-refs:{receipt}")), requester.as_str())
            .await
            .map_err(failed("note a consent reference"))?;
        Ok(true)
    }

    async fn consent_references(
        &self,
        receipt: Uuid,
    ) -> StoreResult<std::collections::BTreeSet<RequesterId>> {
        let found: Vec<String> = self
            .client
            .smembers(self.key(&format!("consent-refs:{receipt}")))
            .await
            .map_err(failed("read consent references"))?;
        Ok(found.into_iter().map(RequesterId::new).collect())
    }

    async fn save_delivery(&self, delivery: &PendingDelivery) -> StoreResult<()> {
        let member = delivery_member(&delivery.subscription, &delivery.event_id);
        let args = vec![
            member,
            delivery.due.timestamp_micros().to_string(),
            delivery.failures.to_string(),
        ];
        let _: i64 = self
            .script(SAVE_DELIVERY_SCRIPT, &["deliveries", "delivery"], args, "keep a delivery")
            .await?;
        Ok(())
    }

    async fn claim_deliveries(
        &self,
        now: DateTime<Utc>,
        lease: chrono::Duration,
    ) -> StoreResult<Vec<PendingDelivery>> {
        let due = now + lease;
        let pairs: Vec<String> = self
            .script(
                CLAIM_SCRIPT,
                &["deliveries", "delivery"],
                vec![now.timestamp_micros().to_string(), due.timestamp_micros().to_string()],
                "claim deliveries",
            )
            .await?;
        let mut claimed: Vec<PendingDelivery> = pairs
            .chunks(2)
            .filter_map(|pair| {
                let (subscription, event_id) = pair.first()?.split_once('\u{1f}')?;
                Some(PendingDelivery {
                    subscription: subscription.to_owned(),
                    event_id: event_id.to_owned(),
                    failures: pair.get(1).and_then(|failures| failures.parse().ok()).unwrap_or(0),
                    due,
                })
            })
            .collect();
        claimed.sort_by(|a, b| (&a.subscription, &a.event_id).cmp(&(&b.subscription, &b.event_id)));
        Ok(claimed)
    }

    async fn finish_delivery(&self, subscription: &str, event_id: &str) -> StoreResult<()> {
        let _: i64 = self
            .script(
                FINISH_DELIVERY_SCRIPT,
                &["deliveries", "delivery"],
                vec![delivery_member(subscription, event_id)],
                "finish a delivery",
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl BeaconStore for RedisEdgeStores {
    async fn add(&self, day: NaiveDate, metric: &str, count: u64) -> StoreResult<()> {
        let day = day_number(day);
        let _: i64 = self
            .script(
                BEACON_ADD_SCRIPT,
                &[&format!("beacon:{day}"), "beacon-days"],
                vec![metric.to_owned(), count.to_string(), day.to_string()],
                "count a beacon",
            )
            .await?;
        Ok(())
    }

    async fn add_visitor(&self, day: NaiveDate, visitor: &str) -> StoreResult<()> {
        let day = day_number(day);
        let _: i64 = self
            .script(
                VISITOR_ADD_SCRIPT,
                &[&format!("visitors:{day}"), "visitor-days"],
                vec![visitor.to_owned(), day.to_string()],
                "count a visitor",
            )
            .await?;
        Ok(())
    }

    async fn total(&self, metric: &str, from: NaiveDate, to: NaiveDate) -> StoreResult<u64> {
        let mut total = 0_u64;
        if metric == "visitors" {
            for day in self.days("visitor-days", from, to).await? {
                let seen: i64 = self
                    .client
                    .scard(self.key(&format!("visitors:{day}")))
                    .await
                    .map_err(failed("read visitors"))?;
                total += u64::try_from(seen).unwrap_or(0);
            }
            return Ok(total);
        }
        for day in self.days("beacon-days", from, to).await? {
            let count: Option<i64> = self
                .client
                .hget(self.key(&format!("beacon:{day}")), metric)
                .await
                .map_err(failed("read beacon counts"))?;
            total += count.and_then(|count| u64::try_from(count).ok()).unwrap_or(0);
        }
        Ok(total)
    }
}
