//! Batching for constantly updated sites (spec 3.5 §7.8.3 "Batching", "Hosts at speed", Q15,
//! Q16).
//!
//! Changes arriving within a window coalesce into one release. Each content kind has its own
//! window — 30 s by default, 10 s for news ([`ContentKind::Article`]), 60 s for shops
//! ([`ContentKind::Product`]), owner-adjustable within five seconds to five minutes. A change
//! whose [`Trigger`] the policy lists in `flush_now` — a correction, a takedown, an emergency
//! notice, an owner request — publishes at once with everything pending. When one item
//! changes several times in a window, the latest version is released and the earlier ones
//! are returned as superseded, for the content history only.
//!
//! [`check_host`] compares the batch windows with a host's [`HostLimits`]: a host without
//! atomic publication cannot serve a window under one minute.

use std::collections::BTreeMap;
use std::time::Duration;

use awp_types::{Atomicity, BatchPolicy, ContentId, ContentItem, ContentKind, HostLimits, Trigger};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SiteError};
use crate::pipeline::EntryPoint;

/// The shortest batch window a host without atomic publication may serve (spec 3.5 §7.8.3).
pub const NON_ATOMIC_MIN_WINDOW: Duration = Duration::from_secs(60);

/// One change to one item, on its way to a batch (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    /// The item in its new version.
    pub item: ContentItem,
    /// When the change arrived.
    pub at: DateTime<Utc>,
    /// Where it came from; only these author content (A-I24).
    pub entry: EntryPoint,
    /// The principal proposing it.
    pub proposer: String,
    /// Why it publishes at once, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    /// Model calls spent writing it: zero for a feed mapping, which never calls a model.
    #[serde(default)]
    pub model_calls: u32,
}

impl Update {
    /// The item's id.
    pub fn id(&self) -> &ContentId {
        &self.item.id
    }

    /// The bytes the item adds to a release, estimated by its JSON size.
    pub fn bytes(&self) -> u64 {
        serde_json::to_vec(&self.item).map_or(0, |b| b.len() as u64)
    }
}

/// Why a batch closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "trigger")]
pub enum FlushReason {
    /// A kind's window elapsed.
    Window,
    /// A `flush_now` change arrived.
    Trigger(Trigger),
    /// The next change would pass `max_items`.
    MaxItems,
    /// The next change would pass `max_bytes`.
    MaxBytes,
    /// The caller flushed everything pending, as at shutdown.
    Drain,
}

/// Changes that publish in one release (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    /// The batch number, from 1.
    pub id: u64,
    /// When its earliest change arrived.
    pub opened_at: DateTime<Utc>,
    /// When it closed.
    pub closed_at: DateTime<Utc>,
    /// Why it closed.
    pub reason: FlushReason,
    /// The latest version of each changed item, one per item, in arrival order.
    pub updates: Vec<Update>,
    /// Earlier versions replaced within the window: content history only, never released.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded: Vec<Update>,
}

impl Batch {
    /// The ids of the items the batch changes.
    pub fn ids(&self) -> Vec<ContentId> {
        self.updates.iter().map(|u| u.item.id.clone()).collect()
    }

    /// Model calls spent on the batch's updates, superseded ones included.
    pub fn model_calls(&self) -> u64 {
        self.updates.iter().chain(&self.superseded).map(|u| u64::from(u.model_calls)).sum()
    }
}

/// The batch policy of each content kind (spec 3.5 §7.8.3, Q15).
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_site::batch::BatchPolicies;
/// use awp_types::ContentKind;
///
/// let policies = BatchPolicies::default();
/// assert_eq!(policies.policy(ContentKind::Article).window, Duration::from_secs(10));
/// assert_eq!(policies.policy(ContentKind::Product).window, Duration::from_secs(60));
/// assert_eq!(policies.policy(ContentKind::Page).window, Duration::from_secs(30));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPolicies {
    /// The policy of kinds without their own.
    pub default: BatchPolicy,
    /// Policies per kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kinds: BTreeMap<ContentKind, BatchPolicy>,
}

