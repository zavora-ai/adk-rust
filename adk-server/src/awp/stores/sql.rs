//! The SQLite and PostgreSQL edge stores (spec 3.5 §7.9; features `awp-sqlite`,
//! `awp-postgres`).
//!
//! | Table | Rows |
//! |-------|------|
//! | `adk_awp_subscriptions` | One per subscription: owner, state and a version beside the JSON record |
//! | `adk_awp_consent` | One per receipt: subject, scope, grant and revocation times beside the JSON receipt |
//! | `adk_awp_status` | One per site: the current signed status |
//! | `adk_awp_held_calls` | One per held call: state and hold time beside the JSON record |
//! | `adk_awp_leads` | One per lead: capture time and score beside the JSON lead |
//! | `adk_awp_idempotency` | One per key: input digest, start time and the recorded outcome |
//! | `adk_awp_events` | One per retained event: its time and audience beside the JSON event |
//! | `adk_awp_catalog_pages` | One per further page of a release's `catalog.changed` |
//! | `adk_awp_consent_refs` | One per consent receipt and requester that referenced it |
//! | `adk_awp_deliveries` | One per pending delivery: failures so far and when it is due |
//! | `adk_awp_beacon` | One per day and metric: the beacon's count |
//! | `adk_awp_beacon_visitors` | One per day and consenting visitor |
//! | `adk_awp_rate_windows` | One per caller, rate class and period: the current window's start and hits |
//!
//! Every row carries its site, the namespace that keeps one site's records apart. Both
//! backends record their steps in `_adk_awp_edge_migrations` with adk-session's migration
//! runners. Parameters appear in ascending order and none repeats, since SQLite numbers
//! `$N` by first appearance.

use adk_awp::v2::store::{
    ConsentReceiptStore, ConsentRecord, HeldCallRecord, HeldCallStore, HeldCallTransition,
    IdempotencyBegin, IdempotencyStore, Lead, LeadStore, RecordedOutcome, StatusStore, StoreError,
    StoreResult, SubscriptionRecord, SubscriptionStore, SubscriptionUpdate,
};
use async_trait::async_trait;
use awp_types::{
    CatalogChangesPage, ConsentReceipt, ConsentScope, EventType, HeldCallState, LeadRef,
    RequesterId, SiteStatus,
};
use chrono::{DateTime, Datelike as _, NaiveDate, TimeZone, Utc};
use uuid::Uuid;

use crate::awp::beacon::BeaconStore;
use crate::awp::events::{EventLog, PendingDelivery, RetainedEvent};

use super::{
    CAS_ATTEMPTS, DEFAULT_SITE, apply_delivery, apply_transition, apply_update, backend, decode,
    encode, wire_name,
};

/// The migration registry of the edge stores.
const REGISTRY_TABLE: &str = "_adk_awp_edge_migrations";

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS adk_awp_subscriptions (site TEXT NOT NULL, id TEXT NOT NULL, \
owner TEXT NOT NULL, state TEXT NOT NULL, version BIGINT NOT NULL, record TEXT NOT NULL, \
PRIMARY KEY (site, id));\
CREATE INDEX IF NOT EXISTS adk_awp_subscriptions_owner ON adk_awp_subscriptions (site, owner);\
CREATE TABLE IF NOT EXISTS adk_awp_consent (site TEXT NOT NULL, id TEXT NOT NULL, \
subject TEXT NOT NULL, scope TEXT NOT NULL, granted_us BIGINT NOT NULL, revoked_us BIGINT, \
receipt TEXT NOT NULL, PRIMARY KEY (site, id));\
CREATE INDEX IF NOT EXISTS adk_awp_consent_subject ON adk_awp_consent (site, subject);\
CREATE TABLE IF NOT EXISTS adk_awp_status (site TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS adk_awp_held_calls (site TEXT NOT NULL, id TEXT NOT NULL, \
state TEXT NOT NULL, created_us BIGINT NOT NULL, record TEXT NOT NULL, PRIMARY KEY (site, id));\
CREATE INDEX IF NOT EXISTS adk_awp_held_calls_state ON adk_awp_held_calls (site, state, created_us);\
CREATE TABLE IF NOT EXISTS adk_awp_leads (site TEXT NOT NULL, id TEXT NOT NULL, \
captured_us BIGINT NOT NULL, score DOUBLE PRECISION, lead TEXT NOT NULL, PRIMARY KEY (site, id));\
CREATE INDEX IF NOT EXISTS adk_awp_leads_captured ON adk_awp_leads (site, captured_us);\
CREATE TABLE IF NOT EXISTS adk_awp_idempotency (site TEXT NOT NULL, key TEXT NOT NULL, \
digest TEXT NOT NULL, began_us BIGINT NOT NULL, outcome TEXT, PRIMARY KEY (site, key));\
CREATE INDEX IF NOT EXISTS adk_awp_idempotency_began ON adk_awp_idempotency (site, began_us);";

/// The event log and the beacon (spec 3.5 §7.5, §7.12).
const SCHEMA_EVENTS: &str = "\
CREATE TABLE IF NOT EXISTS adk_awp_events (site TEXT NOT NULL, id TEXT NOT NULL, \
at_us BIGINT NOT NULL, event TEXT NOT NULL, audience TEXT, PRIMARY KEY (site, id));\
CREATE INDEX IF NOT EXISTS adk_awp_events_at ON adk_awp_events (site, at_us);\
CREATE TABLE IF NOT EXISTS adk_awp_catalog_pages (site TEXT NOT NULL, seq BIGINT NOT NULL, \
page BIGINT NOT NULL, at_us BIGINT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (site, seq, page));\
CREATE TABLE IF NOT EXISTS adk_awp_consent_refs (site TEXT NOT NULL, receipt TEXT NOT NULL, \
requester TEXT NOT NULL, PRIMARY KEY (site, receipt, requester));\
CREATE TABLE IF NOT EXISTS adk_awp_deliveries (site TEXT NOT NULL, subscription TEXT NOT NULL, \
event_id TEXT NOT NULL, failures BIGINT NOT NULL, due_us BIGINT NOT NULL, \
PRIMARY KEY (site, subscription, event_id));\
CREATE INDEX IF NOT EXISTS adk_awp_deliveries_due ON adk_awp_deliveries (site, due_us);\
CREATE TABLE IF NOT EXISTS adk_awp_beacon (site TEXT NOT NULL, day BIGINT NOT NULL, \
metric TEXT NOT NULL, hits BIGINT NOT NULL, PRIMARY KEY (site, day, metric));\
CREATE TABLE IF NOT EXISTS adk_awp_beacon_visitors (site TEXT NOT NULL, day BIGINT NOT NULL, \
visitor TEXT NOT NULL, PRIMARY KEY (site, day, visitor));";

/// The edge's shared rate-limit windows (spec 3.5 §7.9).
const SCHEMA_RATE: &str = "\
CREATE TABLE IF NOT EXISTS adk_awp_rate_windows (site TEXT NOT NULL, key TEXT NOT NULL, \
class TEXT NOT NULL, period TEXT NOT NULL, start_s BIGINT NOT NULL, hits BIGINT NOT NULL, \
PRIMARY KEY (site, key, class, period));\
CREATE INDEX IF NOT EXISTS adk_awp_rate_windows_start ON adk_awp_rate_windows (site, start_s);";

/// The steps both backends run; the schema is portable.
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (1, "create the AWP edge store tables", SCHEMA),
    (2, "create the AWP edge event log and beacon tables", SCHEMA_EVENTS),
    (3, "create the AWP edge rate-limit windows", SCHEMA_RATE),
];

