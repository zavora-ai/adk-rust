//! Encoding shared by the SQL lease, run, schedule, and governance stores.
//!
//! Timestamps are stored as microseconds since the epoch in `BIGINT` columns, structured
//! values as JSON text, and counters as `BIGINT`, so one statement text serves SQLite and
//! PostgreSQL. Instants are truncated to microseconds before they are stored or
//! returned, so a value a store returns equals the value it later reads back.

use std::time::Duration;

use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Builds the `Unavailable` error a store returns when its database fails.
pub(crate) fn storage_error(code: &'static str, message: impl std::fmt::Display) -> AdkError {
    AdkError::new(ErrorComponent::Session, ErrorCategory::Unavailable, code, message.to_string())
}

/// `instant` truncated to microseconds, with its stored form.
pub(crate) fn micros(instant: DateTime<Utc>) -> (DateTime<Utc>, i64) {
    let us = instant.timestamp_micros();
    (DateTime::from_timestamp_micros(us).unwrap_or(instant), us)
}

/// The clock's current time truncated to microseconds, with its stored form.
pub(crate) fn now(clock: &dyn Clock) -> (DateTime<Utc>, i64) {
    micros(clock.now())
}

/// Reads a stored microsecond timestamp.
pub(crate) fn from_micros(code: &'static str, column: &str, us: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(us)
        .ok_or_else(|| storage_error(code, format!("column '{column}' is out of range: {us}")))
}

/// `now + ttl` truncated to microseconds, refusing a zero or overflowing `ttl` with
/// error `ttl_code`, as the in-memory stores do.
pub(crate) fn deadline(
    now: DateTime<Utc>,
    ttl: Duration,
    ttl_code: &'static str,
) -> Result<(DateTime<Utc>, i64)> {
    chrono::Duration::from_std(ttl)
        .ok()
        .filter(|_| !ttl.is_zero())
        .and_then(|ttl| now.checked_add_signed(ttl))
        .map(micros)
        .ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                ttl_code,
                format!("time to live {ttl:?} must be greater than zero and fit in a timestamp"),
            )
        })
}

/// Serializes `value` to JSON text.
pub(crate) fn to_json<T: Serialize>(code: &'static str, value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| storage_error(code, format!("serialize failed: {e}")))
}

/// Parses JSON text read from `column`.
pub(crate) fn from_json<T: DeserializeOwned>(
    code: &'static str,
    column: &str,
    json: &str,
) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|e| storage_error(code, format!("column '{column}' is corrupt: {e}")))
}

/// Converts an unsigned counter to its `BIGINT` form.
pub(crate) fn to_i64(code: &'static str, value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| storage_error(code, format!("{value} does not fit in a BIGINT column")))
}

/// Reads a `BIGINT` counter that must not be negative.
pub(crate) fn to_u64(code: &'static str, column: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| storage_error(code, format!("column '{column}' is negative: {value}")))
}

/// Reads a `BIGINT` counter that must fit in a `u32`.
pub(crate) fn to_u32(code: &'static str, column: &str, value: i64) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| storage_error(code, format!("column '{column}' is out of range: {value}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micros_truncates_and_round_trips() {
        let instant = DateTime::from_timestamp(1_800_000_000, 123_456_789).unwrap();
        let (truncated, us) = micros(instant);
        assert_eq!(us, 1_800_000_000_123_456);
        assert_eq!(from_micros("c", "t", us).unwrap(), truncated);
        assert_eq!(truncated.timestamp_subsec_nanos(), 123_456_000);
    }

    #[test]
    fn deadline_refuses_zero_ttl() {
        let now = DateTime::from_timestamp(0, 0).unwrap();
        assert_eq!(
            deadline(now, Duration::ZERO, "x.invalid_ttl").unwrap_err().code,
            "x.invalid_ttl"
        );
        assert_eq!(deadline(now, Duration::from_secs(2), "x").unwrap().1, 2_000_000);
    }
}