impl Default for BatchPolicies {
    /// 30 s windows; news (articles) 10 s and shops (products) 60 s.
    fn default() -> Self {
        let with_window =
            |secs: u64| BatchPolicy { window: Duration::from_secs(secs), ..BatchPolicy::default() };
        Self {
            default: BatchPolicy::default(),
            kinds: BTreeMap::from([
                (ContentKind::Article, with_window(10)),
                (ContentKind::Product, with_window(60)),
            ]),
        }
    }
}

impl BatchPolicies {
    /// The policy of `kind`.
    pub fn policy(&self, kind: ContentKind) -> &BatchPolicy {
        self.kinds.get(&kind).unwrap_or(&self.default)
    }

    /// Sets `kind`'s policy.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] when the window is outside five seconds to five minutes
    /// or `max_items` is zero.
    pub fn set(&mut self, kind: ContentKind, policy: BatchPolicy) -> Result<()> {
        check_policy(&policy)?;
        self.kinds.insert(kind, policy);
        Ok(())
    }

    /// Checks every policy.
    ///
    /// # Errors
    ///
    /// As [`set`](Self::set).
    pub fn validate(&self) -> Result<()> {
        check_policy(&self.default)?;
        self.kinds.values().try_for_each(check_policy)
    }

    /// The shortest window of any kind.
    pub fn shortest_window(&self) -> Duration {
        self.kinds.values().map(|p| p.window).fold(self.default.window, Duration::min)
    }
}