const SUB_INSERT: &str = "INSERT INTO adk_awp_subscriptions (site, id, owner, state, version, record) \
VALUES ($1, $2, $3, $4, 1, $5) ON CONFLICT (site, id) DO NOTHING";
const SUB_GET: &str =
    "SELECT record, version FROM adk_awp_subscriptions WHERE site = $1 AND id = $2";
const SUB_LIST: &str =
    "SELECT record FROM adk_awp_subscriptions WHERE site = $1 AND owner = $2 ORDER BY id";
const SUB_ACTIVE: &str =
    "SELECT record FROM adk_awp_subscriptions WHERE site = $1 AND state = 'active' ORDER BY id";
const SUB_CAS: &str = "UPDATE adk_awp_subscriptions SET state = $1, version = version + 1, \
record = $2 WHERE site = $3 AND id = $4 AND version = $5";
const SUB_DELETE: &str =
    "DELETE FROM adk_awp_subscriptions WHERE site = $1 AND id = $2 AND owner = $3";

const CONSENT_INSERT: &str = "INSERT INTO adk_awp_consent (site, id, subject, scope, granted_us, \
revoked_us, receipt) VALUES ($1, $2, $3, $4, $5, NULL, $6) ON CONFLICT (site, id) DO NOTHING";
const CONSENT_GET: &str =
    "SELECT receipt, revoked_us FROM adk_awp_consent WHERE site = $1 AND id = $2";
const CONSENT_SUBJECT: &str = "SELECT receipt, revoked_us FROM adk_awp_consent \
WHERE site = $1 AND subject = $2 ORDER BY granted_us, id";
const CONSENT_REVOKE: &str = "UPDATE adk_awp_consent SET revoked_us = COALESCE(revoked_us, $1) \
WHERE site = $2 AND id = $3";
const CONSENT_REVOKE_SCOPE: &str = "UPDATE adk_awp_consent SET revoked_us = $1 \
WHERE site = $2 AND subject = $3 AND scope = $4 AND revoked_us IS NULL RETURNING receipt, revoked_us";
const CONSENT_ERASE: &str = "DELETE FROM adk_awp_consent WHERE site = $1 AND subject = $2";

const STATUS_GET: &str = "SELECT status FROM adk_awp_status WHERE site = $1";
const STATUS_SET: &str = "INSERT INTO adk_awp_status (site, status) VALUES ($1, $2) \
ON CONFLICT (site) DO UPDATE SET status = excluded.status";

const HELD_INSERT: &str = "INSERT INTO adk_awp_held_calls (site, id, state, created_us, record) \
VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site, id) DO NOTHING";
const HELD_GET: &str = "SELECT record FROM adk_awp_held_calls WHERE site = $1 AND id = $2";
const HELD_PENDING: &str = "SELECT record FROM adk_awp_held_calls WHERE site = $1 \
AND state = 'pending' ORDER BY created_us, id";
const HELD_CAS: &str = "UPDATE adk_awp_held_calls SET state = $1, record = $2 \
WHERE site = $3 AND id = $4 AND state = $5";

const LEAD_INSERT: &str = "INSERT INTO adk_awp_leads (site, id, captured_us, score, lead) \
VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site, id) DO NOTHING";
const LEAD_GET: &str = "SELECT lead, score FROM adk_awp_leads WHERE site = $1 AND id = $2";
const LEAD_SINCE: &str = "SELECT lead, score FROM adk_awp_leads WHERE site = $1 \
AND captured_us >= $2 ORDER BY captured_us, id";
const LEAD_SCORE: &str = "UPDATE adk_awp_leads SET score = $1 WHERE site = $2 AND id = $3";
const LEAD_DELETE: &str = "DELETE FROM adk_awp_leads WHERE site = $1 AND id = $2";

const IDEM_BEGIN: &str = "INSERT INTO adk_awp_idempotency (site, key, digest, began_us, outcome) \
VALUES ($1, $2, $3, $4, NULL) ON CONFLICT (site, key) DO NOTHING";
const IDEM_GET: &str =
    "SELECT digest, outcome FROM adk_awp_idempotency WHERE site = $1 AND key = $2";
