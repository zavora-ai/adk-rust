//! Constantly updated sites: the engine that sorts updates into lanes, bounds feeds, batches
//! releases and holds only exceptions (spec 3.5 §7.8.3, A-I24–A-I27, §8.1 `site revert`,
//! `site batches`, `site feeds`).
//!
//! ```text
//! ContentSource ──records──▶ FeedMapping (no model) ──▶ bounds ──held whole──▶ P2 alarm, owner decides
//!                                                          │
//!                                   live values ◀──────────┤
//!                                  (LiveStore, never released)
//!                                                          ▼
//! owner, editor, agents ──Update──▶ Batcher (window per kind, flush triggers)
//!                                                          ▼
//!                       exceptions held alone ◀── rules (price rule, new items)
//!                                                          ▼
//!                  PublishingPipeline::publish_batch: fast tier per item, A-I1 refuses whole,
//!                  one release (delta or snapshot), one log entry, one checkpoint,
//!                  one catalog.changed
//! ```
//!
//! [`UpdateEngine::tick`] closes elapsed windows and runs the scheduled full checks every six
//! hours or 500 releases. What the engine holds for the owner lives in a [`HeldStore`]
//! ([`FileHeldStore`] keeps it across restarts). [`UpdateEngine::batches`] and [`UpdateEngine::feeds`] are the
//! library side of `adk-rust site batches` and `site feeds`; [`UpdateEngine::revert`] of
//! `site revert <item> [--to <seq>]`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecorder};
use adk_core::{ActingContext, Clock};
use async_trait::async_trait;
use awp_types::{ContentId, ContentItem, FeedBounds, PriceRule, ReleaseSource, Trigger, Urgency};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::batch::{Batch, BatchPolicies, Batcher, FlushReason, Update, check_host};
use crate::content::{ContentHistory, ContentStore, HistoryEntry};
use crate::error::{Result, SiteError};
use crate::feed::bounds::{self, BoundsReport, Delivery, price_within_rule};
use crate::feed::{ContentSource, FeedChanges, FeedMapping, FeedRecord};
use crate::gate::{Finding, GateReport};
use crate::live::{LiveStore, LiveValue};
use crate::pipeline::{EntryPoint, Published, PublishingPipeline};
use crate::stream::{StreamLane, StreamStatus};

mod held;

pub use held::{FileHeldStore, HeldState, HeldStore, InMemoryHeldStore, held_store_conformance};

/// Batch records kept for `site batches`.
const RECORDS_KEPT: usize = 1_000;

/// An alarm for the owner (spec 3.5 §7.21; `website.alarm.raised`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteAlarm {
    /// How urgent it is.
    pub urgency: Urgency,
    /// A stable code, such as `site.feed.held`.
    pub code: String,
    /// One line for the owner.
    pub title: String,
    /// What it is about, such as a held delivery's change summary.
    pub detail: Value,
    /// When it was raised.
    pub at: DateTime<Utc>,
}

/// Where alarms go: the product's alarm routing (spec 3.5 §7.21).
#[async_trait]
pub trait AlarmSink: Send + Sync {
    /// Raises an alarm.
    ///
    /// # Errors
    ///
    /// Returns an error when the alarm cannot be queued.
    async fn raise(&self, alarm: &SiteAlarm) -> Result<()>;
}

/// Alarms kept in memory, for tests and for a product that polls them.
#[derive(Debug, Default)]
pub struct InMemoryAlarms(Mutex<Vec<SiteAlarm>>);

impl InMemoryAlarms {
    /// Every alarm raised so far.
    pub async fn all(&self) -> Vec<SiteAlarm> {
        self.0.lock().await.clone()
    }
}

#[async_trait]
impl AlarmSink for InMemoryAlarms {
    async fn raise(&self, alarm: &SiteAlarm) -> Result<()> {
        self.0.lock().await.push(alarm.clone());
        Ok(())
    }
}

/// When the launch gate's full tier runs again (spec 3.5 §7.8.3 "Tiered checks"): every
/// six hours or every 500 releases by default, whichever comes first.
///
/// # Example
///
/// ```rust
/// use adk_site::updates::FullCheckSchedule;
/// use chrono::{Duration, Utc};
///
/// let start = Utc::now();
/// let mut schedule = FullCheckSchedule::default();
/// schedule.ran(start, 0);
/// assert!(!schedule.due(start + Duration::hours(5), 499));
/// assert!(schedule.due(start + Duration::hours(5), 500));
/// assert!(schedule.due(start + Duration::hours(6), 1));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullCheckSchedule {
    /// The longest time between full checks.
    #[serde(with = "secs")]
    pub every: Duration,
    /// The most releases between full checks.
    pub every_releases: u64,
    /// When they last ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<DateTime<Utc>>,
    /// The release they last ran on.
    #[serde(default)]
    pub last_seq: u64,
}

mod secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}

impl Default for FullCheckSchedule {
    fn default() -> Self {
        Self {
            every: Duration::from_secs(6 * 3_600),
            every_releases: 500,
            last_at: None,
            last_seq: 0,
        }
    }
}

impl FullCheckSchedule {
    /// Whether the full checks are due at `now` with release `seq` live.
    pub fn due(&self, now: DateTime<Utc>, seq: u64) -> bool {
        let Some(last) = self.last_at else { return true };
        now - last >= chrono::Duration::from_std(self.every).unwrap_or_default()
            || seq.saturating_sub(self.last_seq) >= self.every_releases
    }

    /// Records that the full checks ran.
    pub fn ran(&mut self, now: DateTime<Utc>, seq: u64) {
        self.last_at = Some(now);
        self.last_seq = seq;
    }
}

/// What the weekly review reports on updates (spec 3.5 §7.8.3 "Model-free updates").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostRates {
    /// The cost of one model call, in millionths of the site's currency unit.
    pub per_model_call_micros: u64,
    /// The cost of one release on the host, in millionths of the site's currency unit.
    pub per_release_micros: u64,
}

