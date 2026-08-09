//! Per-node retry with exponential backoff.
//!
//! A node that fails aborts the run. That is right for a logic error and wrong
//! for a call to a service that is briefly unavailable, so a policy can be
//! attached per node to try again with a growing delay.
//!
//! Retry is off unless configured: a node with no policy runs once. A policy
//! from [`RetryPolicy::default`] allows ten attempts,
//! so a graph that sets no policy behaves exactly as before.
//!
//! # Example
//!
//! ```rust
//! use adk_graph::retry::{RetryOn, RetryPolicy};
//! use std::time::Duration;
//!
//! let policy = RetryPolicy::new(4)
//!     .with_initial_delay(Duration::from_millis(200))
//!     .with_max_delay(Duration::from_secs(5))
//!     .with_backoff_factor(2.0)
//!     .with_jitter(0.0)
//!     .with_retry_on(RetryOn::Any);
//!
//! // Attempt 1 fails, then 200ms, 400ms, 800ms.
//! assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
//! assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(400));
//! assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(800));
//! ```

use std::sync::Arc;
use std::time::Duration;

use crate::error::GraphError;

/// Which failures a policy retries.
#[derive(Clone)]
pub enum RetryOn {
    /// Every failure except an interrupt.
    ///
    /// An interrupt is control flow, not an error: it means a person or a node
    /// asked the run to pause. Retrying it would spin. `Any` therefore excludes
    /// it, which is worth stating because the name reads as though it would not.
    Any,
    /// Only a node timeout.
    Timeout,
    /// A caller-supplied predicate.
    Custom(Arc<dyn Fn(&GraphError) -> bool + Send + Sync>),
}

impl std::fmt::Debug for RetryOn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => f.write_str("Any"),
            Self::Timeout => f.write_str("Timeout"),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

impl RetryOn {
    /// Whether this error should be retried.
    pub fn should_retry(&self, error: &GraphError) -> bool {
        // Never retry an interrupt, whatever the policy says: a pause that
        // retried would defeat the pause.
        if matches!(error, GraphError::Interrupted(_)) {
            return false;
        }
        match self {
            Self::Any => true,
            Self::Timeout => matches!(error, GraphError::NodeTimedOut { .. }),
            Self::Custom(predicate) => predicate(error),
        }
    }
}

/// How many times a node is attempted, and how long between attempts.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `1` means no retry.
    pub max_attempts: u32,
    /// Delay before the second attempt.
    pub initial_delay: Duration,
    /// Ceiling on any single delay.
    pub max_delay: Duration,
    /// Multiplier applied to the delay after each attempt.
    pub backoff_factor: f64,
    /// Fraction of the delay to vary randomly, so retries from many nodes do not
    /// align. `0.0` is exact, `1.0` varies the delay across its whole width.
    pub jitter: f64,
    /// Which failures to retry.
    pub retry_on: RetryOn,
}

/// Ten attempts, one second of initial delay, doubling to a sixty-second cap.
///
/// The nine sleeps between ten attempts total about 243 seconds, so a node that
/// keeps failing takes roughly four minutes to give up, plus the time each attempt
/// itself takes. Lower `max_attempts` where a caller is waiting.
///
/// Attaching no policy at all is still one attempt: this default applies only to a
/// policy you construct.
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_factor: 2.0,
            jitter: 1.0,
            retry_on: RetryOn::Any,
        }
    }
}

impl RetryPolicy {
    /// A policy allowing `max_attempts` in total, with the other defaults.
    pub fn new(max_attempts: u32) -> Self {
        Self { max_attempts: max_attempts.max(1), ..Default::default() }
    }

    /// Set the delay before the second attempt.
    pub fn with_initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Set the ceiling on any single delay.
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set the multiplier applied after each attempt.
    pub fn with_backoff_factor(mut self, factor: f64) -> Self {
        self.backoff_factor = factor;
        self
    }

    /// Set how much the delay varies randomly, as a fraction of itself.
    pub fn with_jitter(mut self, jitter: f64) -> Self {
        self.jitter = jitter.clamp(0.0, 1.0);
        self
    }

    /// Set which failures to retry.
    pub fn with_retry_on(mut self, retry_on: RetryOn) -> Self {
        self.retry_on = retry_on;
        self
    }