const IDEM_COMPLETE: &str = "UPDATE adk_awp_idempotency SET outcome = $1 \
WHERE site = $2 AND key = $3 AND outcome IS NULL";
const IDEM_PURGE: &str = "DELETE FROM adk_awp_idempotency WHERE site = $1 AND began_us < $2";

const EVENT_INSERT: &str = "INSERT INTO adk_awp_events (site, id, at_us, event, audience) \
VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site, id) DO NOTHING";
const EVENT_BETWEEN: &str = "SELECT event, audience FROM adk_awp_events WHERE site = $1 \
AND at_us >= $2 AND at_us < $3 ORDER BY at_us, id";
const EVENT_GET: &str = "SELECT event, audience FROM adk_awp_events WHERE site = $1 AND id = $2";
const EVENT_PURGE: &str = "DELETE FROM adk_awp_events WHERE site = $1 AND at_us < $2";
const PAGE_PUT: &str = "INSERT INTO adk_awp_catalog_pages (site, seq, page, at_us, body) \
VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site, seq, page) \
DO UPDATE SET at_us = excluded.at_us, body = excluded.body";
const PAGE_DROP: &str = "DELETE FROM adk_awp_catalog_pages WHERE site = $1 AND seq = $2";
const PAGE_GET: &str =
    "SELECT body FROM adk_awp_catalog_pages WHERE site = $1 AND seq = $2 AND page = $3";
const PAGE_PURGE: &str = "DELETE FROM adk_awp_catalog_pages WHERE site = $1 AND at_us < $2";
const REF_INSERT: &str = "INSERT INTO adk_awp_consent_refs (site, receipt, requester) \
VALUES ($1, $2, $3) ON CONFLICT (site, receipt, requester) DO NOTHING";
const REF_LIST: &str = "SELECT requester FROM adk_awp_consent_refs WHERE site = $1 \
AND receipt = $2 ORDER BY requester";
const DELIVERY_SAVE: &str = "INSERT INTO adk_awp_deliveries (site, subscription, event_id, \
failures, due_us) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site, subscription, event_id) \
DO UPDATE SET failures = excluded.failures, due_us = excluded.due_us";
const DELIVERY_CLAIM: &str = "UPDATE adk_awp_deliveries SET due_us = $1 WHERE site = $2 \
AND due_us <= $3 RETURNING subscription, event_id, failures";
const DELIVERY_FINISH: &str = "DELETE FROM adk_awp_deliveries WHERE site = $1 \
AND subscription = $2 AND event_id = $3";
const BEACON_ADD: &str = "INSERT INTO adk_awp_beacon (site, day, metric, hits) \
VALUES ($1, $2, $3, $4) ON CONFLICT (site, day, metric) \
DO UPDATE SET hits = adk_awp_beacon.hits + excluded.hits";
const BEACON_TOTAL: &str = "SELECT CAST(COALESCE(SUM(hits), 0) AS BIGINT) FROM adk_awp_beacon \
WHERE site = $1 AND metric = $2 AND day >= $3 AND day < $4";
const VISITOR_ADD: &str = "INSERT INTO adk_awp_beacon_visitors (site, day, visitor) \
VALUES ($1, $2, $3) ON CONFLICT (site, day, visitor) DO NOTHING";
const VISITOR_TOTAL: &str = "SELECT COUNT(*) FROM adk_awp_beacon_visitors WHERE site = $1 \
AND day >= $2 AND day < $3";
/// One hit: a new window starts at one, the current one counts up while under the limit; no
/// row comes back when the hit is refused.
const RATE_HIT: &str = "INSERT INTO adk_awp_rate_windows (site, key, class, period, start_s, hits) \
VALUES ($1, $2, $3, $4, $5, 1) ON CONFLICT (site, key, class, period) DO UPDATE SET \
hits = CASE WHEN adk_awp_rate_windows.start_s = excluded.start_s \
THEN adk_awp_rate_windows.hits + 1 ELSE 1 END, start_s = excluded.start_s \
WHERE adk_awp_rate_windows.start_s <> excluded.start_s OR adk_awp_rate_windows.hits < $6 \
RETURNING hits";
const RATE_PURGE: &str = "DELETE FROM adk_awp_rate_windows WHERE site = $1 AND start_s < $2";

/// A day as stored: days from the common era.
fn day_number(day: NaiveDate) -> i64 {
    i64::from(day.num_days_from_ce())
}

/// A retained event read back from its JSON event and audience.
fn retained_event(name: &str, event: &str, audience: Option<&str>) -> StoreResult<RetainedEvent> {
    Ok(RetainedEvent {
        event: decode(name, event)?,
        audience: audience.map(|audience| decode(name, audience)).transpose()?,
    })
}

fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

fn from_micros(us: i64) -> DateTime<Utc> {
    Utc.timestamp_micros(us).single().unwrap_or(DateTime::<Utc>::MIN_UTC)
}

fn consent_record(
    name: &str,
    receipt: &str,
    revoked_us: Option<i64>,
) -> StoreResult<ConsentRecord> {
    Ok(ConsentRecord { receipt: decode(name, receipt)?, revoked_at: revoked_us.map(from_micros) })
}

fn lead_with_score(name: &str, lead: &str, score: Option<f64>) -> StoreResult<Lead> {
    let mut lead: Lead = decode(name, lead)?;
    if score.is_some() {
        lead.score = score;
    }
    Ok(lead)
}

