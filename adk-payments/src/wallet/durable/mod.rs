//! Durable [`WalletStore`](super::WalletStore) backends, so a site's payments, held payments
//! and paused categories survive process loss and are shared by every instance (spec 3.5
//! §7.14, A-I16).
//!
//! | Backend | Feature | Insert-if-absent and resume |
//! |---------|---------|-----------------------------|
//! | [`SqliteWalletStore`] | `sqlite` | One `INSERT … ON CONFLICT DO NOTHING` on the wallet and key; one `DELETE … RETURNING` |
//! | [`PostgresWalletStore`] | `postgres` | The same statements, shared by every replica |
//! | [`RedisWalletStore`] | `redis` | One Lua script each, which Redis runs atomically |
//!
//! Each stores the [`PaymentRecord`](super::PaymentRecord) and
//! [`CategoryPause`](super::CategoryPause) as JSON, so a record reads back exactly as written,
//! and keeps the fields a query selects on beside it. A listing reads candidates by those
//! fields and keeps the ones [`PaymentQuery::matches`](super::PaymentQuery::matches), in
//! insertion order, as the in-memory store does.
//!
//! The store keeps the journal; the cap is the spend ledger's. Two wallet instances share a
//! cap when they share a store and a durable spend ledger built with the policy's
//! [`spend_limits`](super::WalletPolicy::spend_limits), such as adk-session's
//! `SqliteSpendLedger`, `PostgresSpendLedger` or `RedisSpendLedger` over the same database.

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use chrono::{DateTime, Utc};

use super::store::{CategoryPause, PaymentRecord, PaymentState};

#[cfg(feature = "redis")]
mod redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;

#[cfg(feature = "redis")]
pub use redis::RedisWalletStore;
#[cfg(feature = "postgres")]
pub use sql::PostgresWalletStore;
#[cfg(feature = "sqlite")]
pub use sql::SqliteWalletStore;

/// A durable wallet store could not be reached, or a statement failed (spec 3.5 §7.14).
/// Retryable.
pub const WALLET_STORE_UNAVAILABLE_CODE: &str = "wallet.store_unavailable";

/// A durable wallet store holds a record that does not read back, or a layout a newer
/// release wrote (spec 3.5 §7.14).
pub const WALLET_STORE_CORRUPT_CODE: &str = "wallet.store_corrupt";

/// The store could not `action`.
fn unavailable(backend: &str, action: &str, error: &dyn std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Guardrail,
        ErrorCategory::Unavailable,
        WALLET_STORE_UNAVAILABLE_CODE,
        format!("the {backend} wallet store could not {action}: {error}"),
    )
}

/// The store holds something this release cannot read.
fn corrupt(backend: &str, message: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Guardrail,
        ErrorCategory::Internal,
        WALLET_STORE_CORRUPT_CODE,
        format!("the {backend} wallet store {message}"),
    )
}

/// `value` as stored.
fn encode<T: serde::Serialize>(backend: &str, value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| corrupt(backend, format!("cannot encode: {error}")))
}

/// A stored `what` read back.
fn decode<T: serde::de::DeserializeOwned>(backend: &str, what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text)
        .map_err(|error| corrupt(backend, format!("holds a corrupt {what}: {error}")))
}

/// The stored name of `state`, its wire name.
fn state_name(state: PaymentState) -> &'static str {
    match state {
        PaymentState::Held => "held",
        PaymentState::Executing => "executing",
        PaymentState::Settled => "settled",
        PaymentState::Rejected => "rejected",
        PaymentState::Failed => "failed",
    }
}

/// The instant a window query selects `record` by: its settlement, else its creation, in
/// microseconds since the Unix epoch, rounded down.
fn instant_micros(record: &PaymentRecord) -> i64 {
    micros(record.settled_at.unwrap_or(record.created_at))
}

/// `at` in microseconds since the Unix epoch, rounded down, so `a >= b` implies
/// `micros(a) >= micros(b)` and a stored range is never narrower than the query's.
fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

/// `pauses` in category order, as the in-memory store lists them.
fn in_category_order(mut pauses: Vec<CategoryPause>) -> Vec<CategoryPause> {
    pauses.sort_by_key(|pause| pause.category);
    pauses
}

/// `wallet.payment_not_found` for an update of a payment the store does not hold.
fn payment_not_found(record: &PaymentRecord) -> AdkError {
    super::error::not_found(
        super::error::WALLET_PAYMENT_NOT_FOUND_CODE,
        format!("wallet {} has no payment {}", record.wallet, record.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn state_names_are_the_wire_names() {
        for state in [
            PaymentState::Held,
            PaymentState::Executing,
            PaymentState::Settled,
            PaymentState::Rejected,
            PaymentState::Failed,
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), state_name(state));
        }
    }

    #[test]
    fn micros_round_down_so_ranges_only_widen() {
        let base = Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap();
        let early = base + chrono::Duration::nanoseconds(400);
        let late = base + chrono::Duration::nanoseconds(900);
        assert!(late >= early);
        assert!(micros(late) >= micros(early));
        assert_eq!(micros(early), micros(base));
        let before_epoch = Utc.timestamp_opt(-1, 500).unwrap();
        assert_eq!(micros(before_epoch), -1_000_000);
    }
}
