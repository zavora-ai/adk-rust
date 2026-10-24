//! The review the maintenance tick makes of what may have drifted (spec 3.2 §7.4, §9.1
//! permission drift).
//!
//! At most once per partition and UTC day, the kernel lists what is older than the
//! partition's [`ProactivityPolicy::review_period`](crate::ProactivityPolicy::review_period):
//!
//! | List | What |
//! |---|---|
//! | `rules` | Standing rules with no `expires_at`, granted longer ago than the period, each with a renewal ask |
//! | `sleepers` | `Waiting` processes asleep or suspended for longer than the period |
//! | `frozen` | Processes a freeze parked longer ago than the period |
//! | `crossings` | Crossings out of the partition that recur, at least [`ReviewReport::CROSSING_THRESHOLD`] times in [`ReviewReport::CROSSING_WINDOW`] by source, target, resource and name (spec 3.2 §7.8), each with partition membership as the remedy |
//!
//! A report is published as a [`KernelEvent::Review`](crate::KernelEvent::Review) and written as
//! one `kernel.review` audit record — ids, ages, resource names and counts, no content. The
//! review never revokes, suspends or kills anything, and never waives a crossing.
//!
//! `adk-kernel` reads no harness type (§6 D-1), so it reads standing rules through a
//! [`StandingRuleSource`] the harness implements over its rule store.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::partition::{PartitionId, RecurringCrossing};

/// When a standing rule was granted and whether it expires, as the review reads it (spec
/// 3.2 §9.1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::RuleAge;
///
/// let rule = RuleAge::new("rule-1", "alice", chrono::Utc::now());
/// assert!(rule.expires_at.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuleAge {
    /// The rule's id.
    pub id: String,
    /// The user the rule belongs to.
    pub user: String,
    /// When it was granted.
    pub created_at: DateTime<Utc>,
    /// When it expires; `None` never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl RuleAge {
    /// A rule of `user` granted at `created_at` that never expires.
    pub fn new(id: impl Into<String>, user: impl Into<String>, created_at: DateTime<Utc>) -> Self {
        Self { id: id.into(), user: user.into(), created_at, expires_at: None }
    }

    /// Sets the expiry.
    #[must_use]
    pub fn with_expiry(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }
}

/// Reads the standing rules the review checks (spec 3.2 §9.1): the harness implements it
/// over its rule store and `Harness::kernel` registers it
/// ([`KernelBuilder::standing_rules`](crate::KernelBuilder::standing_rules)).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{RuleAge, StandingRuleSource};
/// use async_trait::async_trait;
///
/// struct Fixed(Vec<RuleAge>);
///
/// #[async_trait]
/// impl StandingRuleSource for Fixed {
///     async fn rules(&self, _app: &str, user: &str) -> adk_kernel::Result<Vec<RuleAge>> {
///         Ok(self.0.iter().filter(|rule| rule.user == user).cloned().collect())
///     }
/// }
/// ```
#[async_trait]
pub trait StandingRuleSource: Send + Sync {
    /// Every standing rule of `user` in `app`.
    ///
    /// # Errors
    ///
    /// Returns the rule store's error; the review then lists no rules.
    async fn rules(&self, app: &str, user: &str) -> Result<Vec<RuleAge>>;
}

/// A standing rule the review asks to renew (spec 3.2 §9.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct StaleRule {
    /// The rule's id.
    pub id: String,
    /// The user it belongs to.
    pub user: String,
    /// How long ago it was granted.
    #[serde(rename = "ageMs", with = "crate::serde_util::millis")]
    pub age: Duration,
}

/// A process the review reports (spec 3.2 §7.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgedProcess {
    /// The process.
    pub pid: String,
    /// How long it has waited.
    #[serde(rename = "ageMs", with = "crate::serde_util::millis")]
    pub age: Duration,
}

/// One partition's review (spec 3.2 §7.4, §9.1): what is older than its review period.
///
/// # Example
///
/// ```rust
/// use adk_kernel::ReviewReport;
///
/// let report: ReviewReport = serde_json::from_value(serde_json::json!({
///     "partition": "default", "reviewPeriodMs": 7_776_000_000u64, "at": "2026-10-23T09:00:00Z",
///     "rules": [{ "id": "rule-1", "user": "alice", "ageMs": 9_000_000_000u64 }],
///     "sleepers": [], "frozen": [],
///     "crossings": [{ "from": "default", "to": "work", "resource": "driver",
///                     "name": "calendar", "count": 9 }]
/// })).unwrap();
/// assert!(report.renewal_ask().unwrap().contains("rule-1"));
/// assert!(report.crossing_remedies()[0].contains("move calendar into default"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ReviewReport {
    /// The partition reviewed.
    pub partition: PartitionId,
    /// Its review period.
    #[serde(rename = "reviewPeriodMs", with = "crate::serde_util::millis")]
    pub review_period: Duration,
    /// When the review ran.
    pub at: DateTime<Utc>,
    /// Standing rules without an expiry granted longer ago than the period.
    #[serde(default)]
    pub rules: Vec<StaleRule>,
    /// `Waiting` processes asleep or suspended longer than the period.
    #[serde(default)]
    pub sleepers: Vec<AgedProcess>,
    /// Processes a freeze parked longer ago than the period.
    #[serde(default)]
    pub frozen: Vec<AgedProcess>,
    /// Crossings out of the partition that recur (spec 3.2 §7.8), most frequent first.
    #[serde(default)]
    pub crossings: Vec<RecurringCrossing>,
}

impl ReviewReport {
    /// How far back the review counts crossings.
    pub const CROSSING_WINDOW: Duration = Duration::from_secs(7 * 86_400);

    /// How many crossings of one resource in the window the review reports.
    pub const CROSSING_THRESHOLD: usize = 5;

    /// Whether nothing is older than the period and no crossing recurs.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
            && self.sleepers.is_empty()
            && self.frozen.is_empty()
            && self.crossings.is_empty()
    }

    /// One line per recurring crossing, with partition membership as the remedy.
    pub fn crossing_remedies(&self) -> Vec<String> {
        self.crossings.iter().map(RecurringCrossing::remedy).collect()
    }

    /// The renewal ask for the stale rules, `None` without any.
    pub fn renewal_ask(&self) -> Option<String> {
        if self.rules.is_empty() {
            return None;
        }
        let ids: Vec<&str> = self.rules.iter().map(|rule| rule.id.as_str()).collect();
        Some(format!(
            "{} standing rule(s) have no expiry and were granted more than {} day(s) ago: {}. \
             Renew each with an expiry, or revoke it.",
            ids.len(),
            self.review_period.as_secs() / 86_400,
            ids.join(", ")
        ))
    }
}
