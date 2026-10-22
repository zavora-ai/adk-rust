//! The consolidation tick's configuration (spec 3.2 §7.5, §13 Q1, Q12).
//!
//! The tick is a `Maintenance` process on the distiller's model that distils the sessions
//! the spec 3.1 distiller never reached and consolidates a partition's `Assumed` facts in
//! two passes. [`ConsolidationTick`] holds its admission gates, its spend caps and the
//! thresholds of both passes; [`ProactivityPolicy::consolidation`](crate::ProactivityPolicy::consolidation)
//! turns it on for a partition. It is on in the CLI's Ask and Auto partitions and off in
//! the library and on servers, so the policy's default is `None`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The consolidation tick of one partition (spec 3.2 §7.5, §13 Q1, Q12).
///
/// | Field | Default | Meaning |
/// |---|---|---|
/// | `min_new_sessions` | 3 | Distilled sessions since the last tick that admit one, or |
/// | `min_new_facts` | 25 | new facts since the last tick that admit one |
/// | `min_interval` | 6 h | Least time between two ticks |
/// | `debounce` | 30 min | Least idle time in the partition before a tick |
/// | `schedule` | `None` | A bare cron schedule, only as an explicit setting |
/// | `budget_share` | 0.10 | Most of the partition's daily proactive budget one day's ticks spend |
/// | `max_tokens_per_tick` | 50 000 | Hard token cap of one tick |
/// | `dedup_score` | 0.92 | Pass 1 collapses `Assumed` near-duplicates at or above this cosine |
/// | `expire_confidence` | 0.5 | Pass 1 expires unused `Assumed` facts under this confidence |
/// | `assumed_ttl` | 90 days | and older than this |
/// | `max_clusters_per_tick` | 20 | Most clusters pass 2 sends to the model in one tick |
/// | `max_loss_fraction` | 0.25 | Most of a scope's live facts one tick may change the status of |
///
/// A tick is admitted only when the partition has new material **and** `min_interval` has
/// passed **and** the partition has been idle for `debounce`, outside quiet hours and under
/// the daily budget. Durations serialize as whole milliseconds (`minIntervalMs`,
/// `debounceMs`, `assumedTtlMs`); a missing field takes its default.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::ConsolidationTick;
///
/// let tick = ConsolidationTick::default();
/// assert_eq!((tick.min_new_sessions, tick.min_new_facts), (3, 25));
/// assert_eq!(tick.min_interval, Duration::from_secs(6 * 60 * 60));
/// assert_eq!(tick.debounce, Duration::from_secs(30 * 60));
/// assert_eq!(tick.assumed_ttl, Duration::from_secs(90 * 24 * 60 * 60));
/// assert!(tick.schedule.is_none());
///
/// let nightly = ConsolidationTick::new().with_schedule("0 3 * * *").with_max_tokens_per_tick(20_000);
/// let json = serde_json::to_value(&nightly).unwrap();
/// assert_eq!(json["schedule"], "0 3 * * *");
/// assert_eq!(json["debounceMs"], 1_800_000);
/// let back: ConsolidationTick = serde_json::from_value(serde_json::json!({ "minNewFacts": 10 })).unwrap();
/// assert_eq!((back.min_new_facts, back.min_new_sessions), (10, 3));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct ConsolidationTick {
    /// Distilled sessions since the last tick that admit a tick (3).
    pub min_new_sessions: usize,
    /// New facts since the last tick that admit a tick (25).
    pub min_new_facts: usize,
    /// Least time since the last tick (6 h).
    #[serde(rename = "minIntervalMs", with = "crate::serde_util::millis")]
    pub min_interval: Duration,
    /// Least idle time in the partition before a tick (30 min).
    #[serde(rename = "debounceMs", with = "crate::serde_util::millis")]
    pub debounce: Duration,
    /// A bare cron schedule; only as an explicit setting, never the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// The share of the partition's daily proactive budget the ticks may spend (0.10).
    pub budget_share: f32,
    /// The hard token cap of one tick (50 000).
    pub max_tokens_per_tick: u64,
    /// Pass 1 marks the older of two `Assumed` facts whose cosine is at or above this
    /// `Superseded` by the newer one (0.92).
    pub dedup_score: f32,
    /// Pass 1 expires an unused `Assumed` fact whose confidence is under this (0.5).
    pub expire_confidence: f32,
    /// and whose age is over this (90 days).
    #[serde(rename = "assumedTtlMs", with = "crate::serde_util::millis")]
    pub assumed_ttl: Duration,
    /// The most clusters pass 2 sends to the model in one tick (20).
    pub max_clusters_per_tick: usize,
    /// The most of a scope's live facts one tick may change the status of (0.25); a result
    /// over it is refused whole.
    pub max_loss_fraction: f32,
}