/// Implements the shared rate-limit windows for a store with `pool` and `site` fields.
macro_rules! sql_rate_limits {
    ($store:ty, $name:literal) => {
        #[async_trait]
        impl crate::awp::limits::RateLimitStore for $store {
            async fn hit(
                &self,
                window: &crate::awp::limits::RateWindow,
                limit: u32,
            ) -> StoreResult<Option<u32>> {
                if limit == 0 {
                    return Ok(None);
                }
                let row: Option<(i64,)> = sqlx::query_as(RATE_HIT)
                    .bind(&self.site)
                    .bind(&window.key)
                    .bind(&window.class)
                    .bind(&window.period)
                    .bind(window.start)
                    .bind(i64::from(limit))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "count a rate-limit hit", &e))?;
                Ok(row.map(|(hits,)| u32::try_from(hits).unwrap_or(u32::MAX)))
            }

            async fn purge_before(&self, before: i64) -> StoreResult<usize> {
                let done = sqlx::query(RATE_PURGE)
                    .bind(&self.site)
                    .bind(before)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "purge rate-limit windows", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }
        }
    };
}

/// Implements the six edge store traits for a store with `pool` and `site` fields.
macro_rules! sql_edge_stores {
    ($store:ty, $name:literal) => {
        impl $store {
            async fn subscription(
                &self,
                id: &str,
            ) -> StoreResult<Option<(SubscriptionRecord, i64)>> {
                let row: Option<(String, i64)> = sqlx::query_as(SUB_GET)
                    .bind(&self.site)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read a subscription", &e))?;
                row.map(|(record, version)| Ok((decode($name, &record)?, version))).transpose()
            }

            /// Changes subscription `id`, owned by `owner` when given, with a
            /// compare-and-set on its version.
            async fn change_subscription(
                &self,
                id: &str,
                owner: Option<&RequesterId>,
                change: impl Fn(&mut SubscriptionRecord),
            ) -> StoreResult<SubscriptionRecord> {
                for _ in 0..CAS_ATTEMPTS {
                    let Some((mut record, version)) = self
                        .subscription(id)
                        .await?
                        .filter(|(record, _)| owner.is_none_or(|owner| &record.owner == owner))
                    else {
                        return Err(StoreError::NotFound(format!("subscription {id}")));
                    };
                    change(&mut record);
                    let done = sqlx::query(SUB_CAS)
                        .bind(wire_name(&record.subscription.state))
                        .bind(encode($name, &record)?)
                        .bind(&self.site)
                        .bind(id)
                        .bind(version)
                        .execute(&self.pool)
                        .await
                        .map_err(|e| backend($name, "change a subscription", &e))?;
                    if done.rows_affected() == 1 {
                        return Ok(record);
                    }
                }
                Err(StoreError::Conflict(format!("subscription {id} kept changing; retry")))
            }

            async fn held(&self, id: &str) -> StoreResult<Option<HeldCallRecord>> {
                let row: Option<(String,)> = sqlx::query_as(HELD_GET)
                    .bind(&self.site)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read a held call", &e))?;
                row.map(|(record,)| decode($name, &record)).transpose()
            }
        }

        #[async_trait]
        impl SubscriptionStore for $store {
            async fn create(&self, record: SubscriptionRecord) -> StoreResult<()> {
                let done = sqlx::query(SUB_INSERT)
                    .bind(&self.site)
                    .bind(&record.subscription.id)
                    .bind(record.owner.as_str())
                    .bind(wire_name(&record.subscription.state))
                    .bind(encode($name, &record)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "store a subscription", &e))?;
                if done.rows_affected() == 0 {
                    return Err(StoreError::Conflict(format!(
                        "subscription {} already exists",
                        record.subscription.id
                    )));
                }
                Ok(())
            }

            async fn get(
                &self,
                owner: &RequesterId,
                id: &str,
            ) -> StoreResult<Option<SubscriptionRecord>> {
                Ok(self
                    .subscription(id)
                    .await?
                    .map(|(record, _)| record)
                    .filter(|record| &record.owner == owner))
            }

            async fn list(&self, owner: &RequesterId) -> StoreResult<Vec<SubscriptionRecord>> {
                let rows: Vec<(String,)> = sqlx::query_as(SUB_LIST)
                    .bind(&self.site)
                    .bind(owner.as_str())
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "list subscriptions", &e))?;
                rows.iter().map(|(record,)| decode($name, record)).collect()
            }

            async fn active_for(
                &self,
                event_type: &EventType,
            ) -> StoreResult<Vec<SubscriptionRecord>> {
                let rows: Vec<(String,)> = sqlx::query_as(SUB_ACTIVE)
                    .bind(&self.site)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "list active subscriptions", &e))?;
                let records: Vec<SubscriptionRecord> = rows
                    .iter()
                    .map(|(record,)| decode($name, record))
                    .collect::<StoreResult<_>>()?;
                Ok(records
                    .into_iter()
                    .filter(|record| record.subscription.types.contains(event_type))
                    .collect())
            }

            async fn update(
                &self,
                owner: &RequesterId,
                id: &str,
                update: SubscriptionUpdate,
            ) -> StoreResult<SubscriptionRecord> {
                self.change_subscription(id, Some(owner), |record| {
                    apply_update(record, update.clone())
                })
                .await
            }

            async fn record_delivery(
                &self,
                id: &str,
                delivered: bool,
            ) -> StoreResult<SubscriptionRecord> {
                self.change_subscription(id, None, |record| apply_delivery(record, delivered)).await
            }

            async fn delete(&self, owner: &RequesterId, id: &str) -> StoreResult<bool> {
                let done = sqlx::query(SUB_DELETE)
                    .bind(&self.site)
                    .bind(id)
                    .bind(owner.as_str())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "delete a subscription", &e))?;
                Ok(done.rows_affected() > 0)
            }
        }

        #[async_trait]
        impl ConsentReceiptStore for $store {
            async fn insert(&self, receipt: ConsentReceipt) -> StoreResult<()> {
                let done = sqlx::query(CONSENT_INSERT)
                    .bind(&self.site)
                    .bind(receipt.id.to_string())
                    .bind(&receipt.subject_ref)
                    .bind(wire_name(&receipt.scope))
                    .bind(micros(receipt.granted_at))
                    .bind(encode($name, &receipt)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "store a consent receipt", &e))?;
                if done.rows_affected() == 0 {
                    return Err(StoreError::Conflict(format!(
                        "consent receipt {} already exists",
                        receipt.id
                    )));
                }
                Ok(())
            }

            async fn get(&self, id: Uuid) -> StoreResult<Option<ConsentRecord>> {
                let row: Option<(String, Option<i64>)> = sqlx::query_as(CONSENT_GET)
                    .bind(&self.site)
                    .bind(id.to_string())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read a consent receipt", &e))?;
                row.map(|(receipt, revoked)| consent_record($name, &receipt, revoked)).transpose()
            }

            async fn for_subject(&self, subject_ref: &str) -> StoreResult<Vec<ConsentRecord>> {
                let rows: Vec<(String, Option<i64>)> = sqlx::query_as(CONSENT_SUBJECT)
                    .bind(&self.site)
                    .bind(subject_ref)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "list consent receipts", &e))?;
                rows.iter()
                    .map(|(receipt, revoked)| consent_record($name, receipt, *revoked))
                    .collect()
            }

            async fn revoke(
                &self,
                id: Uuid,
                at: DateTime<Utc>,
            ) -> StoreResult<Option<ConsentRecord>> {
                sqlx::query(CONSENT_REVOKE)
                    .bind(micros(at))
                    .bind(&self.site)
                    .bind(id.to_string())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "revoke a consent receipt", &e))?;
                ConsentReceiptStore::get(self, id).await
            }

            async fn revoke_scope(
                &self,
                subject_ref: &str,
                scope: &ConsentScope,
                at: DateTime<Utc>,
            ) -> StoreResult<Vec<ConsentRecord>> {
                let rows: Vec<(String, Option<i64>)> = sqlx::query_as(CONSENT_REVOKE_SCOPE)
                    .bind(micros(at))
                    .bind(&self.site)
                    .bind(subject_ref)
                    .bind(wire_name(scope))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "revoke consent receipts", &e))?;
                let mut revoked = rows
                    .iter()
                    .map(|(receipt, revoked)| consent_record($name, receipt, *revoked))
                    .collect::<StoreResult<Vec<_>>>()?;
                revoked.sort_by_key(|record| record.receipt.id);
                Ok(revoked)
            }

            async fn erase(&self, subject_ref: &str) -> StoreResult<usize> {
                let done = sqlx::query(CONSENT_ERASE)
                    .bind(&self.site)
                    .bind(subject_ref)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "erase consent receipts", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }
        }

        #[async_trait]
        impl StatusStore for $store {
            async fn current(&self) -> StoreResult<Option<SiteStatus>> {
                let row: Option<(String,)> = sqlx::query_as(STATUS_GET)
                    .bind(&self.site)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read the status", &e))?;
                row.map(|(status,)| decode($name, &status)).transpose()
            }

            async fn set(&self, status: SiteStatus) -> StoreResult<Option<SiteStatus>> {
                let previous = self.current().await?;
                sqlx::query(STATUS_SET)
                    .bind(&self.site)
                    .bind(encode($name, &status)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "store the status", &e))?;
                Ok(previous)
            }
        }

        #[async_trait]
        impl HeldCallStore for $store {
            async fn create(&self, record: HeldCallRecord) -> StoreResult<()> {
                if record.call.state != HeldCallState::Pending {
                    return Err(StoreError::Conflict("a held call starts pending".to_owned()));
                }
                let done = sqlx::query(HELD_INSERT)
                    .bind(&self.site)
                    .bind(&record.call.id)
                    .bind(wire_name(&record.call.state))
                    .bind(micros(record.call.created_at))
                    .bind(encode($name, &record)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "store a held call", &e))?;
                if done.rows_affected() == 0 {
                    return Err(StoreError::Conflict(format!(
                        "held call {} already exists",
                        record.call.id
                    )));
                }
                Ok(())
            }

            async fn get(&self, id: &str) -> StoreResult<Option<HeldCallRecord>> {
                self.held(id).await
            }

            async fn pending(&self) -> StoreResult<Vec<HeldCallRecord>> {
                let rows: Vec<(String,)> = sqlx::query_as(HELD_PENDING)
                    .bind(&self.site)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "list held calls", &e))?;
                rows.iter().map(|(record,)| decode($name, record)).collect()
            }

            async fn transition(
                &self,
                id: &str,
                transition: HeldCallTransition,
            ) -> StoreResult<HeldCallRecord> {
                for _ in 0..CAS_ATTEMPTS {
                    let Some(record) = self.held(id).await? else {
                        return Err(StoreError::NotFound(format!("held call {id}")));
                    };
                    let from = wire_name(&record.call.state);
                    let next = apply_transition(record, transition.clone())?;
                    let done = sqlx::query(HELD_CAS)
                        .bind(wire_name(&next.call.state))
                        .bind(encode($name, &next)?)
                        .bind(&self.site)
                        .bind(id)
                        .bind(from)
                        .execute(&self.pool)
                        .await
                        .map_err(|e| backend($name, "move a held call", &e))?;
                    if done.rows_affected() == 1 {
                        return Ok(next);
                    }
                }
                Err(StoreError::Conflict(format!("held call {id} kept changing; retry")))
            }
        }

        #[async_trait]
        impl LeadStore for $store {
            async fn insert(&self, lead: Lead) -> StoreResult<()> {
                let done = sqlx::query(LEAD_INSERT)
                    .bind(&self.site)
                    .bind(&lead.id.0)
                    .bind(micros(lead.captured_at))
                    .bind(lead.score)
                    .bind(encode($name, &lead)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "store a lead", &e))?;
                if done.rows_affected() == 0 {
                    return Err(StoreError::Conflict(format!("lead {} already exists", lead.id.0)));
                }
                Ok(())
            }

            async fn get(&self, id: &LeadRef) -> StoreResult<Option<Lead>> {
                let row: Option<(String, Option<f64>)> = sqlx::query_as(LEAD_GET)
                    .bind(&self.site)
                    .bind(&id.0)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read a lead", &e))?;
                row.map(|(lead, score)| lead_with_score($name, &lead, score)).transpose()
            }

            async fn since(&self, since: DateTime<Utc>) -> StoreResult<Vec<Lead>> {
                let rows: Vec<(String, Option<f64>)> = sqlx::query_as(LEAD_SINCE)
                    .bind(&self.site)
                    .bind(micros(since))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "list leads", &e))?;
                rows.iter().map(|(lead, score)| lead_with_score($name, lead, *score)).collect()
            }

            async fn set_score(&self, id: &LeadRef, score: f64) -> StoreResult<()> {
                let done = sqlx::query(LEAD_SCORE)
                    .bind(score)
                    .bind(&self.site)
                    .bind(&id.0)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "score a lead", &e))?;
                if done.rows_affected() == 0 {
                    return Err(StoreError::NotFound(format!("lead {}", id.0)));
                }
                Ok(())
            }

            async fn delete(&self, id: &LeadRef) -> StoreResult<bool> {
                let done = sqlx::query(LEAD_DELETE)
                    .bind(&self.site)
                    .bind(&id.0)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "delete a lead", &e))?;
                Ok(done.rows_affected() > 0)
            }
        }

        #[async_trait]
        impl EventLog for $store {
            async fn append(&self, event: RetainedEvent) -> StoreResult<()> {
                let audience =
                    event.audience.as_ref().map(|audience| encode($name, audience)).transpose()?;
                sqlx::query(EVENT_INSERT)
                    .bind(&self.site)
                    .bind(&event.event.id)
                    .bind(micros(event.event.timestamp))
                    .bind(encode($name, &event.event)?)
                    .bind(audience)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "retain an event", &e))?;
                Ok(())
            }

            async fn between(
                &self,
                since: DateTime<Utc>,
                until: Option<DateTime<Utc>>,
            ) -> StoreResult<Vec<RetainedEvent>> {
                let rows: Vec<(String, Option<String>)> = sqlx::query_as(EVENT_BETWEEN)
                    .bind(&self.site)
                    .bind(micros(since))
                    .bind(until.map_or(i64::MAX, micros))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read events", &e))?;
                rows.iter()
                    .map(|(event, audience)| retained_event($name, event, audience.as_deref()))
                    .collect()
            }

            async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
                sqlx::query(PAGE_PURGE)
                    .bind(&self.site)
                    .bind(micros(cutoff))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "purge catalogue pages", &e))?;
                let done = sqlx::query(EVENT_PURGE)
                    .bind(&self.site)
                    .bind(micros(cutoff))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "purge events", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }

            async fn event(&self, id: &str) -> StoreResult<Option<RetainedEvent>> {
                let row: Option<(String, Option<String>)> = sqlx::query_as(EVENT_GET)
                    .bind(&self.site)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read an event", &e))?;
                row.map(|(event, audience)| retained_event($name, &event, audience.as_deref()))
                    .transpose()
            }

            async fn put_catalog_pages(
                &self,
                seq: u64,
                at: DateTime<Utc>,
                pages: &[CatalogChangesPage],
            ) -> StoreResult<bool> {
                let seq = i64::try_from(seq).unwrap_or(i64::MAX);
                let mut tx =
                    self.pool.begin().await.map_err(|e| backend($name, "store pages", &e))?;
                sqlx::query(PAGE_DROP)
                    .bind(&self.site)
                    .bind(seq)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| backend($name, "replace catalogue pages", &e))?;
                for (index, page) in pages.iter().enumerate() {
                    sqlx::query(PAGE_PUT)
                        .bind(&self.site)
                        .bind(seq)
                        .bind(i64::try_from(index + 2).unwrap_or(i64::MAX))
                        .bind(micros(at))
                        .bind(encode($name, page)?)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| backend($name, "store a catalogue page", &e))?;
                }
                tx.commit().await.map_err(|e| backend($name, "store pages", &e))?;
                Ok(true)
            }

            async fn catalog_page(
                &self,
                seq: u64,
                page: usize,
            ) -> StoreResult<Option<CatalogChangesPage>> {
                let row: Option<(String,)> = sqlx::query_as(PAGE_GET)
                    .bind(&self.site)
                    .bind(i64::try_from(seq).unwrap_or(i64::MAX))
                    .bind(i64::try_from(page).unwrap_or(i64::MAX))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read a catalogue page", &e))?;
                row.map(|(body,)| decode($name, &body)).transpose()
            }

            async fn note_consent_reference(
                &self,
                receipt: Uuid,
                requester: &RequesterId,
            ) -> StoreResult<bool> {
                sqlx::query(REF_INSERT)
                    .bind(&self.site)
                    .bind(receipt.to_string())
                    .bind(requester.as_str())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "note a consent reference", &e))?;
                Ok(true)
            }

            async fn consent_references(
                &self,
                receipt: Uuid,
            ) -> StoreResult<std::collections::BTreeSet<RequesterId>> {
                let rows: Vec<(String,)> = sqlx::query_as(REF_LIST)
                    .bind(&self.site)
                    .bind(receipt.to_string())
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read consent references", &e))?;
                Ok(rows.into_iter().map(|(requester,)| RequesterId::new(requester)).collect())
            }

            async fn save_delivery(&self, delivery: &PendingDelivery) -> StoreResult<()> {
                sqlx::query(DELIVERY_SAVE)
                    .bind(&self.site)
                    .bind(&delivery.subscription)
                    .bind(&delivery.event_id)
                    .bind(i64::from(delivery.failures))
                    .bind(micros(delivery.due))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "keep a delivery", &e))?;
                Ok(())
            }

            async fn claim_deliveries(
                &self,
                now: DateTime<Utc>,
                lease: chrono::Duration,
            ) -> StoreResult<Vec<PendingDelivery>> {
                let due = now + lease;
                let mut rows: Vec<(String, String, i64)> = sqlx::query_as(DELIVERY_CLAIM)
                    .bind(micros(due))
                    .bind(&self.site)
                    .bind(micros(now))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| backend($name, "claim deliveries", &e))?;
                rows.sort();
                Ok(rows
                    .into_iter()
                    .map(|(subscription, event_id, failures)| PendingDelivery {
                        subscription,
                        event_id,
                        failures: u32::try_from(failures).unwrap_or(u32::MAX),
                        due,
                    })
                    .collect())
            }

            async fn finish_delivery(&self, subscription: &str, event_id: &str) -> StoreResult<()> {
                sqlx::query(DELIVERY_FINISH)
                    .bind(&self.site)
                    .bind(subscription)
                    .bind(event_id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "finish a delivery", &e))?;
                Ok(())
            }
        }

        #[async_trait]
        impl BeaconStore for $store {
            async fn add(&self, day: NaiveDate, metric: &str, count: u64) -> StoreResult<()> {
                sqlx::query(BEACON_ADD)
                    .bind(&self.site)
                    .bind(day_number(day))
                    .bind(metric)
                    .bind(i64::try_from(count).unwrap_or(i64::MAX))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "count a beacon", &e))?;
                Ok(())
            }

            async fn forget_visitor(&self, visitor: &str) -> StoreResult<u64> {
                let days: Vec<(i64,)> = sqlx::query_as(
                    "SELECT day FROM adk_awp_beacon_visitors WHERE site = $1 AND visitor = $2",
                )
                .bind(&self.site)
                .bind(visitor)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| backend($name, "read a visitor's days", &e))?;
                for (day,) in &days {
                    sqlx::query(
                        "UPDATE adk_awp_beacon_visitors SET visitor = $1 \
                         WHERE site = $2 AND day = $3 AND visitor = $4",
                    )
                    .bind(crate::awp::beacon::forgotten())
                    .bind(&self.site)
                    .bind(day)
                    .bind(visitor)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "forget a visitor", &e))?;
                }
                Ok(days.len() as u64)
            }

            async fn add_visitor(&self, day: NaiveDate, visitor: &str) -> StoreResult<()> {
                sqlx::query(VISITOR_ADD)
                    .bind(&self.site)
                    .bind(day_number(day))
                    .bind(visitor)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "count a visitor", &e))?;
                Ok(())
            }

            async fn total(
                &self,
                metric: &str,
                from: NaiveDate,
                to: NaiveDate,
            ) -> StoreResult<u64> {
                let (total,): (i64,) = if metric == "visitors" {
                    sqlx::query_as(VISITOR_TOTAL)
                        .bind(&self.site)
                        .bind(day_number(from))
                        .bind(day_number(to))
                        .fetch_one(&self.pool)
                        .await
                } else {
                    sqlx::query_as(BEACON_TOTAL)
                        .bind(&self.site)
                        .bind(metric)
                        .bind(day_number(from))
                        .bind(day_number(to))
                        .fetch_one(&self.pool)
                        .await
                }
                .map_err(|e| backend($name, "read beacon counts", &e))?;
                Ok(u64::try_from(total).unwrap_or(0))
            }
        }

        #[async_trait]
        impl IdempotencyStore for $store {
            async fn begin(
                &self,
                key: &str,
                input_digest: &str,
                at: DateTime<Utc>,
            ) -> StoreResult<IdempotencyBegin> {
                let done = sqlx::query(IDEM_BEGIN)
                    .bind(&self.site)
                    .bind(key)
                    .bind(input_digest)
                    .bind(micros(at))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "begin an idempotent call", &e))?;
                if done.rows_affected() == 1 {
                    return Ok(IdempotencyBegin::Started);
                }
                let row: Option<(String, Option<String>)> = sqlx::query_as(IDEM_GET)
                    .bind(&self.site)
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read an idempotency record", &e))?;
                match row {
                    // Purged between the insert and the read: begin again.
                    None => self.begin(key, input_digest, at).await,
                    Some((digest, _)) if digest != input_digest => Ok(IdempotencyBegin::Conflict),
                    Some((_, Some(outcome))) => {
                        Ok(IdempotencyBegin::Recorded(decode::<RecordedOutcome>($name, &outcome)?))
                    }
                    Some((_, None)) => Ok(IdempotencyBegin::InFlight),
                }
            }

            async fn complete(&self, key: &str, outcome: RecordedOutcome) -> StoreResult<()> {
                let done = sqlx::query(IDEM_COMPLETE)
                    .bind(encode($name, &outcome)?)
                    .bind(&self.site)
                    .bind(key)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "record an outcome", &e))?;
                if done.rows_affected() == 1 {
                    return Ok(());
                }
                let exists: Option<(String, Option<String>)> = sqlx::query_as(IDEM_GET)
                    .bind(&self.site)
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| backend($name, "read an idempotency record", &e))?;
                match exists {
                    None => Err(StoreError::NotFound(format!("idempotency key {key}"))),
                    Some(_) => Err(StoreError::Conflict(format!(
                        "idempotency key {key} already has an outcome; outcomes are write-once"
                    ))),
                }
            }

            async fn purge_before(&self, cutoff: DateTime<Utc>) -> StoreResult<usize> {
                let done = sqlx::query(IDEM_PURGE)
                    .bind(&self.site)
                    .bind(micros(cutoff))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| backend($name, "purge idempotency records", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }
        }
    };
}

