//! Time sources for stores whose behaviour depends on the current time.
//!
//! Spend ledgers read the time from a [`Clock`] to pick period windows and to expire
//! reservations. Production code uses [`SystemClock`]; tests and conformance suites
//! use [`ManualClock`] to cross a day or month boundary without waiting for it.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Source of the current UTC time.
///
/// # Example
///
/// ```rust
/// use adk_core::{Clock, SystemClock};
///
/// let before = chrono::Utc::now();
/// assert!(SystemClock.now() >= before);
/// ```
pub trait Clock: fmt::Debug + Send + Sync {
    /// Returns the current time.
    fn now(&self) -> DateTime<Utc>;
}

/// The operating system's wall clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that reads a time the caller sets and moves only when told to.
///
/// Clones share the same time, so a test can hand one clone to a store and advance
/// another.
///
/// # Example
///
/// ```rust
/// use adk_core::{Clock, ManualClock};
/// use chrono::{TimeZone, Utc};
/// use std::time::Duration;
///
/// let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 31, 23, 59, 0).unwrap());
/// clock.advance(Duration::from_secs(120));
/// assert_eq!(clock.now(), Utc.with_ymd_and_hms(2026, 11, 1, 0, 1, 0).unwrap());
/// ```
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    /// Creates a clock that reads `at`.
    pub fn new(at: DateTime<Utc>) -> Self {
        Self { now: Arc::new(Mutex::new(at)) }
    }

    /// Sets the time the clock reads.
    pub fn set(&self, at: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = at;
    }

    /// Moves the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        let by = chrono::Duration::from_std(by).unwrap_or(chrono::Duration::MAX);
        let mut now = self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *now = now.checked_add_signed(by).unwrap_or(DateTime::<Utc>::MAX_UTC);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
