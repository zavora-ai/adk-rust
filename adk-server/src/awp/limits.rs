//! Rate limits at the edge (spec 3.5 §7.3; AWP 2.0 §6.6).
//!
//! Limits key on [`EdgeCaller::limit_key`]: the verified requester id, else a digest of the
//! presented credential, else the peer address. Each caller has a rate class — the class its
//! owner rule names, else its tier's default — with two fixed windows:
//!
//! | Window | Counts | Applies to |
//! |---|---|---|
//! | One minute | Every edge request | `requestsPerMinute` |
//! | One day | Capability calls, per class | `callsPerDay` |
//!
//! A capability's own `rateClass` replaces the caller's for its calls. A request over a limit
//! is refused with `rate_limit_exceeded` and `Retry-After`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use awp_types::{RateClass, TrustTier};
use chrono::{DateTime, Utc};

use super::tiers::EdgeCaller;

/// How many counters the limiter keeps before it drops expired windows.
const PURGE_THRESHOLD: usize = 10_000;

/// A refused request: the class whose limit it passed and when to retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimited {
    /// The rate class.
    pub class: String,
    /// The limit.
    pub limit: u32,
    /// Seconds until the window resets.
    pub retry_after: u64,
}

/// Where a caller stands in its request window, for the `RateLimit` response fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateStanding {
    /// The rate class.
    pub class: String,
    /// The limit per minute.
    pub limit: u32,
    /// Requests left in the window.
    pub remaining: u32,
    /// Seconds until the window resets.
    pub reset: u64,
}

/// A counter's key: the caller's limit key, the class and the window.
type CounterKey = (String, String, Window);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Window {
    Minute,
    Day,
}

impl Window {
    fn seconds(self) -> i64 {
        match self {
            Window::Minute => 60,
            Window::Day => 86_400,
        }
    }
}

/// Fixed-window rate limits per caller and class (spec 3.5 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_server::awp::limits::EdgeLimits;
/// use adk_server::awp::tiers::EdgeCaller;
/// use awp_types::{RateClass, TrustTier};
///
/// let limits = EdgeLimits::default().tier_default(
///     TrustTier::Public,
///     RateClass { requests_per_minute: 1, calls_per_day: 10 },
/// );
/// let caller = EdgeCaller::public("ip:203.0.113.9");
/// assert!(limits.check_request(&caller).is_ok());
/// assert!(limits.check_request(&caller).is_err());
/// ```
#[derive(Debug)]
pub struct EdgeLimits {
    defaults: BTreeMap<TrustTier, RateClass>,
    classes: BTreeMap<String, RateClass>,
    counters: Mutex<HashMap<CounterKey, (i64, u32)>>,
}

impl Default for EdgeLimits {
    /// Public 60 requests a minute and 200 calls a day; known 120 and 2,000; verified 300 and
    /// 10,000; partner 600 and 50,000.
    fn default() -> Self {
        let class =
            |requests_per_minute, calls_per_day| RateClass { requests_per_minute, calls_per_day };
        Self {
            defaults: BTreeMap::from([
                (TrustTier::Public, class(60, 200)),
                (TrustTier::Known, class(120, 2_000)),
                (TrustTier::Verified, class(300, 10_000)),
                (TrustTier::Partner, class(600, 50_000)),
            ]),
            classes: BTreeMap::new(),
            counters: Mutex::new(HashMap::new()),
        }
    }
}

impl EdgeLimits {
    /// Sets the default class of `tier`.
    #[must_use]
    pub fn tier_default(mut self, tier: TrustTier, class: RateClass) -> Self {
        self.defaults.insert(tier, class);
        self
    }

    /// Defines the classes owner rules and capabilities name.
    #[must_use]
    pub fn classes(mut self, classes: BTreeMap<String, RateClass>) -> Self {
        self.classes.extend(classes);
        self
    }

    fn class_for(&self, caller: &EdgeCaller, override_class: Option<&str>) -> (String, RateClass) {
        let named = override_class
            .or(caller.rate_class.as_deref())
            .and_then(|name| self.classes.get(name).map(|class| (name.to_owned(), *class)));
        named.unwrap_or_else(|| {
            let class = self
                .defaults
                .get(&caller.tier)
                .copied()
                .unwrap_or(RateClass { requests_per_minute: 60, calls_per_day: 200 });
            (format!("tier-{}", caller.tier), class)
        })
    }

    fn hit(
        &self,
        key: &str,
        class: &str,
        window: Window,
        limit: u32,
        now: DateTime<Utc>,
    ) -> Result<(u32, u64), RateLimited> {
        let start = now.timestamp() - now.timestamp().rem_euclid(window.seconds());
        let reset = u64::try_from(start + window.seconds() - now.timestamp()).unwrap_or(1).max(1);
        let mut counters = self.counters.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if counters.len() > PURGE_THRESHOLD {
            counters
                .retain(|(_, _, window), (began, _)| *began + window.seconds() > now.timestamp());
        }
        let entry =
            counters.entry((key.to_owned(), class.to_owned(), window)).or_insert((start, 0));
        if entry.0 != start {
            *entry = (start, 0);
        }
        if entry.1 >= limit {
            return Err(RateLimited { class: class.to_owned(), limit, retry_after: reset });
        }
        entry.1 += 1;
        Ok((limit - entry.1, reset))
    }

    /// Counts one edge request against the caller's per-minute limit.
    ///
    /// # Errors
    ///
    /// [`RateLimited`] when the caller passed it.
    pub fn check_request(&self, caller: &EdgeCaller) -> Result<RateStanding, RateLimited> {
        let (class, limits) = self.class_for(caller, None);
        let (remaining, reset) = self.hit(
            &caller.limit_key,
            &class,
            Window::Minute,
            limits.requests_per_minute,
            Utc::now(),
        )?;
        Ok(RateStanding { class, limit: limits.requests_per_minute, remaining, reset })
    }

    /// Counts one capability call against the daily limit of the caller's class, or of
    /// `capability_class` when the capability names one.
    ///
    /// # Errors
    ///
    /// [`RateLimited`] when the caller passed it.
    pub fn check_call(
        &self,
        caller: &EdgeCaller,
        capability_class: Option<&str>,
    ) -> Result<(), RateLimited> {
        let (class, limits) = self.class_for(caller, capability_class);
        self.hit(&caller.limit_key, &class, Window::Day, limits.calls_per_day, Utc::now())
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_key_on_the_caller_and_class() {
        let limits = EdgeLimits::default()
            .tier_default(TrustTier::Known, RateClass { requests_per_minute: 2, calls_per_day: 1 })
            .classes(BTreeMap::from([(
                "bulk".to_owned(),
                RateClass { requests_per_minute: 1, calls_per_day: 5 },
            )]));
        let known = |id: &str| EdgeCaller {
            tier: TrustTier::Known,
            requester: Some(awp_types::RequesterId::new(id)),
            limit_key: format!("id:{id}"),
            rate_class: None,
            claimed_agent: None,
        };
        let (a, b) = (known("a"), known("b"));
        assert!(limits.check_request(&a).is_ok());
        assert!(limits.check_request(&a).is_ok());
        let refused = limits.check_request(&a).unwrap_err();
        assert_eq!(refused.class, "tier-known");
        assert!(refused.retry_after >= 1 && refused.retry_after <= 60);
        assert!(limits.check_request(&b).is_ok(), "another requester has its own window");

        assert!(limits.check_call(&a, None).is_ok());
        assert!(limits.check_call(&a, None).is_err());
        assert!(
            limits.check_call(&a, Some("bulk")).is_ok(),
            "a capability's class has its own window"
        );
    }
}