/// The six edge stores, the event log, the beacon store and the shared rate-limit windows in
/// SQLite (spec 3.5 §7.9, feature `awp-sqlite`).
///
/// One site's records sit under its `site` namespace, so a database such as `harness.db`
/// holds several sites and other stores. Call [`migrate`](Self::migrate) once before use.
///
/// # Example
///
/// ```rust
/// use adk_awp::v2::store::StatusStore;
/// use adk_server::awp::stores::SqliteEdgeStores;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
/// let stores = SqliteEdgeStores::new(pool).with_site("example.com");
/// stores.migrate().await?;
/// assert!(stores.current().await?.is_none());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "awp-sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteEdgeStores {
    pool: sqlx::SqlitePool,
    site: String,
}

#[cfg(feature = "awp-sqlite")]
impl SqliteEdgeStores {
    /// The stores over `pool`, in the default site namespace.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool, site: DEFAULT_SITE.to_owned() }
    }

    /// Keeps the records under `site`.
    #[must_use]
    pub fn with_site(mut self, site: impl Into<String>) -> Self {
        self.site = site.into();
        self
    }

    /// Creates or upgrades the tables; idempotent.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] when a statement fails or a newer release migrated the
    /// database.
    pub async fn migrate(&self) -> StoreResult<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            REGISTRY_TABLE,
            MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| backend("sqlite", "migrate", &error.message))
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
}