/// Update counts and costs for the weekly review (spec 3.5 §7.8.3, §7.21).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReview {
    /// Item updates: live values set plus item versions batched.
    pub updates: u64,
    /// Live item updates.
    pub live_updates: u64,
    /// Item versions batched for release.
    pub release_updates: u64,
    /// Updates whose writing called a model.
    pub updates_with_model: u64,
    /// Model calls spent on updates.
    pub model_calls: u64,
    /// `updates_with_model / updates`.
    pub model_share: f64,
    /// Feed deliveries.
    pub deliveries: u64,
    /// Deliveries held whole by their bounds.
    pub held_deliveries: u64,
    /// Items held alone: exceptions and fast-tier failures.
    pub held_items: u64,
    /// Releases published.
    pub releases: u64,
    /// Scheduled full checks run.
    pub full_checks: u64,
    /// Cost of the model calls and releases, in millionths of the currency unit.
    pub cost_micros: u64,
    /// Cost per thousand updates, in millionths of the currency unit.
    pub cost_per_thousand_updates_micros: f64,
}

/// Why an item waits on its own (spec 3.5 §7.8.3 "Approvals on exceptions only",
/// "Tiered checks").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum HoldReason {
    /// The price moves more than the price rule allows (A-I25).
    PriceRule {
        /// The largest change the rule allows, in percent.
        allowed_pct: f32,
    },
    /// A new item waits for its approval chain.
    NewItem,
    /// The fast tier failed on it.
    FastTier {
        /// The blocking findings.
        findings: Vec<Finding>,
    },
}

/// An item held alone; the rest of its batch published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldItem {
    /// The version waiting.
    pub update: Update,
    /// Why it waits.
    pub reason: HoldReason,
    /// The batch it came in.
    pub batch: u64,
    /// When it was held.
    pub held_at: DateTime<Utc>,
}

/// A delivery held whole by its bounds (spec 3.5 A-I27).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldDelivery {
    /// The delivery number.
    pub delivery: u64,
    /// The feed.
    pub feed: String,
    /// When it arrived.
    pub received_at: DateTime<Utc>,
    /// The bounds it broke and what it would change.
    pub report: BoundsReport,
    /// What it would change, applied only if the owner releases it.
    pub changes: FeedChanges,
}

/// The owner's decision on something held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Publish it.
    Release,
    /// Drop it.
    Discard,
}

/// What one delivery did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryOutcome {
    /// The delivery number.
    pub delivery: u64,
    /// Whether its bounds held it whole.
    pub held: bool,
    /// Live item updates applied.
    pub live: usize,
    /// Item versions queued for release.
    pub queued: usize,
    /// Keys the site has no item for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
    /// Batches it closed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batches: Vec<BatchRecord>,
}

/// One batch for `site batches` (spec 3.5 §8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRecord {
    /// The batch number.
    pub batch: u64,
    /// When its earliest change arrived.
    pub opened_at: DateTime<Utc>,
    /// When it closed.
    pub closed_at: DateTime<Utc>,
    /// Why it closed.
    pub reason: FlushReason,
    /// The release it published, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_seq: Option<u64>,
    /// When the release went live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_at: Option<DateTime<Utc>>,
    /// The items it published.
    pub published: Vec<ContentId>,
    /// Earlier versions replaced within the window.
    pub superseded: usize,
    /// The items it held alone, with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<(ContentId, HoldReason)>,
    /// Why the release was refused whole (A-I1), when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// Pages of changed ids after the first `catalog.changed` page.
    pub catalog_pages: usize,
    /// Approval-to-live of each published item, in milliseconds: from the change's arrival
    /// (its approval, for a change within the rules) to its release going live.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latencies_ms: Vec<i64>,
    /// Model calls spent on its updates.
    pub model_calls: u64,
}

/// One feed for `site feeds` (spec 3.5 §8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedStatus {
    /// The mapping.
    pub mapping: FeedMapping,
    /// Its bounds.
    pub bounds: FeedBounds,
    /// Deliveries so far.
    pub deliveries: u64,
    /// Records so far.
    pub records: u64,
    /// Records in the last minute.
    pub records_last_minute: u64,
    /// When it last delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<DateTime<Utc>>,
    /// Deliveries held whole.
    pub held: u64,
    /// Model calls its mapping made: always zero.
    pub model_calls: u64,
}

/// An item changed since a rollback point, which the Steward proposes replaying (spec 3.5
/// §7.8.3 "Undo per item").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayCandidate {
    /// The item.
    pub id: ContentId,
    /// The release its latest version went live in.
    pub release_seq: u64,
    /// When that version was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    /// Who saved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The version in the content history.
    pub version: u64,
}

#[derive(Debug)]
struct FeedState {
    mapping: FeedMapping,
    bounds: FeedBounds,
    deliveries: u64,
    records: u64,
    recent: VecDeque<(DateTime<Utc>, u64)>,
    last_delivery: Option<DateTime<Utc>>,
    held: u64,
}

impl FeedState {
    fn records_last_minute(&self, now: DateTime<Utc>) -> u64 {
        let since = now - chrono::Duration::minutes(1);
        self.recent.iter().filter(|(at, _)| *at > since).map(|(_, n)| n).sum()
    }
}

#[derive(Debug)]
struct Inner {
    batcher: Batcher,
    feeds: BTreeMap<String, FeedState>,
    held_deliveries: BTreeMap<u64, HeldDelivery>,
    held_items: BTreeMap<ContentId, HeldItem>,
    records: VecDeque<BatchRecord>,
    review: UpdateReview,
    schedule: FullCheckSchedule,
    next_delivery: u64,
    /// Whether the held state was read from the store.
    restored: bool,
}

impl Inner {
    fn held_state(&self) -> HeldState {
        HeldState {
            items: self.held_items.values().cloned().collect(),
            deliveries: self.held_deliveries.values().cloned().collect(),
            next_delivery: self.next_delivery,
        }
    }
}

/// Builds an [`UpdateEngine`].
pub struct UpdateEngineBuilder {
    pipeline: Arc<PublishingPipeline>,
    live: Arc<dyn LiveStore>,
    policies: BatchPolicies,
    price_rule: PriceRule,
    approve_new_items: bool,
    alarms: Option<Arc<dyn AlarmSink>>,
    audit: Option<(Arc<dyn AuditRecorder>, ActingContext)>,
    costs: CostRates,
    schedule: FullCheckSchedule,
    streams: Option<Arc<StreamLane>>,
    held: Option<Arc<dyn HeldStore>>,
}

impl UpdateEngineBuilder {
    /// Keeps held items and held deliveries in `store`, so they survive a restart; in memory
    /// by default.
    pub fn held_store(mut self, store: Arc<dyn HeldStore>) -> Self {
        self.held = Some(store);
        self
    }

