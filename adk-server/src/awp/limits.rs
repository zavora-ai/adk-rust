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
//!
//! The windows are counted in the process unless [`EdgeLimits::store`] shares them through a
//! [`RateLimitStore`]: the SQLite, PostgreSQL and Redis edge stores implement it, so edge
//! replicas and a restarted edge count against one window (spec 3.5 §7.9). A store that
//! fails is logged and the process's own counters answer until it recovers.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_awp::v2::store::StoreResult;
use async_trait::async_trait;
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

    fn name(self) -> &'static str {
        match self {
            Window::Minute => "minute",
            Window::Day => "day",
        }
    }
}

/// One caller's fixed window for one rate class (spec 3.5 §7.3, §7.9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RateWindow {
    /// The caller's limit key, such as `ip:203.0.113.9` or `id:{requester}`.
    pub key: String,
    /// The rate class.
    pub class: String,
    /// `minute` for requests, `day` for capability calls.
    pub period: String,
    /// When the window began, in Unix seconds.
    pub start: i64,
    /// How long it lasts, in seconds.
    pub seconds: i64,
}

/// Where the edge counts its rate-limit windows, shared by every replica and kept across a
/// restart (spec 3.5 §7.9).
///
/// [`InMemoryRateLimitStore`] keeps them in the process; the SQLite, PostgreSQL and Redis
/// edge stores (`awp-sqlite`, `awp-postgres`, `awp-redis`) share them. Every implementation
/// passes [`rate_limit_conformance_tests!`](crate::rate_limit_conformance_tests).
///
/// # Example
///
/// ```rust
/// use adk_server::awp::limits::{InMemoryRateLimitStore, RateLimitStore, RateWindow};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), adk_awp::v2::store::StoreError> {
/// let store = InMemoryRateLimitStore::default();
/// let window = RateWindow {
///     key: "ip:203.0.113.9".into(),
///     class: "tier-public".into(),
///     period: "minute".into(),
///     start: 1_761_465_600,
///     seconds: 60,
/// };
/// assert_eq!(store.hit(&window, 1).await?, Some(1));
/// assert_eq!(store.hit(&window, 1).await?, None, "the second request passes the limit");
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait RateLimitStore: Send + Sync {
    /// Counts one hit in `window` unless its count has reached `limit`: the count after the
    /// hit, or `None` when the hit is refused. A window that began later than the stored one
    /// for the same key, class and period starts again from zero. Atomic: concurrent hits
    /// never pass the limit together.
    ///
    /// # Errors
    ///
    /// The backend's failure.
    async fn hit(&self, window: &RateWindow, limit: u32) -> StoreResult<Option<u32>>;

    /// Drops the windows that began before `before`, in Unix seconds; returns how many. A
    /// backend whose windows expire by themselves drops none.
    ///
    /// # Errors
    ///
    /// The backend's failure.
    async fn purge_before(&self, before: i64) -> StoreResult<usize>;
}

/// A window's key in memory: the caller's limit key, the class and the period.
type WindowKey = (String, String, String);

/// A [`RateLimitStore`] in the process, shared by its clones.
#[derive(Debug, Clone, Default)]
pub struct InMemoryRateLimitStore {
    windows: Arc<Mutex<HashMap<WindowKey, (i64, u32)>>>,
}

#[async_trait]
impl RateLimitStore for InMemoryRateLimitStore {
    async fn hit(&self, window: &RateWindow, limit: u32) -> StoreResult<Option<u32>> {
        let mut windows = self.windows.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = windows
            .entry((window.key.clone(), window.class.clone(), window.period.clone()))
            .or_insert((window.start, 0));
        if entry.0 != window.start {
            *entry = (window.start, 0);
        }
        if entry.1 >= limit {
            return Ok(None);
        }
        entry.1 += 1;
        Ok(Some(entry.1))
    }

    async fn purge_before(&self, before: i64) -> StoreResult<usize> {
        let mut windows = self.windows.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = windows.len();
        windows.retain(|_, (start, _)| *start >= before);
        Ok(count - windows.len())
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
pub struct EdgeLimits {
    defaults: BTreeMap<TrustTier, RateClass>,
    classes: BTreeMap<String, RateClass>,
    counters: Mutex<HashMap<CounterKey, (i64, u32)>>,
    store: Option<Arc<dyn RateLimitStore>>,
    store_hits: AtomicU64,
}

impl std::fmt::Debug for EdgeLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeLimits")
            .field("defaults", &self.defaults)
            .field("classes", &self.classes)
            .field("shared", &self.store.is_some())
            .finish_non_exhaustive()
    }
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
            store: None,
            store_hits: AtomicU64::new(0),
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