#[cfg(feature = "awp-sqlite")]
sql_edge_stores!(SqliteEdgeStores, "sqlite");
#[cfg(feature = "awp-sqlite")]
sql_rate_limits!(SqliteEdgeStores, "sqlite");

/// The six edge stores, the event log, the beacon store and the rate-limit windows in
/// PostgreSQL, shared by every edge replica (spec 3.5 §7.9, feature `awp-postgres`).
///
/// [`migrate`](Self::migrate) runs under an advisory lock, so replicas starting together
/// migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::awp::stores::PostgresEdgeStores;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stores = PostgresEdgeStores::connect("postgres://localhost/site").await?.with_site("example.com");
/// stores.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "awp-postgres")]
#[derive(Debug, Clone)]
pub struct PostgresEdgeStores {
    pool: sqlx::PgPool,
    site: String,
}

#[cfg(feature = "awp-postgres")]
impl PostgresEdgeStores {
    /// The stores over `pool`, in the default site namespace.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool, site: DEFAULT_SITE.to_owned() }
    }

    /// Connects to `url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] when the server cannot be reached.
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = sqlx::PgPool::connect(url)
            .await
            .map_err(|error| backend("postgres", "connect", &error))?;
        Ok(Self::new(pool))
    }

    /// Keeps the records under `site`.
    #[must_use]
    pub fn with_site(mut self, site: impl Into<String>) -> Self {
        self.site = site.into();
        self
    }

    /// Creates or upgrades the tables under an advisory lock; idempotent.
    ///
    /// # Errors
    ///
    /// [`StoreError::Backend`] when a statement fails or a newer release migrated the
    /// database.
    pub async fn migrate(&self) -> StoreResult<()> {
        migrate_locked(self.pool.clone()).await
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
}

