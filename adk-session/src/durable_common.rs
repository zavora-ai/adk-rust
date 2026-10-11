//! Helpers shared by the Redis, MongoDB, Neo4j, and Firestore durable runtime stores.

use std::time::Duration;

use adk_core::run_store::RunState;
use adk_core::schedule::Schedule;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use chrono::{DateTime, SubsecRound, Utc};

/// Error code for a lease store backend failure.
pub(crate) const LEASE_STORAGE: &str = "session.lease_store.storage";
/// Error code for a run store backend failure.
pub(crate) const RUN_STORAGE: &str = "session.run_store.storage";
/// Error code for a schedule store backend failure.
pub(crate) const SCHEDULE_STORAGE: &str = "session.schedule_store.storage";
/// Error code for a governance state store backend failure.
pub(crate) const GOVERNANCE_STORAGE: &str = "session.governance_store.storage";

/// Builds the error a store returns when its backend fails; `code` is one of the
/// `*_STORAGE` constants.
pub(crate) fn storage_error(code: &'static str, message: impl std::fmt::Display) -> AdkError {
    AdkError::new(ErrorComponent::Session, ErrorCategory::Unavailable, code, message.to_string())
}

/// Truncates `now` to whole microseconds, the precision every backend stores times at.
pub(crate) fn stored_now(now: DateTime<Utc>) -> DateTime<Utc> {
    now.trunc_subsecs(6)
}

/// Returns `now + ttl`, refusing a zero or overflowing `ttl` with error `code`.
pub(crate) fn deadline(
    now: DateTime<Utc>,
    ttl: Duration,
    code: &'static str,
) -> Result<DateTime<Utc>> {
    chrono::Duration::from_std(ttl)
        .ok()
        .filter(|_| !ttl.is_zero())
        .and_then(|ttl| now.checked_add_signed(ttl))
        .map(stored_now)
        .ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                code,
                format!("time to live {ttl:?} must be greater than zero and fit in a timestamp"),
            )
        })
}

/// The state name stored next to a record, for queries.
pub(crate) fn state_name(state: RunState) -> &'static str {
    match state {
        RunState::Queued => "queued",
        RunState::Running => "running",
        RunState::Waiting => "waiting",
        RunState::Completed => "completed",
        RunState::Failed => "failed",
        RunState::Cancelled => "cancelled",
        _ => "unknown",
    }
}

/// Whether `stored` already holds `incoming`'s definition, so an upsert changes nothing.
pub(crate) fn same_definition(stored: &Schedule, incoming: &Schedule) -> bool {
    stored.spec == incoming.spec
        && stored.run == incoming.run
        && stored.missed == incoming.missed
        && stored.paused == incoming.paused
        && stored.next_fire_at == incoming.next_fire_at
}

/// Lowercase hex of `text`'s bytes: safe in keys, document IDs, and paths, and ordered
/// like the text itself.
pub(crate) fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn deadlines_refuse_zero_and_truncate() {
        let now = Utc.timestamp_opt(1_800_000_000, 0).single().unwrap();
        assert!(deadline(now, Duration::ZERO, "x").is_err());
        let late = deadline(now, Duration::from_nanos(1_500), "x").unwrap();
        assert_eq!(late, now + chrono::Duration::microseconds(1));
    }

    #[test]
    fn hex_keeps_text_order() {
        let (a, b) = ("conformance:a/ü", "conformance:b");
        assert!(hex(a) < hex(b));
        assert_eq!(hex("a/"), "612f");
    }
}