    /// Whether another attempt is allowed after `attempts_so_far` failures.
    pub fn allows_another_attempt(&self, attempts_so_far: u32) -> bool {
        attempts_so_far < self.max_attempts
    }

    /// The delay before attempt number `attempt`, counting the first attempt as
    /// zero, so `delay_for_attempt(1)` precedes the second attempt.
    ///
    /// The delay grows by `backoff_factor` each time, is clamped to `max_delay`,
    /// and is then varied by up to `jitter` of itself. A `backoff_factor` at or
    /// below zero is treated as `1.0`, giving a constant delay rather than
    /// collapsing to nothing.
    ///
    /// The clamp happens before the jitter, so a jittered delay can sit slightly
    /// above `max_delay` only when `jitter` is positive; with `jitter` at zero the
    /// ceiling is exact.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let factor = if self.backoff_factor <= 0.0 { 1.0 } else { self.backoff_factor };
        let mut millis = self.initial_delay.as_millis() as f64;
        for _ in 1..attempt {
            millis *= factor;
        }
        let capped = millis.min(self.max_delay.as_millis() as f64);

        if self.jitter <= 0.0 {
            return Duration::from_millis(capped as u64);
        }
        // Vary symmetrically around the capped delay, floored at zero.
        let spread = capped * self.jitter;
        let offset = pseudo_random_unit() * 2.0 * spread - spread;
        Duration::from_millis((capped + offset).max(0.0) as u64)
    }
}

/// A value in `[0, 1)` derived from the clock.
///
/// Jitter only has to spread retries apart; it is not a security primitive, so
/// this avoids taking a dependency on a random number generator.
fn pseudo_random_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constructed default retries; attaching no policy at all does not. The
    /// second half of that is the executor's business, not this type's.
    #[test]
    fn a_default_policy_retries_ten_times() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, 10);
        assert!(policy.allows_another_attempt(1), "a first failure is retried");
        assert!(policy.allows_another_attempt(9), "and so is a ninth");
        assert!(!policy.allows_another_attempt(10), "the tenth is the last");
    }

    /// The nine sleeps between ten attempts, so the wall-clock cost is stated in
    /// one place rather than inferred from the backoff rules.
    #[test]
    fn the_default_gives_up_after_about_four_minutes() {
        let policy = RetryPolicy::default().with_jitter(0.0);
        let total: Duration = (1..policy.max_attempts).map(|n| policy.delay_for_attempt(n)).sum();
        assert_eq!(total, Duration::from_secs(243));
    }

    #[test]
    fn delays_grow_by_the_backoff_factor() {
        let policy = RetryPolicy::new(5)
            .with_initial_delay(Duration::from_millis(100))
            .with_backoff_factor(3.0)
            .with_jitter(0.0);
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(300));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(900));
    }

    #[test]
    fn a_delay_is_capped() {
        let policy = RetryPolicy::new(10)
            .with_initial_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_millis(250))
            .with_backoff_factor(10.0)
            .with_jitter(0.0);
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(250));
    }

    #[test]
    fn a_non_positive_backoff_factor_gives_a_constant_delay() {
        let policy = RetryPolicy::new(4)
            .with_initial_delay(Duration::from_millis(50))
            .with_backoff_factor(0.0)
            .with_jitter(0.0);
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(50));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(50));
    }

    #[test]
    fn an_interrupt_is_never_retried() {
        let interrupt = GraphError::Interrupted(Box::new(crate::error::InterruptedExecution::new(
            "t".to_string(),
            "c".to_string(),
            crate::interrupt::Interrupt::Before("n".to_string()),
            Default::default(),
            0,
        )));
        assert!(!RetryOn::Any.should_retry(&interrupt));
        let always = RetryOn::Custom(Arc::new(|_| true));
        assert!(!always.should_retry(&interrupt), "even a permissive predicate must not retry it");
    }

    #[test]
    fn timeout_only_retries_a_timeout() {
        let timeout =
            GraphError::NodeTimedOut { node: "slow".to_string(), elapsed: Duration::from_secs(1) };
        let other =
            GraphError::NodeExecutionFailed { node: "n".to_string(), message: "boom".to_string() };
        assert!(RetryOn::Timeout.should_retry(&timeout));
        assert!(!RetryOn::Timeout.should_retry(&other));
        assert!(RetryOn::Any.should_retry(&other));
    }
}