impl ConsolidationTick {
    /// The default `max_tokens_per_tick`; the spec names the cap without a number.
    pub const DEFAULT_MAX_TOKENS_PER_TICK: u64 = 50_000;

    /// The tick with the CLI's defaults (spec 3.2 §7.5).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the distilled sessions that admit a tick.
    #[must_use]
    pub fn with_min_new_sessions(mut self, sessions: usize) -> Self {
        self.min_new_sessions = sessions;
        self
    }

    /// Sets the new facts that admit a tick.
    #[must_use]
    pub fn with_min_new_facts(mut self, facts: usize) -> Self {
        self.min_new_facts = facts;
        self
    }

    /// Sets the least time between two ticks.
    #[must_use]
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    /// Sets the least idle time in the partition before a tick.
    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Sets a bare cron schedule.
    #[must_use]
    pub fn with_schedule(mut self, schedule: impl Into<String>) -> Self {
        self.schedule = Some(schedule.into());
        self
    }

    /// Sets the share of the partition's daily proactive budget the ticks may spend.
    #[must_use]
    pub fn with_budget_share(mut self, share: f32) -> Self {
        self.budget_share = share;
        self
    }

    /// Sets the hard token cap of one tick.
    #[must_use]
    pub fn with_max_tokens_per_tick(mut self, tokens: u64) -> Self {
        self.max_tokens_per_tick = tokens;
        self
    }

    /// Sets pass 1's near-duplicate cosine threshold.
    #[must_use]
    pub fn with_dedup_score(mut self, score: f32) -> Self {
        self.dedup_score = score;
        self
    }

    /// Sets pass 1's expiry confidence threshold.
    #[must_use]
    pub fn with_expire_confidence(mut self, confidence: f32) -> Self {
        self.expire_confidence = confidence;
        self
    }

    /// Sets the age past which pass 1 expires an unused `Assumed` fact.
    #[must_use]
    pub fn with_assumed_ttl(mut self, ttl: Duration) -> Self {
        self.assumed_ttl = ttl;
        self
    }

    /// Sets the most clusters pass 2 sends to the model in one tick.
    #[must_use]
    pub fn with_max_clusters_per_tick(mut self, clusters: usize) -> Self {
        self.max_clusters_per_tick = clusters;
        self
    }

    /// Sets the most of a scope's live facts one tick may change the status of.
    #[must_use]
    pub fn with_max_loss_fraction(mut self, fraction: f32) -> Self {
        self.max_loss_fraction = fraction;
        self
    }
}

impl Default for ConsolidationTick {
    fn default() -> Self {
        Self {
            min_new_sessions: 3,
            min_new_facts: 25,
            min_interval: Duration::from_secs(6 * 60 * 60),
            debounce: Duration::from_secs(30 * 60),
            schedule: None,
            budget_share: 0.10,
            max_tokens_per_tick: Self::DEFAULT_MAX_TOKENS_PER_TICK,
            dedup_score: 0.92,
            expire_confidence: 0.5,
            assumed_ttl: Duration::from_secs(90 * 24 * 60 * 60),
            max_clusters_per_tick: 20,
            max_loss_fraction: 0.25,
        }
    }
}
