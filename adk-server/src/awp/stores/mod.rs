//! Durable edge stores: SQLite, PostgreSQL and Redis implementations of `adk-awp`'s six edge
//! store traits (spec 3.5 §7.9).
//!
//! | Backend | Feature | Atomic steps |
//! |---------|---------|--------------|
//! | [`SqliteEdgeStores`] | `awp-sqlite` | `INSERT … ON CONFLICT DO NOTHING` for every create and idempotency `begin`; compare-and-set `UPDATE`s for subscriptions, held calls and outcomes |
//! | [`PostgresEdgeStores`] | `awp-postgres` | The same statements, shared by every edge replica |
//! | [`RedisEdgeStores`] | `awp-redis` | `HSETNX` creates, a compare-and-set Lua script, and `SET … GET` for the status |
//!
//! Each backend implements [`SubscriptionStore`](adk_awp::v2::store::SubscriptionStore),
//! [`ConsentReceiptStore`](adk_awp::v2::store::ConsentReceiptStore),
//! [`StatusStore`](adk_awp::v2::store::StatusStore),
//! [`HeldCallStore`](adk_awp::v2::store::HeldCallStore),
//! [`LeadStore`](adk_awp::v2::store::LeadStore) and
//! [`IdempotencyStore`](adk_awp::v2::store::IdempotencyStore), passes
//! `adk_awp::edge_store_conformance_tests!`, and keeps one site's records under its `site`
//! namespace, so several sites share one database (spec 3.5 A-I22). Records are stored as JSON
//! beside the columns queries select on, so they read back exactly as written.

use adk_awp::v2::store::StoreError;

#[cfg(feature = "awp-redis")]
mod redis;
#[cfg(any(feature = "awp-sqlite", feature = "awp-postgres"))]
mod sql;

#[cfg(feature = "awp-redis")]
pub use redis::RedisEdgeStores;
#[cfg(feature = "awp-postgres")]
pub use sql::PostgresEdgeStores;
#[cfg(feature = "awp-sqlite")]
pub use sql::SqliteEdgeStores;

/// The namespace a store keeps records under unless one is set.
pub const DEFAULT_SITE: &str = "default";

/// A backend failure, as the store traits report it.
fn backend(name: &str, action: &str, error: &dyn std::fmt::Display) -> StoreError {
    StoreError::Backend(format!("the {name} edge store could not {action}: {error}"))
}

/// `value` as stored.
fn encode<T: serde::Serialize>(name: &str, value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| backend(name, "encode a record", &error))
}

/// A stored record read back.
fn decode<T: serde::de::DeserializeOwned>(name: &str, text: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|error| backend(name, "read a stored record", &error))
}

/// The wire name of a serde unit or string value, such as a scope or a state.
#[cfg(any(feature = "awp-sqlite", feature = "awp-postgres"))]
fn wire_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// How many times a compare-and-set retries under contention.
const CAS_ATTEMPTS: usize = 16;

/// Applies `update` to a stored subscription, as every backend does: reactivation clears the
/// failure count.
fn apply_update(
    record: &mut adk_awp::v2::store::SubscriptionRecord,
    update: adk_awp::v2::store::SubscriptionUpdate,
) {
    if let Some(types) = update.types {
        record.subscription.types = types;
    }
    if let Some(url) = update.url {
        record.subscription.url = url;
    }
    if let Some(state) = update.state {
        if state == awp_types::SubscriptionState::Active {
            record.subscription.failures = 0;
        }
        record.subscription.state = state;
    }
}

/// Records a delivery outcome on a stored subscription: success clears the failures; the
/// sixth consecutive failure suspends it (spec 3.5 §7.5).
fn apply_delivery(record: &mut adk_awp::v2::store::SubscriptionRecord, delivered: bool) {
    if delivered {
        record.subscription.failures = 0;
    } else {
        record.subscription.failures = record.subscription.failures.saturating_add(1);
        if record.subscription.failures >= awp_types::SubscriptionSuspended::MAX_FAILURES {
            record.subscription.state = awp_types::SubscriptionState::Suspended;
        }
    }
}

/// The held call `record` becomes under `transition`, or why it cannot (spec 3.5 §7.4).
fn apply_transition(
    mut record: adk_awp::v2::store::HeldCallRecord,
    transition: adk_awp::v2::store::HeldCallTransition,
) -> Result<adk_awp::v2::store::HeldCallRecord, StoreError> {
    let id = record.call.id.clone();
    if !adk_awp::v2::store::held_call_transition_allowed(record.call.state, transition.to) {
        return Err(StoreError::Conflict(format!(
            "held call {id} cannot move from {:?} to {:?}",
            record.call.state, transition.to
        )));
    }
    if transition.to == awp_types::HeldCallState::Completed
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
    Ok(record)
}

/// The stored idempotency entry of a key.
#[cfg(feature = "awp-redis")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdempotencyEntry {
    digest: String,
    began_us: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<adk_awp::v2::store::RecordedOutcome>,
}
