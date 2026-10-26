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
    ConsentReceipt, ConsentScope, EventType, HeldCallState, LeadRef, RequesterId, SiteStatus,
};
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

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

/// The steps both backends run; the schema is portable.
const MIGRATIONS: &[(i64, &str, &str)] = &[(1, "create the AWP edge store tables", SCHEMA)];

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

const IDEM_BEGIN: &str = "INSERT INTO adk_awp_idempotency (site, key, digest, began_us, outcome) \
VALUES ($1, $2, $3, $4, NULL) ON CONFLICT (site, key) DO NOTHING";
const IDEM_GET: &str =
    "SELECT digest, outcome FROM adk_awp_idempotency WHERE site = $1 AND key = $2";
const IDEM_COMPLETE: &str = "UPDATE adk_awp_idempotency SET outcome = $1 \
WHERE site = $2 AND key = $3 AND outcome IS NULL";
const IDEM_PURGE: &str = "DELETE FROM adk_awp_idempotency WHERE site = $1 AND began_us < $2";

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

/// The six edge stores in SQLite (spec 3.5 §7.9, feature `awp-sqlite`).
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

    /// The six stores as the edge takes them.
    pub fn edge_stores(&self) -> crate::awp::EdgeStores {
        let this = std::sync::Arc::new(self.clone());
        crate::awp::EdgeStores::new(this.clone(), this.clone(), this.clone(), this.clone())
            .with_subscriptions(this.clone())
            .with_leads(this)
    }
}

#[cfg(feature = "awp-sqlite")]
sql_edge_stores!(SqliteEdgeStores, "sqlite");

/// The six edge stores in PostgreSQL, shared by every edge replica (spec 3.5 §7.9, feature
/// `awp-postgres`).
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

    /// The six stores as the edge takes them.
    pub fn edge_stores(&self) -> crate::awp::EdgeStores {
        let this = std::sync::Arc::new(self.clone());
        crate::awp::EdgeStores::new(this.clone(), this.clone(), this.clone(), this.clone())
            .with_subscriptions(this.clone())
            .with_leads(this)
    }
}

#[cfg(feature = "awp-postgres")]
sql_edge_stores!(PostgresEdgeStores, "postgres");

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
            IDEM_BEGIN,
            IDEM_GET,
            IDEM_COMPLETE,
            IDEM_PURGE,
        ] {
            let seen = parameters(statement);
            assert_eq!(seen, (1..=seen.len()).collect::<Vec<_>>(), "{statement}");
        }
    }
}