    /// Counts the windows in `store`, shared by every edge over it and kept across a restart
    /// (spec 3.5 §7.9); the edge calls [`admit_request`](Self::admit_request) and
    /// [`admit_call`](Self::admit_call), which read it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_server::awp::limits::{EdgeLimits, InMemoryRateLimitStore};
    /// use adk_server::awp::tiers::EdgeCaller;
    /// use awp_types::{RateClass, TrustTier};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let windows = Arc::new(InMemoryRateLimitStore::default());
    /// let class = RateClass { requests_per_minute: 1, calls_per_day: 10 };
    /// let first = EdgeLimits::default().tier_default(TrustTier::Public, class).store(windows.clone());
    /// let second = EdgeLimits::default().tier_default(TrustTier::Public, class).store(windows);
    /// let caller = EdgeCaller::public("ip:203.0.113.9");
    /// assert!(first.admit_request(&caller).await.is_ok());
    /// assert!(second.admit_request(&caller).await.is_err(), "one window across both edges");
    /// # }
    /// ```
    #[must_use]
    pub fn store(mut self, store: Arc<dyn RateLimitStore>) -> Self {
        self.store = Some(store);
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

    /// Counts one hit in the store's window, else in the process's: the shared windows when
    /// the limits have a [`store`](Self::store).
    async fn hit_shared(
        &self,
        key: &str,
        class: &str,
        window: Window,
        limit: u32,
        now: DateTime<Utc>,
    ) -> Result<(u32, u64), RateLimited> {
        let Some(store) = &self.store else {
            return self.hit(key, class, window, limit, now);
        };
        let start = now.timestamp() - now.timestamp().rem_euclid(window.seconds());
        let reset = u64::try_from(start + window.seconds() - now.timestamp()).unwrap_or(1).max(1);
        if limit == 0 {
            return Err(RateLimited { class: class.to_owned(), limit, retry_after: reset });
        }
        let shared = RateWindow {
            key: key.to_owned(),
            class: class.to_owned(),
            period: window.name().to_owned(),
            start,
            seconds: window.seconds(),
        };
        match store.hit(&shared, limit).await {
            Ok(Some(count)) => {
                if self.store_hits.fetch_add(1, Ordering::Relaxed) % PURGE_THRESHOLD as u64
                    == PURGE_THRESHOLD as u64 - 1
                {
                    let before = now.timestamp() - Window::Day.seconds();
                    if let Err(error) = store.purge_before(before).await {
                        tracing::warn!(error = %error, "awp rate windows could not be purged");
                    }
                }
                Ok((limit.saturating_sub(count), reset))
            }
            Ok(None) => Err(RateLimited { class: class.to_owned(), limit, retry_after: reset }),
            Err(error) => {
                tracing::warn!(error = %error, "the shared rate-limit store failed; counting in the process");
                self.hit(key, class, window, limit, now)
            }
        }
    }

    /// Counts one edge request against the caller's per-minute limit, in the shared windows
    /// when the limits have a [`store`](Self::store) (spec 3.5 §7.9).
    ///
    /// # Errors
    ///
    /// [`RateLimited`] when the caller passed it.
    pub async fn admit_request(&self, caller: &EdgeCaller) -> Result<RateStanding, RateLimited> {
        let (class, limits) = self.class_for(caller, None);
        let (remaining, reset) = self
            .hit_shared(
                &caller.limit_key,
                &class,
                Window::Minute,
                limits.requests_per_minute,
                Utc::now(),
            )
            .await?;
        Ok(RateStanding { class, limit: limits.requests_per_minute, remaining, reset })
    }

    /// Counts one capability call against the daily limit of the caller's class, or of
    /// `capability_class`, in the shared windows when the limits have a
    /// [`store`](Self::store) (spec 3.5 §7.9).
    ///
    /// # Errors
    ///
    /// [`RateLimited`] when the caller passed it.
    pub async fn admit_call(
        &self,
        caller: &EdgeCaller,
        capability_class: Option<&str>,
    ) -> Result<(), RateLimited> {
        let (class, limits) = self.class_for(caller, capability_class);
        self.hit_shared(&caller.limit_key, &class, Window::Day, limits.calls_per_day, Utc::now())
            .await
            .map(|_| ())
    }

    /// Counts one edge request against the caller's per-minute limit in the process's own
    /// windows; [`admit_request`](Self::admit_request) also reads a shared store.
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
    /// `capability_class` when the capability names one, in the process's own windows;
    /// [`admit_call`](Self::admit_call) also reads a shared store.
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

    struct Broken;

    #[async_trait]
    impl RateLimitStore for Broken {
        async fn hit(&self, _: &RateWindow, _: u32) -> StoreResult<Option<u32>> {
            Err(adk_awp::v2::store::StoreError::Backend("down".into()))
        }
        async fn purge_before(&self, _: i64) -> StoreResult<usize> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn shared_windows_hold_across_edges_and_a_failed_store_falls_back() {
        let class = RateClass { requests_per_minute: 1, calls_per_day: 1 };
        let windows = Arc::new(InMemoryRateLimitStore::default());
        let edge = |store: Arc<dyn RateLimitStore>| {
            EdgeLimits::default().tier_default(TrustTier::Public, class).store(store)
        };
        let (first, second) = (edge(windows.clone()), edge(windows.clone()));
        let caller = EdgeCaller::public("ip:203.0.113.9");
        assert!(first.admit_request(&caller).await.is_ok());
        assert!(second.admit_request(&caller).await.is_err(), "one window across both edges");
        assert!(second.admit_call(&caller, None).await.is_ok());
        assert!(first.admit_call(&caller, None).await.is_err());
        assert!(
            first.check_request(&caller).is_ok(),
            "the synchronous check counts in the process only"
        );

        let degraded = edge(Arc::new(Broken));
        assert!(degraded.admit_request(&caller).await.is_ok(), "the process counts instead");
        assert!(degraded.admit_request(&caller).await.is_err());
    }
}