#[cfg(feature = "awp-postgres")]
sql_edge_stores!(PostgresEdgeStores, "postgres");
#[cfg(feature = "awp-postgres")]
sql_rate_limits!(PostgresEdgeStores, "postgres");

/// Runs the migrations under a session-level advisory lock keyed by the registry table.
#[cfg(feature = "awp-postgres")]
fn migrate_locked(
    pool: sqlx::PgPool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = StoreResult<()>> + Send>> {
    use sqlx::Connection as _;

    Box::pin(async move {
        let key = REGISTRY_TABLE
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
            .cast_signed();
        let mut lock = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .map_err(|error| backend("postgres", "connect for the migration lock", &error))?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut lock)
            .await
            .map_err(|error| backend("postgres", "take the migration lock", &error))?;
        let result = adk_session::migration::pg_runner::run_sql_migrations(
            &pool,
            REGISTRY_TABLE,
            MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| backend("postgres", "migrate", &error.message));
        let _ = lock.close().await;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameters(statement: &str) -> Vec<usize> {
        let mut seen = Vec::new();
        let bytes = statement.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'$' {
                let digits: String =
                    statement[index + 1..].chars().take_while(char::is_ascii_digit).collect();
                seen.push(digits.parse::<usize>().unwrap());
                index += digits.len();
            }
            index += 1;
        }
        seen
    }

    #[test]
    fn statements_number_parameters_by_first_appearance() {
        for statement in [
            SUB_INSERT,
            SUB_GET,
            SUB_LIST,
            SUB_ACTIVE,
            SUB_CAS,
            SUB_DELETE,
            CONSENT_INSERT,
            CONSENT_GET,
            CONSENT_SUBJECT,
            CONSENT_REVOKE,
            CONSENT_REVOKE_SCOPE,
            CONSENT_ERASE,
            STATUS_GET,
            STATUS_SET,
            HELD_INSERT,
            HELD_GET,
            HELD_PENDING,
            HELD_CAS,
            LEAD_INSERT,
            LEAD_GET,
            LEAD_SINCE,
            LEAD_SCORE,
            LEAD_DELETE,
            IDEM_BEGIN,
            IDEM_GET,
            IDEM_COMPLETE,
            IDEM_PURGE,
            EVENT_INSERT,
            EVENT_BETWEEN,
            EVENT_GET,
            EVENT_PURGE,
            PAGE_PUT,
            PAGE_DROP,
            PAGE_GET,
            PAGE_PURGE,
            REF_INSERT,
            REF_LIST,
            DELIVERY_SAVE,
            DELIVERY_CLAIM,
            DELIVERY_FINISH,
            BEACON_ADD,
            BEACON_TOTAL,
            VISITOR_ADD,
            VISITOR_TOTAL,
            RATE_HIT,
            RATE_PURGE,
        ] {
            let seen = parameters(statement);
            assert_eq!(seen, (1..=seen.len()).collect::<Vec<_>>(), "{statement}");
        }
    }
}