fn check_policy(policy: &BatchPolicy) -> Result<()> {
    if !BatchPolicy::WINDOW_RANGE.contains(&policy.window) {
        return Err(SiteError::InvalidConfig(format!(
            "a batch window of {} s is outside 5 s to 300 s; choose a window in that range",
            policy.window.as_secs()
        )));
    }
    if policy.max_items == 0 {
        return Err(SiteError::InvalidConfig(
            "a batch policy needs max_items of at least 1".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Queue {
    opened_at: DateTime<Utc>,
    bytes: u64,
    /// Position of each item in `updates`.
    index: BTreeMap<ContentId, usize>,
    updates: Vec<Update>,
    superseded: Vec<Update>,
}

impl Queue {
    fn new(at: DateTime<Utc>) -> Self {
        Self {
            opened_at: at,
            bytes: 0,
            index: BTreeMap::new(),
            updates: Vec::new(),
            superseded: Vec::new(),
        }
    }
}

/// Coalesces updates into batches by window, trigger and size (spec 3.5 §7.8.3).
///
/// Time comes from the updates and from [`poll`](Self::poll), so a warped clock drives it.
///
/// # Example
///
/// ```rust
/// use adk_site::batch::{BatchPolicies, Batcher, Update};
/// use adk_site::pipeline::EntryPoint;
/// use awp_types::{ContentItem, ContentKind, ContentState};
/// use chrono::{Duration, TimeZone, Utc};
///
/// let t0 = Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap();
/// let update = |price: u64, at| Update {
///     item: ContentItem {
///         id: "panel".into(), kind: ContentKind::Product, slug: "panel".into(),
///         language: "en".parse().unwrap(),
///         fields: [("price".to_string(), price.into())].into(),
///         volatile: vec![], body: None, media: vec![], sources: vec![], schedule: None,
///         state: ContentState::Published,
///     },
///     at,
///     entry: EntryPoint::Feed { feed: "supplier".into() },
///     proposer: "feed:supplier".into(),
///     trigger: None,
///     model_calls: 0,
/// };
/// let mut batcher = Batcher::new(BatchPolicies::default());
/// assert!(batcher.push(update(100, t0)).is_empty());
/// assert!(batcher.push(update(105, t0 + Duration::seconds(20))).is_empty());
/// assert!(batcher.poll(t0 + Duration::seconds(59)).is_empty());
/// let batches = batcher.poll(t0 + Duration::seconds(60));
/// assert_eq!(batches.len(), 1);
/// assert_eq!(batches[0].updates[0].item.fields["price"], 105);
/// assert_eq!(batches[0].superseded.len(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct Batcher {
    policies: BatchPolicies,
    queues: BTreeMap<ContentKind, Queue>,
    next_id: u64,
}

impl Batcher {
    /// A batcher with `policies`.
    pub fn new(policies: BatchPolicies) -> Self {
        Self { policies, queues: BTreeMap::new(), next_id: 1 }
    }

    /// Continues numbering batches after `last`.
    pub fn starting_after(mut self, last: u64) -> Self {
        self.next_id = last + 1;
        self
    }

    /// The policies.
    pub fn policies(&self) -> &BatchPolicies {
        &self.policies
    }

    /// Updates waiting for their window.
    pub fn pending(&self) -> usize {
        self.queues.values().map(|q| q.updates.len()).sum()
    }

    /// The earliest time a window closes, if anything is pending.
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.queues.iter().map(|(kind, queue)| self.deadline(*kind, queue)).min()
    }

    fn deadline(&self, kind: ContentKind, queue: &Queue) -> DateTime<Utc> {
        let window = self.policies.policy(kind).window;
        queue.opened_at + chrono::Duration::from_std(window).unwrap_or_default()
    }

    /// Adds an update and returns the batches it closes: its kind's window when it elapsed
    /// before the update arrived, its kind's queue when the update would pass `max_items` or
    /// `max_bytes`, and everything pending when its trigger flushes.
    pub fn push(&mut self, update: Update) -> Vec<Batch> {
        let kind = update.item.kind;
        let policy = self.policies.policy(kind).clone();
        let at = update.at;
        let bytes = update.bytes();
        let mut closed = Vec::new();
        // A window that elapsed before this change closes on its own, polled or not.
        if self.queues.get(&kind).is_some_and(|queue| self.deadline(kind, queue) <= at)
            && let Some(queue) = self.queues.remove(&kind)
        {
            closed.push(self.close(vec![queue], at, FlushReason::Window));
        }
        if let Some(queue) = self.queues.get(&kind) {
            let new_item = !queue.index.contains_key(update.id());
            let reason = if new_item && queue.updates.len() >= policy.max_items {
                Some(FlushReason::MaxItems)
            } else if !queue.updates.is_empty() && queue.bytes + bytes > policy.max_bytes {
                Some(FlushReason::MaxBytes)
            } else {
                None
            };
            if let Some(reason) = reason
                && let Some(queue) = self.queues.remove(&kind)
            {
                closed.push(self.close(vec![queue], at, reason));
            }
        }
        let queue = self.queues.entry(kind).or_insert_with(|| Queue::new(at));
        queue.bytes += bytes;
        match queue.index.get(update.id()).copied() {
            Some(position) => {
                let earlier = std::mem::replace(&mut queue.updates[position], update);
                queue.superseded.push(earlier);
            }
            None => {
                queue.index.insert(update.item.id.clone(), queue.updates.len());
                queue.updates.push(update);
            }
        }
        let trigger = queue.updates.iter().filter_map(|u| u.trigger).find(|trigger| {
            self.policies.policy(kind).flush_now.contains(trigger)
                || policy.flush_now.contains(trigger)
        });
        if let Some(trigger) = trigger {
            let queues = std::mem::take(&mut self.queues).into_values().collect();
            closed.push(self.close(queues, at, FlushReason::Trigger(trigger)));
        }
        closed
    }

    /// Closes the windows that have elapsed at `now`, one batch per kind.
    pub fn poll(&mut self, now: DateTime<Utc>) -> Vec<Batch> {
        let due: Vec<ContentKind> = self
            .queues
            .iter()
            .filter(|(kind, queue)| self.deadline(**kind, queue) <= now)
            .map(|(kind, _)| *kind)
            .collect();
        let mut closed = Vec::new();
        for kind in due {
            if let Some(queue) = self.queues.remove(&kind) {
                closed.push(self.close(vec![queue], now, FlushReason::Window));
            }
        }
        closed
    }

    /// Closes everything pending into one batch, if anything is.
    pub fn drain(&mut self, now: DateTime<Utc>) -> Option<Batch> {
        if self.queues.is_empty() {
            return None;
        }
        let queues = std::mem::take(&mut self.queues).into_values().collect();
        Some(self.close(queues, now, FlushReason::Drain))
    }

    fn close(&mut self, queues: Vec<Queue>, now: DateTime<Utc>, reason: FlushReason) -> Batch {
        let id = self.next_id;
        self.next_id += 1;
        let opened_at = queues.iter().map(|q| q.opened_at).min().unwrap_or(now);
        let mut updates = Vec::new();
        let mut superseded = Vec::new();
        for queue in queues {
            updates.extend(queue.updates);
            superseded.extend(queue.superseded);
        }
        Batch { id, opened_at, closed_at: now, reason, updates, superseded }
    }
}

/// Whether a host suits a site's batch windows (spec 3.5 §7.8.3 "Hosts at speed", Q16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFit {
    /// Whether the host can serve the windows.
    pub fits: bool,
    /// What does not fit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    /// What onboarding proposes instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
}

impl HostFit {
    /// The fit as a result, for onboarding to refuse an unfit host.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] naming the problems and the proposal.
    pub fn require(self) -> Result<()> {
        if self.fits {
            return Ok(());
        }
        Err(SiteError::InvalidConfig(format!(
            "the host cannot serve these batch windows: {}. {}",
            self.problems.join("; "),
            self.proposal.unwrap_or_default()
        )))
    }
}

/// Compares the site's batch windows with a host's limits (spec 3.5 §7.8.3 "Hosts at
/// speed").
///
/// A host with [`Atomicity::None`] cannot serve a window under one minute; a host whose
/// `max_publishes_per_hour` is below one publish per shortest window cannot keep up. Either
/// way the proposal is object storage behind a CDN with a release prefix and a pointer.
///
/// # Example
///
/// ```rust
/// use adk_site::batch::{BatchPolicies, check_host};
/// use awp_types::{Atomicity, HostLimits};
///
/// let in_place = HostLimits { max_publishes_per_hour: None, delta_upload: true, atomic: Atomicity::None };
/// let fit = check_host(&in_place, &BatchPolicies::default());
/// assert!(!fit.fits, "news windows are 10 s");
/// assert!(fit.require().is_err());
/// ```
pub fn check_host(limits: &HostLimits, policies: &BatchPolicies) -> HostFit {
    let shortest = policies.shortest_window();
    let mut problems = Vec::new();
    if limits.atomic == Atomicity::None && shortest < NON_ATOMIC_MIN_WINDOW {
        problems.push(format!(
            "the host does not publish atomically, so batch windows must be a minute or more, and the shortest is {} s",
            shortest.as_secs()
        ));
    }
    if let Some(max) = limits.max_publishes_per_hour {
        let needed = 3_600 / shortest.as_secs().max(1);
        if needed > u64::from(max) {
            problems.push(format!(
                "a {} s window needs up to {needed} publishes an hour and the host accepts {max}",
                shortest.as_secs()
            ));
        }
    }
    let fits = problems.is_empty();
    HostFit {
        fits,
        problems,
        proposal: (!fits).then(|| {
            "Use object storage behind a CDN that publishes each release under its own prefix and switches a pointer last (Atomicity::PrefixPointer), or lengthen the windows.".to_string()
        }),
    }
}

#[cfg(test)]
mod tests {
    use awp_types::ContentState;
    use chrono::TimeZone;

    use super::*;

    fn update(id: &str, kind: ContentKind, version: u64, at: DateTime<Utc>) -> Update {
        Update {
            item: ContentItem {
                id: id.into(),
                kind,
                slug: id.into(),
                language: "en".parse().unwrap(),
                fields: BTreeMap::from([("v".to_string(), version.into())]),
                volatile: Vec::new(),
                body: None,
                media: Vec::new(),
                sources: Vec::new(),
                schedule: None,
                state: ContentState::Published,
            },
            at,
            entry: EntryPoint::Feed { feed: "f".into() },
            proposer: "feed:f".into(),
            trigger: None,
            model_calls: 0,
        }
    }

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap() + chrono::Duration::seconds(secs)
    }

    #[test]
    fn kinds_keep_their_own_windows() {
        let mut batcher = Batcher::new(BatchPolicies::default());
        batcher.push(update("a", ContentKind::Article, 1, t(0)));
        batcher.push(update("p", ContentKind::Product, 1, t(1)));
        assert_eq!(batcher.next_due(), Some(t(10)));
        let first = batcher.poll(t(10));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].ids(), vec![ContentId::from("a")]);
        assert!(batcher.poll(t(60)).is_empty());
        let second = batcher.poll(t(61));
        assert_eq!(second[0].ids(), vec![ContentId::from("p")]);
        assert_eq!(second[0].id, 2);
    }

    #[test]
    fn a_trigger_flushes_everything_pending_at_once() {
        let mut batcher = Batcher::new(BatchPolicies::default());
        batcher.push(update("p", ContentKind::Product, 1, t(0)));
        let mut correction = update("a", ContentKind::Article, 1, t(3));
        correction.trigger = Some(Trigger::Correction);
        let closed = batcher.push(correction);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].reason, FlushReason::Trigger(Trigger::Correction));
        assert_eq!(closed[0].updates.len(), 2);
        assert_eq!(batcher.pending(), 0);
    }

    #[test]
    fn max_items_and_bytes_split_a_window() {
        let mut policies = BatchPolicies::default();
        let policy = BatchPolicy {
            window: Duration::from_secs(60),
            max_items: 2,
            max_bytes: 1_000_000,
            flush_now: Vec::new(),
        };
        policies.set(ContentKind::Product, policy).unwrap();
        let mut batcher = Batcher::new(policies);
        assert!(batcher.push(update("a", ContentKind::Product, 1, t(0))).is_empty());
        assert!(batcher.push(update("b", ContentKind::Product, 1, t(1))).is_empty());
        assert!(batcher.push(update("a", ContentKind::Product, 2, t(2))).is_empty());
        let closed = batcher.push(update("c", ContentKind::Product, 1, t(3)));
        assert_eq!(closed[0].reason, FlushReason::MaxItems);
        assert_eq!(closed[0].updates.len(), 2);
        assert_eq!(batcher.pending(), 1);

        let mut small = BatchPolicies::default();
        let tiny = BatchPolicy { max_bytes: 10, ..BatchPolicy::default() };
        small.set(ContentKind::Page, tiny).unwrap();
        let mut batcher = Batcher::new(small);
        batcher.push(update("x", ContentKind::Page, 1, t(0)));
        let closed = batcher.push(update("y", ContentKind::Page, 1, t(1)));
        assert_eq!(closed[0].reason, FlushReason::MaxBytes);
    }

    #[test]
    fn windows_outside_the_range_are_refused() {
        let mut policies = BatchPolicies::default();
        let fast = BatchPolicy { window: Duration::from_secs(1), ..BatchPolicy::default() };
        assert!(policies.set(ContentKind::Post, fast).is_err());
        assert!(policies.validate().is_ok());
    }

    #[test]
    fn hosts_without_atomic_publication_need_minute_windows() {
        let none = HostLimits {
            max_publishes_per_hour: None,
            delta_upload: false,
            atomic: Atomicity::None,
        };
        let mut policies = BatchPolicies::default();
        assert!(!check_host(&none, &policies).fits);
        let minute = BatchPolicy { window: Duration::from_secs(60), ..BatchPolicy::default() };
        policies.default = minute.clone();
        policies.kinds = BTreeMap::from([(ContentKind::Product, minute)]);
        assert!(check_host(&none, &policies).fits);
        let limited = HostLimits {
            max_publishes_per_hour: Some(10),
            delta_upload: true,
            atomic: Atomicity::Deploy,
        };
        let fit = check_host(&limited, &policies);
        assert!(!fit.fits);
        assert!(fit.problems[0].contains("60 publishes"), "{fit:?}");
        let pointer = HostLimits {
            max_publishes_per_hour: None,
            delta_upload: true,
            atomic: Atomicity::PrefixPointer,
        };
        assert!(check_host(&pointer, &BatchPolicies::default()).fits);
    }
}