    /// The stream lane: live blogs, tickers and emergency updates, folded into releases
    /// (spec 3.5 §7.8.3).
    pub fn streams(mut self, streams: Arc<StreamLane>) -> Self {
        self.streams = Some(streams);
        self
    }

    /// The batch policies; default 30 s, news 10 s, shops 60 s.
    pub fn policies(mut self, policies: BatchPolicies) -> Self {
        self.policies = policies;
        self
    }

    /// The charter's price rule for changes that do not come from a feed (A-I25).
    pub fn price_rule(mut self, rule: PriceRule) -> Self {
        self.price_rule = rule;
        self
    }

    /// Whether new items wait for their approval chain; default `true`.
    pub fn approve_new_items(mut self, approve: bool) -> Self {
        self.approve_new_items = approve;
        self
    }

    /// Where alarms go.
    pub fn alarms(mut self, alarms: Arc<dyn AlarmSink>) -> Self {
        self.alarms = Some(alarms);
        self
    }

    /// Records `site.batch.*`, `site.content.reverted` and `website.alarm.raised` as
    /// `acting`.
    pub fn audit(mut self, recorder: Arc<dyn AuditRecorder>, acting: ActingContext) -> Self {
        self.audit = Some((recorder, acting));
        self
    }

    /// The rates the weekly review prices updates with.
    pub fn costs(mut self, costs: CostRates) -> Self {
        self.costs = costs;
        self
    }

    /// When the full checks run.
    pub fn full_checks(mut self, schedule: FullCheckSchedule) -> Self {
        self.schedule = schedule;
        self
    }

    /// The engine.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] when a batch window is out of range or the pipeline's
    /// host cannot serve the windows (spec 3.5 §7.8.3 "Hosts at speed").
    pub fn build(self) -> Result<UpdateEngine> {
        self.policies.validate()?;
        check_host(&self.pipeline.site_host().limits(), &self.policies).require()?;
        let clock = self.pipeline.clock();
        let mut schedule = self.schedule;
        if schedule.last_at.is_none() {
            schedule.last_at = Some(clock.now());
        }
        Ok(UpdateEngine {
            store: self.pipeline.content_store(),
            history: self.pipeline.content_history(),
            pipeline: self.pipeline,
            live: self.live,
            price_rule: self.price_rule,
            approve_new_items: self.approve_new_items,
            alarms: self.alarms,
            audit: self.audit,
            costs: self.costs,
            streams: self.streams,
            held: self.held.unwrap_or_else(|| Arc::new(InMemoryHeldStore::default())),
            clock,
            inner: Mutex::new(Inner {
                batcher: Batcher::new(self.policies),
                feeds: BTreeMap::new(),
                held_deliveries: BTreeMap::new(),
                held_items: BTreeMap::new(),
                records: VecDeque::new(),
                review: UpdateReview::default(),
                schedule,
                next_delivery: 1,
                restored: false,
            }),
        })
    }
}

/// The engine of a constantly updated site (spec 3.5 §7.8.3).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::feed::{FeedMapping, FieldMapping, FieldType, FeedRecord};
/// use adk_site::live::InMemoryLiveStore;
/// use adk_site::updates::UpdateEngine;
/// use awp_types::{ContentKind, FeedBounds, PriceRule, Rate};
///
/// # async fn demo(pipeline: Arc<adk_site::pipeline::PublishingPipeline>) -> adk_site::Result<()> {
/// let engine = UpdateEngine::builder(pipeline, Arc::new(InMemoryLiveStore::default())).build()?;
/// let mapping = FeedMapping::new("supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
///     .field("price", FieldMapping::new("price", FieldType::Number))
///     .field("stock", FieldMapping::new("qty", FieldType::Integer));
/// let bounds = FeedBounds {
///     max_change_fraction: FeedBounds::DEFAULT_MAX_CHANGE_FRACTION,
///     price_rule: PriceRule::default(),
///     field_bounds: Default::default(),
///     max_rate: Rate { per_minute: 10_000 },
/// };
/// engine.add_feed(mapping, bounds).await?;
/// let outcome = engine.ingest("supplier", vec![FeedRecord::new([("sku", "A1".into()), ("qty", 3.into())])]).await?;
/// assert_eq!(outcome.queued, 0, "a stock change never makes a release");
/// engine.tick().await?;
/// # Ok(())
/// # }
/// ```
pub struct UpdateEngine {
    pipeline: Arc<PublishingPipeline>,
    store: Arc<dyn ContentStore>,
    history: Option<Arc<dyn ContentHistory>>,
    live: Arc<dyn LiveStore>,
    price_rule: PriceRule,
    approve_new_items: bool,
    alarms: Option<Arc<dyn AlarmSink>>,
    audit: Option<(Arc<dyn AuditRecorder>, ActingContext)>,
    costs: CostRates,
    streams: Option<Arc<StreamLane>>,
    held: Arc<dyn HeldStore>,
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for UpdateEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateEngine").finish_non_exhaustive()
    }
}

fn percentile(values: &mut [i64], p: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    Some(values[rank.clamp(1, values.len()) - 1])
}

impl UpdateEngine {
    /// Starts an engine over `pipeline`, writing live values to `live`.
    pub fn builder(
        pipeline: Arc<PublishingPipeline>,
        live: Arc<dyn LiveStore>,
    ) -> UpdateEngineBuilder {
        UpdateEngineBuilder {
            pipeline,
            live,
            policies: BatchPolicies::default(),
            price_rule: PriceRule::default(),
            approve_new_items: true,
            alarms: None,
            audit: None,
            costs: CostRates::default(),
            schedule: FullCheckSchedule::default(),
            streams: None,
            held: None,
        }
    }

    /// The engine's state, with what it holds read from its [`HeldStore`] on first use.
    async fn lock_held(&self) -> Result<tokio::sync::MutexGuard<'_, Inner>> {
        let mut inner = self.inner.lock().await;
        if !inner.restored {
            let state = self.held.load().await?;
            let restored = (state.items.len(), state.deliveries.len());
            for item in state.items {
                inner.held_items.entry(item.update.id().clone()).or_insert(item);
            }
            let mut next = inner.next_delivery.max(state.next_delivery);
            for delivery in state.deliveries {
                next = next.max(delivery.delivery + 1);
                inner.held_deliveries.entry(delivery.delivery).or_insert(delivery);
            }
            inner.next_delivery = next;
            inner.restored = true;
            if restored != (0, 0) {
                tracing::info!(
                    site.held.items = restored.0,
                    site.held.deliveries = restored.1,
                    "held updates restored"
                );
            }
        }
        Ok(inner)
    }

    /// Writes what the engine holds to its [`HeldStore`].
    async fn save_held(&self, inner: &Inner) -> Result<()> {
        self.held.save(&inner.held_state()).await
    }

    /// The pipeline releases go through.
    pub fn pipeline(&self) -> &Arc<PublishingPipeline> {
        &self.pipeline
    }

    /// Adds a feed the owner configured (A-I24).
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] for a mapping that fills a stream field, maps nothing,
    /// or reuses a feed name; or bounds with a change fraction outside (0, 1].
    pub async fn add_feed(&self, mapping: FeedMapping, bounds: FeedBounds) -> Result<()> {
        mapping.validate(self.pipeline.site_config())?;
        if !(bounds.max_change_fraction > 0.0 && bounds.max_change_fraction <= 1.0) {
            return Err(SiteError::InvalidConfig(format!(
                "feed '{}' has a max change fraction of {}; use a share above 0 and at most 1",
                mapping.feed, bounds.max_change_fraction
            )));
        }
        let mut inner = self.inner.lock().await;
        if inner.feeds.contains_key(&mapping.feed) {
            return Err(SiteError::InvalidConfig(format!(
                "a feed named '{}' is already configured",
                mapping.feed
            )));
        }
        inner.feeds.insert(
            mapping.feed.clone(),
            FeedState {
                mapping,
                bounds,
                deliveries: 0,
                records: 0,
                recent: VecDeque::new(),
                last_delivery: None,
                held: 0,
            },
        );
        Ok(())
    }

    /// Pulls one delivery from `source` and ingests it under the source's name.
    ///
    /// # Errors
    ///
    /// As [`ingest`](Self::ingest); errors from the source.
    pub async fn pull(&self, source: &dyn ContentSource) -> Result<DeliveryOutcome> {
        let records = source.pull().await?;
        self.ingest(source.name(), records).await
    }

    /// Ingests one delivery of `feed`: maps it without a model, checks its bounds, writes its
    /// live values and queues its item versions; a delivery that breaks its bounds is held
    /// whole with a P2 alarm (A-I27).
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an unknown feed; store, alarm and publishing errors.
    pub async fn ingest(&self, feed: &str, records: Vec<FeedRecord>) -> Result<DeliveryOutcome> {
        let now = self.clock.now();
        let (mapping, bounds_config, last_minute, delivery) = {
            let mut inner = self.lock_held().await?;
            let delivery = inner.next_delivery;
            inner.next_delivery += 1;
            let state = inner
                .feeds
                .get_mut(feed)
                .ok_or_else(|| SiteError::NotFound(format!("feed '{feed}'")))?;
            state.deliveries += 1;
            state.records += records.len() as u64;
            state.last_delivery = Some(now);
            state.recent.push_back((now, records.len() as u64));
            while state
                .recent
                .front()
                .is_some_and(|(at, _)| now - *at > chrono::Duration::minutes(1))
            {
                state.recent.pop_front();
            }
            let last_minute = state.records_last_minute(now);
            inner.review.deliveries += 1;
            (
                inner.feeds[feed].mapping.clone(),
                inner.feeds[feed].bounds.clone(),
                last_minute,
                delivery,
            )
        };
        let mut mapped = Vec::new();
        let mut problems = Vec::new();
        for record in &records {
            match mapping.map(record, now) {
                Ok(record) => mapped.push(record),
                Err(problem) => problems.push(problem),
            }
        }
        let current: BTreeMap<ContentId, ContentItem> =
            self.store.list().await?.into_iter().map(|item| (item.id.clone(), item)).collect();
        let collection = current.values().filter(|item| item.kind == mapping.kind).count();
        let changes = mapping.changes(self.pipeline.site_config(), &mapped, &current, now);
        let report = bounds::check(
            &bounds_config,
            &Delivery {
                records: records.len(),
                mapped: &mapped,
                problems: &problems,
                changes: &changes,
                collection,
                records_last_minute: last_minute,
                now,
            },
        );
        if report.held() {
            let held = HeldDelivery {
                delivery,
                feed: feed.to_string(),
                received_at: now,
                report: report.clone(),
                changes,
            };
            {
                let mut inner = self.lock_held().await?;
                inner.review.held_deliveries += 1;
                if let Some(state) = inner.feeds.get_mut(feed) {
                    state.held += 1;
                }
                inner.held_deliveries.insert(delivery, held);
                self.save_held(&inner).await?;
            }
            tracing::warn!(
                feed,
                delivery,
                breaks = report.breaks.len(),
                "site.batch.held: feed bounds broken (A-I27)"
            );
            let detail = serde_json::json!({
                "feed": feed,
                "delivery": delivery,
                "breaks": report.breaks,
                "summary": report.summary,
            });
            self.record("site.batch.held", detail.clone()).await?;
            self.alarm(
                Urgency::P2,
                "site.feed.held",
                format!(
                    "Feed '{feed}' delivery {delivery} broke its bounds and is held: it would change {} of {} items",
                    report.summary.items_changed, report.summary.collection
                ),
                detail,
            )
            .await?;
            return Ok(DeliveryOutcome { delivery, held: true, ..DeliveryOutcome::default() });
        }
        let unknown = changes.unknown.clone();
        let mut outcome = self.apply(feed, changes, now).await?;
        outcome.delivery = delivery;
        outcome.unknown = unknown;
        Ok(outcome)
    }

    /// Writes a delivery's live values and queues its item versions.
    async fn apply(
        &self,
        feed: &str,
        changes: FeedChanges,
        now: DateTime<Utc>,
    ) -> Result<DeliveryOutcome> {
        for change in &changes.live {
            let values = change
                .fields
                .iter()
                .map(|(field, value)| {
                    let live =
                        LiveValue { value: value.clone(), at: now, feed: Some(feed.to_string()) };
                    (field.clone(), live)
                })
                .collect();
            self.live.set(&change.id, values).await?;
        }
        let queued = changes.release.len();
        let updates: Vec<Update> = changes
            .release
            .into_iter()
            .map(|change| Update {
                item: change.after,
                at: now,
                entry: EntryPoint::Feed { feed: feed.to_string() },
                proposer: format!("feed:{feed}"),
                trigger: None,
                model_calls: 0,
            })
            .collect();
        {
            let mut inner = self.inner.lock().await;
            inner.review.live_updates += changes.live.len() as u64;
            inner.review.updates += changes.live.len() as u64;
        }
        let batches = self.queue(updates).await?;
        Ok(DeliveryOutcome {
            delivery: 0,
            held: false,
            live: changes.live.len(),
            queued,
            unknown: Vec::new(),
            batches,
        })
    }

    /// Submits a change from an entry point that authors content (A-I24): an owner, an
    /// editor, a committer, an agent, a schedule or a stream fold. A change whose trigger the
    /// policy lists publishes at once with everything pending.
    ///
    /// # Errors
    ///
    /// Store, alarm and publishing errors.
    pub async fn submit(&self, update: Update) -> Result<Vec<BatchRecord>> {
        self.queue(vec![update]).await
    }

    async fn queue(&self, updates: Vec<Update>) -> Result<Vec<BatchRecord>> {
        let closed = {
            let mut inner = self.inner.lock().await;
            let mut closed = Vec::new();
            for update in updates {
                inner.review.updates += 1;
                inner.review.release_updates += 1;
                inner.review.model_calls += u64::from(update.model_calls);
                if update.model_calls > 0 {
                    inner.review.updates_with_model += 1;
                }
                closed.extend(inner.batcher.push(update));
            }
            closed
        };
        self.publish_all(closed).await
    }

    /// Folds the streams that are due, closes the windows that have elapsed, publishes
    /// them, and runs the full checks when they are due.
    ///
    /// # Errors
    ///
    /// Store, alarm, gate, signing and publishing errors.
    pub async fn tick(&self) -> Result<Vec<BatchRecord>> {
        let mut records = self.fold_streams().await?;
        let now = self.clock.now();
        let closed = self.inner.lock().await.batcher.poll(now);
        records.extend(self.publish_all(closed).await?);
        self.full_checks_if_due().await?;
        Ok(records)
    }

    fn lane(&self) -> Result<&Arc<StreamLane>> {
        self.streams
            .as_ref()
            .ok_or_else(|| SiteError::Pipeline("the engine has no stream lane".into()))
    }

    /// Opens a stream on a published story's stream field (spec 3.5 §7.8.3 "Stream").
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an unknown story; [`SiteError::InvalidConfig`] when
    /// `field` is not a stream field of the story's kind; as [`StreamLane::open`].
    pub async fn open_stream(&self, item: &ContentId, field: &str) -> Result<()> {
        let story =
            self.store.get(item).await?.ok_or_else(|| SiteError::NotFound(item.0.clone()))?;
        let config = self.pipeline.site_config();
        if !crate::lanes::stream_fields(config, story.kind).iter().any(|f| f == field) {
            return Err(SiteError::InvalidConfig(format!(
                "'{field}' is not a stream field of {} items; declare it under [lanes.{}] stream",
                crate::content::kind_name(story.kind),
                crate::content::kind_name(story.kind)
            )));
        }
        self.lane()?.open(&story, field, self.clock.now()).await
    }

    /// Appends a reporter's entry to an open stream; the edge serves it at once.
    ///
    /// # Errors
    ///
    /// As [`StreamLane::append`].
    pub async fn append_entry(
        &self,
        item: &ContentId,
        markdown: &str,
        corrects: Option<u64>,
    ) -> Result<awp_types::StreamEntry> {
        self.lane()?.append(item, markdown, corrects, self.clock.now()).await
    }

    /// The entries of a stream after number `after`: the edge feed the page polls.
    ///
    /// # Errors
    ///
    /// As [`StreamLane::since`].
    pub async fn stream_since(
        &self,
        item: &ContentId,
        after: u64,
    ) -> Result<Vec<awp_types::StreamEntry>> {
        self.lane()?.since(item, after).await
    }

    /// Closes a stream; its last entries fold at the next [`tick`](Self::tick).
    ///
    /// # Errors
    ///
    /// As [`StreamLane::close`].
    pub async fn close_stream(&self, item: &ContentId) -> Result<()> {
        self.lane()?.close(item, self.clock.now()).await
    }

    /// The streams and their folds.
    pub async fn streams(&self) -> Vec<StreamStatus> {
        match &self.streams {
            Some(lane) => lane.status().await,
            None => Vec::new(),
        }
    }

    /// Folds every due stream into its story and queues the folded story for release.
    async fn fold_streams(&self) -> Result<Vec<BatchRecord>> {
        let Some(lane) = &self.streams else { return Ok(Vec::new()) };
        let now = self.clock.now();
        let mut updates = Vec::new();
        for id in lane.due(now).await {
            let Some(story) = self.store.get(&id).await? else { continue };
            if let Some(folded) = lane.fold(&story, now).await? {
                let entries =
                    lane.status().await.into_iter().find(|s| s.item == id).map_or(0, |s| s.entries);
                self.record(
                    "site.stream.folded",
                    serde_json::json!({ "item": id, "entries": entries }),
                )
                .await?;
                updates.push(Update {
                    item: folded,
                    at: now,
                    entry: EntryPoint::Schedule,
                    proposer: format!("stream:{id}"),
                    trigger: None,
                    model_calls: 0,
                });
            }
        }
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        self.queue(updates).await
    }

    /// Publishes everything pending now, as at shutdown.
    ///
    /// # Errors
    ///
    /// As [`tick`](Self::tick).
    pub async fn drain(&self) -> Result<Vec<BatchRecord>> {
        let now = self.clock.now();
        let closed = self.inner.lock().await.batcher.drain(now);
        self.publish_all(closed.into_iter().collect()).await
    }

    /// When the next window closes, for a scheduler's wake.
    pub async fn next_due(&self) -> Option<DateTime<Utc>> {
        self.inner.lock().await.batcher.next_due()
    }

    async fn publish_all(&self, batches: Vec<Batch>) -> Result<Vec<BatchRecord>> {
        let mut records = Vec::new();
        for batch in batches {
            records.push(self.publish(batch).await?);
        }
        Ok(records)
    }

    /// The exception an update raises, if any: a price outside the rule or a new item.
    fn exception(
        &self,
        update: &Update,
        before: Option<&ContentItem>,
        rules: &BTreeMap<String, PriceRule>,
    ) -> Option<HoldReason> {
        if update.trigger == Some(Trigger::OwnerRequest) {
            return None;
        }
        if before.is_none() && self.approve_new_items {
            return Some(HoldReason::NewItem);
        }
        let rule = match &update.entry {
            EntryPoint::Feed { feed } => rules.get(feed).copied().unwrap_or(self.price_rule),
            _ => self.price_rule,
        };
        (!price_within_rule(&rule, before, &update.item))
            .then_some(HoldReason::PriceRule { allowed_pct: rule.auto_within_pct })
    }

    async fn publish(&self, batch: Batch) -> Result<BatchRecord> {
        let rules: BTreeMap<String, PriceRule> = {
            let inner = self.inner.lock().await;
            inner
                .feeds
                .iter()
                .map(|(name, state)| (name.clone(), state.bounds.price_rule))
                .collect()
        };
        let now = self.clock.now();
        let mut publish = Vec::new();
        let mut arrived: BTreeMap<ContentId, DateTime<Utc>> = BTreeMap::new();
        let mut held: Vec<(ContentId, HoldReason)> = Vec::new();
        let mut new_holds = Vec::new();
        for update in &batch.updates {
            let before = self.store.get(update.id()).await?;
            match self.exception(update, before.as_ref(), &rules) {
                Some(reason) => {
                    held.push((update.id().clone(), reason.clone()));
                    new_holds.push(HeldItem {
                        update: update.clone(),
                        reason,
                        batch: batch.id,
                        held_at: now,
                    });
                }
                None => {
                    arrived.insert(update.id().clone(), update.at);
                    publish.push(self.with_live_values(update.item.clone()).await?);
                }
            }
        }
        let feeds: BTreeSet<&str> = batch
            .updates
            .iter()
            .filter_map(|u| match &u.entry {
                EntryPoint::Feed { feed } => Some(feed.as_str()),
                _ => None,
            })
            .collect();
        let all_feed = batch.updates.iter().all(|u| matches!(u.entry, EntryPoint::Feed { .. }));
        let first = batch.updates.first().map(|u| u.proposer.clone()).unwrap_or_default();
        let (job, actor) = match feeds.iter().next() {
            Some(feed) if all_feed && feeds.len() == 1 => {
                (format!("feed:{feed}"), format!("feed:{feed}"))
            }
            _ if batch.updates.iter().all(|u| u.proposer == first) => ("batch".to_string(), first),
            _ => ("batch".to_string(), "batch".to_string()),
        };
        let superseded: Vec<ContentItem> =
            batch.superseded.iter().map(|u| u.item.clone()).collect();
        let source = ReleaseSource::Edit { job, decision: None };
        let mut record = BatchRecord {
            batch: batch.id,
            opened_at: batch.opened_at,
            closed_at: batch.closed_at,
            reason: batch.reason,
            release_seq: None,
            live_at: None,
            published: Vec::new(),
            superseded: batch.superseded.len(),
            held: Vec::new(),
            refused: None,
            catalog_pages: 0,
            latencies_ms: Vec::new(),
            model_calls: batch.model_calls(),
        };
        match self.pipeline.publish_batch(publish, superseded, source, &actor).await {
            Ok(release) => {
                for (id, findings) in &release.held {
                    let Some(update) = batch.updates.iter().find(|u| u.id() == id) else {
                        continue;
                    };
                    let reason = HoldReason::FastTier { findings: findings.clone() };
                    held.push((id.clone(), reason.clone()));
                    new_holds.push(HeldItem {
                        update: update.clone(),
                        reason,
                        batch: batch.id,
                        held_at: now,
                    });
                }
                if let Some(published) = &release.published {
                    let live_at = self.clock.now();
                    record.release_seq = Some(published.staged.manifest.seq);
                    record.live_at = Some(live_at);
                    record.published = published.tag.items.clone();
                    record.catalog_pages = published.announcement.catalog_pages.len();
                    record.latencies_ms = record
                        .published
                        .iter()
                        .filter_map(|id| arrived.get(id))
                        .map(|at| (live_at - *at).num_milliseconds())
                        .collect();
                }
            }
            Err(SiteError::GateFailed(summary)) => {
                record.refused = Some(summary.clone());
                let detail = serde_json::json!({ "batch": batch.id, "gate": summary });
                self.record("site.release.refused", detail.clone()).await?;
                self.alarm(
                    Urgency::P2,
                    "site.release.refused",
                    format!("Batch {} was refused whole: it holds server code (A-I1)", batch.id),
                    detail,
                )
                .await?;
            }
            Err(error) => return Err(error),
        }
        record.held = held;
        {
            let mut inner = self.lock_held().await?;
            inner.review.held_items += new_holds.len() as u64;
            if record.release_seq.is_some() {
                inner.review.releases += 1;
            }
            let holds = !new_holds.is_empty();
            for hold in new_holds {
                inner.held_items.insert(hold.update.id().clone(), hold);
            }
            if holds {
                self.save_held(&inner).await?;
            }
            inner.records.push_back(record.clone());
            while inner.records.len() > RECORDS_KEPT {
                inner.records.pop_front();
            }
        }
        if !record.held.is_empty() {
            let detail = serde_json::json!({ "batch": record.batch, "held": record.held });
            self.record("site.batch.held", detail).await?;
        }
        if let Some(seq) = record.release_seq {
            let detail = serde_json::json!({
                "batch": record.batch,
                "releaseSeq": seq,
                "items": record.published.len(),
                "superseded": record.superseded,
            });
            self.record("site.batch.published", detail).await?;
        }
        Ok(record)
    }

    /// The item with its live fields set to the values the edge serves now, so the page
    /// shows the last released value with its time when script is off (spec 3.5 §7.8.3).
    async fn with_live_values(&self, mut item: ContentItem) -> Result<ContentItem> {
        let fields: Vec<String> = crate::lanes::live_fields(self.pipeline.site_config(), &item)
            .into_iter()
            .map(str::to_string)
            .collect();
        if fields.is_empty() {
            return Ok(item);
        }
        let live = self.live.get(&item.id).await?;
        for field in fields {
            if let Some(value) = live.get(&field) {
                item.fields.insert(field, value.value.clone());
            }
        }
        Ok(item)
    }

    async fn full_checks_if_due(&self) -> Result<Option<GateReport>> {
        let now = self.clock.now();
        let seq = self.pipeline.previous().await.map_or(0, |p| p.manifest.seq);
        if !self.inner.lock().await.schedule.due(now, seq) {
            return Ok(None);
        }
        let report = self.pipeline.full_check().await?;
        {
            let mut inner = self.inner.lock().await;
            inner.schedule.ran(now, seq);
            inner.review.full_checks += 1;
        }
        if !report.passed() {
            let detail = serde_json::json!({ "releaseSeq": seq, "summary": report.summary() });
            self.alarm(
                Urgency::P2,
                "site.checks.failed",
                format!("The scheduled full checks failed on release {seq}: {}", report.summary()),
                detail,
            )
            .await?;
        }
        Ok(Some(report))
    }

    /// Runs the scheduled full checks now.
    ///
    /// # Errors
    ///
    /// Build and gate errors.
    pub async fn run_full_checks(&self) -> Result<GateReport> {
        let now = self.clock.now();
        let seq = self.pipeline.previous().await.map_or(0, |p| p.manifest.seq);
        let report = self.pipeline.full_check().await?;
        let mut inner = self.inner.lock().await;
        inner.schedule.ran(now, seq);
        inner.review.full_checks += 1;
        Ok(report)
    }

    /// The owner's decision on a held delivery: release applies it whole, discard drops it
    /// (spec 3.5 A-I27).
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for a delivery that is not held; as [`ingest`](Self::ingest).
    pub async fn decide_delivery(
        &self,
        delivery: u64,
        decision: Decision,
        by: &str,
    ) -> Result<DeliveryOutcome> {
        let held = {
            let mut inner = self.lock_held().await?;
            let held = inner
                .held_deliveries
                .remove(&delivery)
                .ok_or_else(|| SiteError::NotFound(format!("held delivery {delivery}")))?;
            self.save_held(&inner).await?;
            held
        };
        let detail = serde_json::json!({ "delivery": delivery, "feed": held.feed, "decision": decision, "by": by });
        self.record("site.batch.decided", detail).await?;
        match decision {
            Decision::Discard => Ok(DeliveryOutcome { delivery, ..DeliveryOutcome::default() }),
            Decision::Release => {
                let kept = held.clone();
                match self.apply(&held.feed, held.changes, self.clock.now()).await {
                    Ok(mut outcome) => {
                        outcome.delivery = delivery;
                        Ok(outcome)
                    }
                    Err(error) => {
                        // Still held: the owner's decision did not take effect.
                        let mut inner = self.lock_held().await?;
                        inner.held_deliveries.insert(delivery, kept);
                        self.save_held(&inner).await?;
                        Err(error)
                    }
                }
            }
        }
    }

    /// The owner's decision on a held item. Releasing it publishes it at once; the approver
    /// must not be the change's proposer (A-I23).
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an item that is not held; [`SiteError::Pipeline`] when
    /// the proposer approves their own change; publishing errors.
    pub async fn decide_item(
        &self,
        id: &ContentId,
        decision: Decision,
        by: &str,
    ) -> Result<Vec<BatchRecord>> {
        let held = {
            let mut inner = self.lock_held().await?;
            let held = inner.held_items.get(id).ok_or_else(|| SiteError::NotFound(id.0.clone()))?;
            if decision == Decision::Release && held.update.proposer == by {
                return Err(SiteError::Pipeline(format!(
                    "'{by}' proposed the change and cannot approve it (A-I23)"
                )));
            }
            let held =
                inner.held_items.remove(id).ok_or_else(|| SiteError::NotFound(id.0.clone()))?;
            self.save_held(&inner).await?;
            held
        };
        let detail = serde_json::json!({ "item": id, "decision": decision, "by": by });
        self.record("site.content.decided", detail).await?;
        match decision {
            Decision::Discard => Ok(Vec::new()),
            Decision::Release => {
                let mut update = held.update.clone();
                update.at = self.clock.now();
                update.trigger = Some(Trigger::OwnerRequest);
                match self.submit(update).await {
                    Ok(records) => Ok(records),
                    Err(error) => {
                        // Still held: the owner's decision did not take effect.
                        let mut inner = self.lock_held().await?;
                        inner.held_items.insert(id.clone(), held);
                        self.save_held(&inner).await?;
                        Err(error)
                    }
                }
            }
        }
    }

    /// Publishes one item's earlier version as a new release and keeps every other change:
    /// the version release `to` served, or the one before its latest release (spec 3.5
    /// §7.8.3 "Undo per item", `site revert <item> [--to <seq>]`).
    ///
    /// # Errors
    ///
    /// [`SiteError::Pipeline`] without a content history or an earlier version; publishing
    /// errors.
    pub async fn revert(
        &self,
        id: &ContentId,
        to: Option<u64>,
        by: &str,
        entry: EntryPoint,
    ) -> Result<Vec<BatchRecord>> {
        let history = self
            .history
            .as_ref()
            .ok_or_else(|| SiteError::Pipeline("reverting needs a content history".into()))?;
        let version = match to {
            Some(seq) => history.at_release(id, seq).await?,
            None => history.previous_release(id).await?,
        }
        .ok_or_else(|| {
            SiteError::Pipeline(format!("item '{id}' has no earlier released version"))
        })?;
        let detail =
            serde_json::json!({ "item": id, "to": to, "version": version.version, "by": by });
        self.record("site.content.reverted", detail).await?;
        tracing::info!(item = %id, version = version.version, "site.content.reverted");
        self.submit(Update {
            item: version.item,
            at: self.clock.now(),
            entry,
            proposer: by.to_string(),
            trigger: Some(Trigger::OwnerRequest),
            model_calls: 0,
        })
        .await
    }

    /// The items changed in releases after `since`, with their latest released versions,
    /// in release order: what the Steward proposes replaying after a rollback to `since`
    /// (spec 3.5 §7.8.3 "Undo per item").
    ///
    /// # Errors
    ///
    /// [`SiteError::Pipeline`] without a content history; store and history errors.
    pub async fn replay_list(&self, since: u64) -> Result<Vec<ReplayCandidate>> {
        let history = self
            .history
            .as_ref()
            .ok_or_else(|| SiteError::Pipeline("a replay list needs a content history".into()))?;
        let mut candidates = Vec::new();
        for item in self.store.list().await? {
            let latest = history
                .versions(&item.id)
                .await?
                .into_iter()
                .filter(|entry| entry.meta.release_seq.is_some_and(|seq| seq > since))
                .max_by_key(|entry| (entry.meta.release_seq, entry.version));
            if let Some(HistoryEntry { version, meta, .. }) = latest {
                candidates.push(ReplayCandidate {
                    id: item.id.clone(),
                    release_seq: meta.release_seq.unwrap_or_default(),
                    at: meta.at,
                    actor: meta.actor,
                    version,
                });
            }
        }
        candidates.sort_by(|a, b| (a.release_seq, &a.id).cmp(&(b.release_seq, &b.id)));
        Ok(candidates)
    }

    /// Rolls the whole site back to release `to_seq` for an incident (A-I11), puts the
    /// working copy back to what that release served, and returns the changes made since,
    /// which the Steward proposes replaying one by one with [`replay`](Self::replay).
    ///
    /// # Errors
    ///
    /// As [`PublishingPipeline::rollback`] and [`replay_list`](Self::replay_list).
    pub async fn rollback(&self, to_seq: u64) -> Result<(Published, Vec<ReplayCandidate>)> {
        let candidates = self.replay_list(to_seq).await?;
        let published = self.pipeline.rollback(to_seq).await?;
        if let Some(history) = &self.history {
            for candidate in &candidates {
                match history.at_release(&candidate.id, to_seq).await? {
                    Some(entry) => self.store.put(&entry.item).await?,
                    None => {
                        self.store.remove(&candidate.id).await?;
                    }
                }
            }
        }
        Ok((published, candidates))
    }

    /// Publishes a change listed by [`rollback`](Self::rollback) again, as the owner's
    /// request.
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] when the version is gone from the history; publishing errors.
    pub async fn replay(
        &self,
        candidate: &ReplayCandidate,
        by: &str,
        entry: EntryPoint,
    ) -> Result<Vec<BatchRecord>> {
        let history = self
            .history
            .as_ref()
            .ok_or_else(|| SiteError::Pipeline("replaying needs a content history".into()))?;
        let version = history
            .versions(&candidate.id)
            .await?
            .into_iter()
            .find(|entry| entry.version == candidate.version)
            .ok_or_else(|| {
                SiteError::NotFound(format!("{} version {}", candidate.id, candidate.version))
            })?;
        self.submit(Update {
            item: version.item,
            at: self.clock.now(),
            entry,
            proposer: by.to_string(),
            trigger: Some(Trigger::OwnerRequest),
            model_calls: 0,
        })
        .await
    }

    /// Recent batches with their held items, newest last (`site batches`).
    pub async fn batches(&self) -> Vec<BatchRecord> {
        self.inner.lock().await.records.iter().cloned().collect()
    }

    /// Items held alone, waiting for a decision, including those held before a restart.
    pub async fn held_items(&self) -> Vec<HeldItem> {
        match self.lock_held().await {
            Ok(inner) => inner.held_items.values().cloned().collect(),
            Err(error) => {
                tracing::warn!(error = %error, "held updates could not be read");
                self.inner.lock().await.held_items.values().cloned().collect()
            }
        }
    }

    /// Deliveries held whole, waiting for the owner, including those held before a restart.
    pub async fn held_deliveries(&self) -> Vec<HeldDelivery> {
        match self.lock_held().await {
            Ok(inner) => inner.held_deliveries.values().cloned().collect(),
            Err(error) => {
                tracing::warn!(error = %error, "held updates could not be read");
                self.inner.lock().await.held_deliveries.values().cloned().collect()
            }
        }
    }

    /// Feeds with their bounds and rates (`site feeds`).
    pub async fn feeds(&self) -> Vec<FeedStatus> {
        let now = self.clock.now();
        self.inner
            .lock()
            .await
            .feeds
            .values()
            .map(|state| FeedStatus {
                mapping: state.mapping.clone(),
                bounds: state.bounds.clone(),
                deliveries: state.deliveries,
                records: state.records,
                records_last_minute: state.records_last_minute(now),
                last_delivery: state.last_delivery,
                held: state.held,
                model_calls: 0,
            })
            .collect()
    }

    /// Update counts and costs for the weekly review: cost per thousand updates and the
    /// share of updates that needed a model.
    pub async fn review(&self) -> UpdateReview {
        let mut review = self.inner.lock().await.review.clone();
        review.model_share = if review.updates == 0 {
            0.0
        } else {
            review.updates_with_model as f64 / review.updates as f64
        };
        review.cost_micros = review.model_calls * self.costs.per_model_call_micros
            + review.releases * self.costs.per_release_micros;
        review.cost_per_thousand_updates_micros = if review.updates == 0 {
            0.0
        } else {
            review.cost_micros as f64 * 1_000.0 / review.updates as f64
        };
        review
    }

    /// The 50th and 95th percentile approval-to-live, in milliseconds, over the batches
    /// kept for `site batches`.
    pub async fn latency_percentiles(&self) -> Option<(i64, i64)> {
        let mut all: Vec<i64> =
            self.inner.lock().await.records.iter().flat_map(|r| r.latencies_ms.clone()).collect();
        let p50 = percentile(&mut all, 50.0)?;
        let p95 = percentile(&mut all, 95.0)?;
        Some((p50, p95))
    }

    async fn alarm(
        &self,
        urgency: Urgency,
        code: &str,
        title: String,
        detail: Value,
    ) -> Result<()> {
        let alarm = SiteAlarm {
            urgency,
            code: code.into(),
            title,
            detail: detail.clone(),
            at: self.clock.now(),
        };
        if let Some(alarms) = &self.alarms {
            alarms.raise(&alarm).await?;
        }
        self.record(
            "website.alarm.raised",
            serde_json::json!({ "code": code, "urgency": urgency, "detail": detail }),
        )
        .await
    }

    async fn record(&self, action: &str, value: Value) -> Result<()> {
        let Some((recorder, acting)) = &self.audit else { return Ok(()) };
        let draft = AuditRecordDraft::new(AuditRecordKind::Other("site".into()), action)
            .actor(AuditActor::new(acting, ""))
            .args_digest(recorder.digest(&value))
            .ext("site", value);
        recorder.record(draft).await.map_err(|e| SiteError::Pipeline(format!("audit: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_pick_ranks() {
        let mut values = vec![5, 1, 4, 2, 3, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&mut values, 50.0), Some(5));
        assert_eq!(percentile(&mut values, 95.0), Some(10));
        assert_eq!(percentile(&mut [], 95.0), None);
    }
}
