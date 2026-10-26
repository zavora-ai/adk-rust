//! The site view (spec 3.5 §7.24, §8.2): status, releases and their page changes, KPIs with
//! trends, jobs and their autonomy levels, signals, the wallet and held batches — and the
//! owner's decisions on them.
//!
//! | Decision | Who | What happens |
//! |---|---|---|
//! | Promote a job | An owner (A-I13) | The kernel's ladder raises it one rung by the owner's decision; the harness grants the standing rules the new level needs |
//! | Lower a job | An owner | The ladder lowers it one rung; the standing rules it no longer warrants are revoked |
//! | Release or discard a held item | An owner, never the change's proposer (A-I23, A-I25) | The update engine publishes it at once, or drops it |
//! | Release or discard a held feed delivery | An owner (A-I27) | The engine applies it whole, or drops it whole |
//! | Acknowledge an alarm | Anyone with a role | The alarm stops repeating |

use std::collections::BTreeMap;

use adk_core::Result;
use adk_core::principal::SubjectRef;
use adk_harness::website::{LadderCompiler, apply_demotion, promote_job};
use adk_kernel::charter::{KpiPhase, OwnerDecision, PromotionEligibility, SignalFilter};
use adk_payments::wallet::{HoldReason as WalletHold, PaymentQuery, PaymentState};
use adk_site::feed::bounds::BoundBreak;
use adk_site::pipeline::PageChange;
use adk_site::release::manifest_path;
use adk_site::updates::{Decision, HoldReason};
use awp_types::{
    AutonomyLevel, ContentId, Direction, FileEntry, ReleaseManifest, ReleaseSource, Role,
    SignalKind, SpendCategory, Urgency,
};
use chrono::{DateTime, Datelike, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::words::{self, major, money};
use super::{Pill, Tone, WebsiteDesk, invalid, kernel, not_found, site, unavailable};
use crate::kinds::template;
use crate::team::SiteTeam;

/// How many releases the view lists.
const RELEASES: u64 = 8;

/// The site's status (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    /// The state, as a pill.
    pub state: Pill,
    /// Since when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Why, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A page a release changed (spec 3.3 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageLine {
    /// The page.
    pub path: String,
    /// How it changed.
    pub change: Pill,
}

/// One signed release (spec 3.5 §7.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseLine {
    /// Its number.
    pub seq: u64,
    /// When it was built.
    pub at: DateTime<Utc>,
    /// What made it, in words.
    pub source: String,
    /// The pages it changed.
    pub pages: Vec<PageLine>,
    /// How many other files it changed.
    pub other_files: usize,
}

/// One KPI this week (spec 3.5 §7.11, §7.24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KpiCard {
    /// The KPI.
    pub id: String,
    /// Its name.
    pub name: String,
    /// This week.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub this_week: Option<String>,
    /// The week before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_week: Option<String>,
    /// Its baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
    /// Its target, the owner's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The trend, in its direction: up is better.
    pub trend: Pill,
    /// "Measuring until 8 Nov" or "Tracking".
    pub phase: String,
}

/// One job and its level (spec 3.5 §7.11, §7.20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobLine {
    /// The job.
    pub id: String,
    /// What it is.
    pub title: String,
    /// Its area.
    pub area: String,
    /// The agent that does it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// How much it does alone.
    pub level: String,
    /// Since when.
    pub since: DateTime<Utc>,
    /// Its record towards a promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// Whether it has earned the offer of a promotion.
    pub offered: bool,
    /// The level a promotion would give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Whether the viewer may raise it.
    pub can_promote: bool,
    /// Whether the viewer may lower it.
    pub can_lower: bool,
}

/// One signal from the public side, typed and screened (spec 3.5 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignalLine {
    /// What it is.
    pub text: String,
    /// How many arrived.
    pub count: u32,
    /// The latest.
    pub last_at: DateTime<Utc>,
}

/// Signals waiting in the back office.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignalsView {
    /// How many are open.
    pub open: usize,
    /// The latest open ones.
    pub lines: Vec<SignalLine>,
}

/// A spend category against its cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CategoryLine {
    /// The category.
    pub name: String,
    /// Spent this month.
    pub spent: String,
    /// Its cap.
    pub cap: String,
    /// Spent as a share of the cap, in percent from 0 to 100.
    pub percent: u32,
    /// Why it is paused, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<String>,
}

/// A payment held for the owner (spec 3.5 §7.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldPayment {
    /// What it is for.
    pub description: String,
    /// How much.
    pub amount: String,
    /// Why it waits.
    pub why: String,
}

/// The wallet (spec 3.5 §7.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WalletView {
    /// What is available.
    pub balance: String,
    /// Spent this month.
    pub spent: String,
    /// The month's caps together.
    pub budget: String,
    /// Each category.
    pub categories: Vec<CategoryLine>,
    /// Payments held for the owner.
    pub held: Vec<HeldPayment>,
}

/// An item held alone (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldItemLine {
    /// The item.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Why it waits.
    pub reason: String,
    /// The batch it came in.
    pub batch: u64,
    /// When it was held.
    pub held_at: DateTime<Utc>,
}

/// A feed delivery held whole (spec 3.5 A-I27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldDeliveryLine {
    /// The delivery.
    pub delivery: u64,
    /// The feed.
    pub feed: String,
    /// The bounds it broke.
    pub breaks: Vec<String>,
    /// What it would change.
    pub would: Vec<String>,
    /// When it arrived.
    pub received_at: DateTime<Utc>,
}

/// One recent batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchLine {
    /// The batch.
    pub batch: u64,
    /// When it closed.
    pub closed_at: DateTime<Utc>,
    /// One line.
    pub text: String,
}

/// Held batches (spec 3.5 §7.8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldView {
    /// Items held alone.
    pub items: Vec<HeldItemLine>,
    /// Deliveries held whole.
    pub deliveries: Vec<HeldDeliveryLine>,
    /// The latest batches.
    pub batches: Vec<BatchLine>,
}

/// An open alarm (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlarmLine {
    /// The alarm.
    pub id: String,
    /// Its urgency.
    pub urgency: Pill,
    /// What happened.
    pub text: String,
    /// When.
    pub raised_at: DateTime<Utc>,
    /// Whether it repeats until a person acknowledges it: an urgent alarm.
    pub can_acknowledge: bool,
}

/// The site as every surface shows it (spec 3.5 §7.24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SiteViewModel {
    /// The site's name.
    pub name: String,
    /// Its public address.
    pub origin: String,
    /// Whether it is live.
    pub launched: bool,
    /// Its status.
    pub status: StatusView,
    /// The latest releases, newest first.
    pub releases: Vec<ReleaseLine>,
    /// This week's KPIs.
    pub kpis: Vec<KpiCard>,
    /// The jobs and their levels.
    pub jobs: Vec<JobLine>,
    /// Signals from the public side.
    pub signals: SignalsView,
    /// The wallet, when the site has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet: Option<WalletView>,
    /// Held batches.
    pub held: HeldView,
    /// Open alarms.
    pub alarms: Vec<AlarmLine>,
    /// Whether the viewer decides as an owner.
    pub owner: bool,
}

/// An owner's decision on the site view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SiteCommand {
    /// Raise a job one rung (A-I13).
    Promote {
        /// The job.
        job: String,
    },
    /// Lower a job one rung.
    Lower {
        /// The job.
        job: String,
        /// Why.
        reason: String,
    },
    /// Release or discard an item held alone.
    Item {
        /// The item.
        id: String,
        /// `release` or `discard`.
        decision: HeldDecision,
    },
    /// Release or discard a delivery held whole (A-I27).
    Delivery {
        /// The delivery.
        delivery: u64,
        /// `release` or `discard`.
        decision: HeldDecision,
    },
    /// Acknowledge an alarm.
    Acknowledge {
        /// The alarm.
        alarm: String,
    },
}

/// The owner's decision on something held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HeldDecision {
    /// Publish it.
    Release,
    /// Drop it.
    Discard,
}

/// What a site decision did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SiteOutcome {
    /// The view after it.
    pub view: SiteViewModel,
    /// One line on what happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub(crate) fn source_words(source: &ReleaseSource) -> String {
    match source {
        ReleaseSource::Onboarding => "The first release".into(),
        ReleaseSource::Migration => "Moved from the old site".into(),
        ReleaseSource::Rollback { to_seq } => format!("Rolled back to release {to_seq}"),
        ReleaseSource::Edit { job, .. } => match job.as_str() {
            "web-editor" => "An edit in the web editor".into(),
            "correction" => "A correction".into(),
            "withdrawal" => "A withdrawal".into(),
            "revert" => "One item undone".into(),
            "onboarding" => "The launch".into(),
            other => crate::jobs::job(other).map_or_else(
                || format!("An edit by {}", other.replace('-', " ")),
                |spec| format!("{} job", spec.title),
            ),
        },
        _ => "A release".into(),
    }
}

fn hold_words(reason: &HoldReason) -> String {
    match reason {
        HoldReason::PriceRule { allowed_pct } => {
            format!("The price moves more than the {allowed_pct}% your price rule allows")
        }
        HoldReason::NewItem => "A new item waits for its approval".into(),
        HoldReason::FastTier { findings } => format!(
            "It failed a check: {}",
            findings.first().map_or("see the batch", |f| f.message.as_str())
        ),
        _ => "It waits for you".into(),
    }
}

/// "item" or "items", for `n` of them.
fn items(n: usize) -> &'static str {
    if n == 1 { "item" } else { "items" }
}

fn break_words(bound: &BoundBreak) -> String {
    match bound {
        BoundBreak::ChangeFraction { changed, collection, max } => format!(
            "It changes {changed} of {collection} {}; at most {:.0}% may change at once",
            items(*collection),
            max * 100.0
        ),
        BoundBreak::Field { field, key, .. } => format!(
            "{} has a value outside its allowed range{}",
            field.as_deref().map_or("A field".to_string(), |f| format!("The {f}")),
            key.as_deref().map(|k| format!(" (record {k})")).unwrap_or_default()
        ),
        BoundBreak::Rate { records, .. } => {
            format!("{records} records in a minute is faster than allowed")
        }
        _ => "It breaks a bound".into(),
    }
}

fn signal_words(kind: &SignalKind) -> String {
    match kind {
        SignalKind::LeadCaptured { source, .. } => format!("A new lead, from {}", source.as_str()),
        SignalKind::ContentGap { topic, question_count, .. } => {
            format!(
                "Visitors asked about \u{201c}{}\u{201d} ({question_count} times)",
                topic.as_str()
            )
        }
        SignalKind::VisitorIntent { capability, .. } => {
            format!("Someone tried {}", capability.replace('_', " "))
        }
        SignalKind::SupportEscalation { reason, .. } => {
            format!("Someone needs help: {}", reason.0.replace('_', " "))
        }
        SignalKind::ReviewSeen { platform, rating, .. } => {
            format!("A {rating}-star review on {platform}")
        }
        SignalKind::InboundMessage { channel, .. } => format!("A message on {}", channel.as_str()),
        SignalKind::AgentInteraction { capability, .. } => {
            format!("An agent used {}", capability.replace('_', " "))
        }
        _ => "Something on the public side".into(),
    }
}

/// The page files of a release that changed, against the release before it.
fn page_changes(
    before: &BTreeMap<String, FileEntry>,
    after: &BTreeMap<String, FileEntry>,
) -> (Vec<PageLine>, usize) {
    let mut pages = Vec::new();
    let mut other = 0;
    for (path, entry) in after {
        let change = match before.get(path) {
            None => PageChange::Added,
            Some(old) if old != entry => PageChange::Changed,
            Some(_) => continue,
        };
        if path.ends_with(".html") {
            pages.push(PageLine { path: path.clone(), change: words::page_change(change) });
        } else {
            other += 1;
        }
    }
    for path in before.keys().filter(|p| !after.contains_key(*p)) {
        if path.ends_with(".html") {
            pages.push(PageLine {
                path: path.clone(),
                change: words::page_change(PageChange::Removed),
            });
        } else {
            other += 1;
        }
    }
    (pages, other)
}

impl WebsiteDesk {
    /// The full file set of release `seq`, from the manifests the host serves.
    async fn files_of(
        &self,
        seq: u64,
        cache: &mut BTreeMap<u64, (ReleaseManifest, BTreeMap<String, FileEntry>)>,
    ) -> Result<Option<(ReleaseManifest, BTreeMap<String, FileEntry>)>> {
        if let Some(found) = cache.get(&seq) {
            return Ok(Some(found.clone()));
        }
        let runtime = self.runtime().await;
        let mut chain: Vec<ReleaseManifest> = Vec::new();
        let mut base_files = BTreeMap::new();
        let mut at = seq;
        loop {
            if let Some((_, files)) = cache.get(&at) {
                base_files = files.clone();
                break;
            }
            let Some(bytes) = runtime.host().fetch(&manifest_path(at)).await.map_err(site)? else {
                return Ok(None);
            };
            let manifest: ReleaseManifest = serde_json::from_slice(&bytes).map_err(|e| {
                invalid("website.site.manifest", format!("release {at}'s manifest: {e}"))
            })?;
            let base = manifest.base;
            chain.push(manifest);
            match base {
                Some(base) => at = base,
                None => break,
            }
        }
        let mut files = base_files;
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
            cache.insert(manifest.seq, (manifest.clone(), files.clone()));
        }
        Ok(cache.get(&seq).cloned())
    }

    async fn releases(&self) -> Result<Vec<ReleaseLine>> {
        let runtime = self.runtime().await;
        let Some(latest) = runtime.pipeline().previous().await.map(|p| p.manifest.seq) else {
            return Ok(Vec::new());
        };
        let mut cache = BTreeMap::new();
        let mut lines = Vec::new();
        let first = latest.saturating_sub(RELEASES - 1).max(1);
        for seq in (first..=latest).rev() {
            let Some((manifest, files)) = self.files_of(seq, &mut cache).await? else { continue };
            let before = if seq > 1 {
                self.files_of(seq - 1, &mut cache)
                    .await?
                    .map(|(_, files)| files)
                    .unwrap_or_default()
            } else {
                BTreeMap::new()
            };
            let (pages, other_files) = page_changes(&before, &files);
            lines.push(ReleaseLine {
                seq,
                at: manifest.created_at,
                source: source_words(&manifest.source),
                pages,
                other_files,
            });
        }
        Ok(lines)
    }

    async fn compiler(&self) -> Result<Option<(LadderCompiler, awp_types::Charter)>> {
        let Some(charters) = &self.inner.charters else { return Ok(None) };
        let Some(current) = charters.current(self.inner.site.id).await.map_err(kernel)? else {
            return Ok(None);
        };
        let template = template(current.charter.kind);
        let team = SiteTeam::new(self.inner.site.clone(), &template);
        let jobs = template.jobs();
        let compiler = LadderCompiler::new(self.inner.site.clone())
            .with_charter(&current.charter, crate::jobs::bindings(&team, &jobs));
        Ok(Some((compiler, current.charter)))
    }

    /// The site view for `person` (spec 3.5 §7.24). Anyone holding a role in the charter
    /// reads it.
    ///
    /// # Errors
    ///
    /// `Forbidden` for a person with no role; the stores' errors.
    pub async fn site_view(&self, person: &str) -> Result<SiteViewModel> {
        let roles = self.roles(person).await?;
        if roles.is_empty() {
            return Err(super::forbidden(
                "website.ui.not_allowed",
                format!("{person} holds no role in this site's charter"),
            ));
        }
        let owner = roles.contains(&Role::Owner);
        let site_id = self.inner.site.id;
        let now = self.now();
        let today = now.date_naive();
        let monday =
            today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()));

        let status = match &self.inner.status {
            Some(status) => status.get(site_id).await?,
            None => None,
        };
        let status = match status {
            Some(record) => StatusView {
                state: words::site_state(record.state),
                since: Some(record.since),
                reason: Some(sentence(&record.reason)),
            },
            None => StatusView {
                state: Pill::new("Not live yet", Tone::Neutral),
                since: None,
                reason: None,
            },
        };
        let releases = self.releases().await?;
        let charter = match &self.inner.charters {
            Some(charters) => charters.current(site_id).await.map_err(kernel)?.map(|c| c.charter),
            None => None,
        };

        let mut kpis = Vec::new();
        if let Some(tracker) = &self.inner.tracker {
            for review in tracker.review(monday).await.map_err(kernel)? {
                let direction = charter
                    .as_ref()
                    .and_then(|c| c.kpis.iter().find(|k| k.id == review.kpi).map(|k| k.direction));
                let trend = match (review.week, review.previous_week) {
                    (Some(week), Some(previous)) if (week - previous).abs() < f64::EPSILON => {
                        Pill::new("Same", Tone::Neutral)
                    }
                    (Some(week), Some(previous)) => {
                        let better =
                            matches!(direction, Some(Direction::Down)) != (week > previous);
                        if better {
                            Pill::new("Better", Tone::Good)
                        } else {
                            Pill::new("Worse", Tone::Bad)
                        }
                    }
                    _ => Pill::new("New", Tone::Neutral),
                };
                kpis.push(KpiCard {
                    id: review.kpi.clone(),
                    name: review.name.clone(),
                    this_week: review.week.map(words::number),
                    last_week: review.previous_week.map(words::number),
                    baseline: review.baseline.map(words::number),
                    target: review.target.map(words::number),
                    trend,
                    phase: match &review.phase {
                        KpiPhase::MeasureOnly { until } => {
                            format!("Finding its baseline until {}", until.format("%-d %b"))
                        }
                        _ => "Tracking".into(),
                    },
                });
            }
        }

        let mut jobs = Vec::new();
        if let Some(ladder) = &self.inner.ladder {
            let records = ladder.store().list(site_id).await.map_err(kernel)?;
            let mut offers: BTreeMap<String, PromotionEligibility> = BTreeMap::new();
            for record in &records {
                offers.insert(
                    record.job.clone(),
                    ladder.eligibility(site_id, &record.job).await.map_err(kernel)?,
                );
            }
            let team =
                charter.as_ref().map(|c| SiteTeam::new(self.inner.site.clone(), &template(c.kind)));
            for record in records {
                let spec = crate::jobs::job(&record.job);
                let offer = offers.get(&record.job);
                let next = offer.and_then(|o| o.next);
                jobs.push(JobLine {
                    title: spec
                        .map_or_else(|| record.job.replace('-', " "), |s| s.title.to_string()),
                    area: record.area.as_deref().map_or("Other", words::area).into(),
                    agent: match (spec, &team) {
                        (Some(spec), Some(team)) => {
                            spec.ladder_agent(team).map(|a| words::agent(a).to_string())
                        }
                        _ => None,
                    },
                    level: words::level(record.level).into(),
                    since: record.since,
                    record: offer.filter(|o| o.streak > 0).map(|o| {
                        format!(
                            "{} of {} approvals in a row unchanged",
                            o.streak.min(o.needed),
                            o.needed
                        )
                    }),
                    offered: offer.is_some_and(|o| o.eligible),
                    next: next.map(|level| words::level(level).to_string()),
                    // Autonomy is earned (D-7): the owner promotes a job the record offers.
                    can_promote: owner
                        && next.is_some()
                        && offer.is_some_and(|o| o.eligible)
                        && self.inner.rules.is_some(),
                    can_lower: owner && record.level != AutonomyLevel::Observe,
                    id: record.job,
                });
            }
            jobs.sort_by(|a, b| {
                (b.offered, &a.area, &a.title).cmp(&(a.offered, &b.area, &b.title))
            });
        }

        let signals = match &self.inner.signals {
            Some((store, partition)) => {
                let records = store
                    .list(&SignalFilter::default().partition(partition.clone()).open(true))
                    .await
                    .map_err(kernel)?;
                let mut lines: Vec<SignalLine> = records
                    .iter()
                    .map(|record| SignalLine {
                        text: signal_words(&record.signal.kind),
                        count: record.count,
                        last_at: record.last_at,
                    })
                    .collect();
                lines.sort_by_key(|line| std::cmp::Reverse(line.last_at));
                lines.truncate(8);
                SignalsView { open: records.len(), lines }
            }
            None => SignalsView { open: 0, lines: Vec::new() },
        };

        let wallet = match &self.inner.wallet {
            Some(parts) => {
                let policy = parts.wallet.policy();
                let currency = policy.currency.clone();
                let month = today
                    .with_day(1)
                    .and_then(|day| day.and_hms_opt(0, 0, 0))
                    .map_or(now, |start| start.and_utc());
                let read = |metric: String| {
                    let kpi = parts.kpi.clone();
                    async move {
                        kpi.0.read(site_id, &metric, month, now).await.ok().flatten().unwrap_or(0.0)
                    }
                };
                let pauses = parts.store.pauses(parts.wallet.id()).await.unwrap_or_default();
                let mut categories = Vec::new();
                let mut budget = 0u64;
                for (category, mandate) in &policy.categories {
                    budget += mandate.cap_per_period;
                    let spent = read(format!("spend.{}", category_key(*category))).await;
                    let cap_major = mandate.cap_per_period as f64
                        / 10f64.powi(words::minor_exponent(&currency));
                    categories.push(CategoryLine {
                        name: words::category(*category).into(),
                        spent: major(spent, &currency),
                        cap: money(mandate.cap_per_period, &currency),
                        percent: if cap_major > 0.0 {
                            ((spent / cap_major).clamp(0.0, 1.0) * 100.0).round() as u32
                        } else {
                            0
                        },
                        paused: pauses
                            .iter()
                            .find(|p| p.category == *category)
                            .map(|p| p.reason.clone()),
                    });
                }
                let held = parts
                    .store
                    .list(&PaymentQuery::new(parts.wallet.id()).state(PaymentState::Held))
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|payment| HeldPayment {
                        description: payment.description.clone(),
                        amount: money(payment.amount, &payment.currency),
                        why: match &payment.hold {
                            Some(WalletHold::OverPaymentLimit { max_per_payment, .. }) => format!(
                                "Over the {} limit for one payment",
                                money(*max_per_payment, &payment.currency)
                            ),
                            Some(WalletHold::OverCategoryCap { cap, spent, .. }) => format!(
                                "It would take the month past its {} cap ({} spent)",
                                money(*cap, &payment.currency),
                                money(*spent, &payment.currency)
                            ),
                            _ => "Over a limit you set".into(),
                        },
                    })
                    .collect();
                Some(WalletView {
                    balance: money(parts.wallet.balance().await.unwrap_or(0), &currency),
                    spent: major(read("spend".into()).await, &currency),
                    budget: money(budget, &currency),
                    categories,
                    held,
                })
            }
            None => None,
        };

        let held = match &self.inner.updates {
            Some(engine) => HeldView {
                items: engine
                    .held_items()
                    .await
                    .into_iter()
                    .map(|held| HeldItemLine {
                        id: held.update.item.id.0.clone(),
                        title: held
                            .update
                            .item
                            .fields
                            .get("title")
                            .and_then(|v| v.as_str())
                            .map_or_else(|| held.update.item.slug.clone(), str::to_string),
                        reason: hold_words(&held.reason),
                        batch: held.batch,
                        held_at: held.held_at,
                    })
                    .collect(),
                deliveries: engine
                    .held_deliveries()
                    .await
                    .into_iter()
                    .map(|held| {
                        let summary = &held.report.summary;
                        let mut would = vec![format!(
                            "Change {} of {} {} ({:.0}%)",
                            summary.items_changed,
                            summary.collection,
                            items(summary.collection),
                            summary.fraction * 100.0
                        )];
                        if let Some(prices) = &summary.prices {
                            would.push(if prices.changed == 1 {
                                format!("1 price changes, by {:.0}%", prices.largest_change_pct)
                            } else {
                                format!(
                                    "{} prices change, the largest by {:.0}%",
                                    prices.changed, prices.largest_change_pct
                                )
                            });
                        }
                        HeldDeliveryLine {
                            delivery: held.delivery,
                            feed: held.feed.clone(),
                            breaks: held.report.breaks.iter().map(break_words).collect(),
                            would,
                            received_at: held.received_at,
                        }
                    })
                    .collect(),
                batches: engine
                    .batches()
                    .await
                    .into_iter()
                    .rev()
                    .take(5)
                    .map(|batch| BatchLine {
                        batch: batch.batch,
                        closed_at: batch.closed_at,
                        text: match (&batch.refused, batch.release_seq) {
                            (Some(reason), _) => format!("Refused whole: {reason}"),
                            (None, Some(seq)) => format!(
                                "{} item{} live in release {seq}{}",
                                batch.published.len(),
                                if batch.published.len() == 1 { "" } else { "s" },
                                if batch.held.is_empty() {
                                    String::new()
                                } else {
                                    format!("; {} held", batch.held.len())
                                }
                            ),
                            (None, None) => format!("Nothing published; {} held", batch.held.len()),
                        },
                    })
                    .collect(),
            },
            None => HeldView { items: Vec::new(), deliveries: Vec::new(), batches: Vec::new() },
        };

        let alarms = match &self.inner.alarms {
            Some(router) => {
                let open = router.open().await;
                let recent = router.raised_since(now - chrono::Duration::days(1)).await;
                let open_ids: Vec<String> = open.iter().map(|alarm| alarm.id.clone()).collect();
                let mut lines: Vec<AlarmLine> = open
                    .into_iter()
                    .map(|alarm| (alarm, true))
                    .chain(
                        recent
                            .into_iter()
                            .filter(|alarm| !open_ids.contains(&alarm.id))
                            .filter(|alarm| alarm.urgency == Urgency::P2)
                            .map(|alarm| (alarm, false)),
                    )
                    .map(|(alarm, open)| AlarmLine {
                        urgency: match alarm.urgency {
                            Urgency::P1 => Pill::new("Urgent", Tone::Bad),
                            Urgency::P2 => Pill::new("Within the hour", Tone::Waiting),
                            _ => Pill::new("For the review", Tone::Neutral),
                        },
                        text: alarm.summary.clone(),
                        raised_at: alarm.raised_at,
                        id: alarm.id,
                        can_acknowledge: open,
                    })
                    .collect();
                lines.sort_by_key(|line| std::cmp::Reverse(line.raised_at));
                lines
            }
            None => Vec::new(),
        };

        Ok(SiteViewModel {
            name: self.site_name().await,
            origin: self.inner.site.origin.clone(),
            launched: !releases.is_empty(),
            status,
            releases,
            kpis,
            jobs,
            signals,
            wallet,
            held,
            alarms,
            owner,
        })
    }

    /// Carries out an owner's `command` on the site view (spec 3.5 §7.11, §7.8.3, A-I13,
    /// A-I23, A-I27).
    ///
    /// # Errors
    ///
    /// `Forbidden` for a person who is not an owner; the ladder's, the rule store's and the
    /// update engine's errors.
    pub async fn site_command(&self, person: &str, command: SiteCommand) -> Result<SiteOutcome> {
        let note = match command {
            SiteCommand::Promote { job } => {
                self.require(person, &[Role::Owner], "Promoting a job").await?;
                let ladder =
                    self.inner.ladder.as_ref().ok_or_else(|| unavailable("The autonomy ladder"))?;
                let rules = self
                    .inner
                    .rules
                    .clone()
                    .ok_or_else(|| unavailable("The standing-rule store"))?;
                let (compiler, _) = self.compiler().await?.ok_or_else(|| {
                    invalid("website.site.no_charter", "jobs are promoted under a signed charter")
                })?;
                // Autonomy is earned (D-7, §7.11): the owner accepts an offer the job's record
                // made.
                let eligibility =
                    ladder.eligibility(self.inner.site.id, &job).await.map_err(kernel)?;
                if !eligibility.eligible {
                    return Err(invalid(
                        "website.site.not_earned",
                        format!(
                            "{job} has {} of the {} unchanged approvals in a row a promotion needs",
                            eligibility.streak.min(eligibility.needed),
                            eligibility.needed
                        ),
                    ));
                }
                let decision = OwnerDecision::new(
                    self.next_id("promote"),
                    SubjectRef::human(person),
                    self.now(),
                );
                let promotion =
                    promote_job(ladder, &compiler, rules.as_ref(), &job, &decision, person).await?;
                crate::observability::record_autonomy_change(&job, None, promotion.job.level);
                format!(
                    "{} now works at \u{201c}{}\u{201d}, by your decision.",
                    crate::jobs::job(&job).map_or(job.as_str(), |s| s.title),
                    words::level(promotion.job.level)
                )
            }
            SiteCommand::Lower { job, reason } => {
                self.require(person, &[Role::Owner], "Lowering a job").await?;
                let ladder =
                    self.inner.ladder.as_ref().ok_or_else(|| unavailable("The autonomy ladder"))?;
                let reason = if reason.trim().is_empty() {
                    "the owner lowered it".to_string()
                } else {
                    reason.trim().to_string()
                };
                let lowered = ladder
                    .lower(self.inner.site.id, &job, &format!("{person}: {reason}"))
                    .await
                    .map_err(kernel)?
                    .ok_or_else(|| invalid("website.site.bottom", "the job only looks already"))?;
                if let (Some(rules), Some((compiler, _))) =
                    (&self.inner.rules, self.compiler().await?)
                {
                    apply_demotion(&compiler, rules.as_ref(), &job, lowered.level).await?;
                }
                format!(
                    "{} now works at \u{201c}{}\u{201d}.",
                    crate::jobs::job(&job).map_or(job.as_str(), |s| s.title),
                    words::level(lowered.level)
                )
            }
            SiteCommand::Item { id, decision } => {
                self.require(person, &[Role::Owner], "Deciding a held item").await?;
                let engine =
                    self.inner.updates.as_ref().ok_or_else(|| unavailable("The update engine"))?;
                let records = engine
                    .decide_item(&ContentId(id), decision.into(), person)
                    .await
                    .map_err(site)?;
                match decision {
                    HeldDecision::Discard => "Discarded; nothing from it is published.".into(),
                    HeldDecision::Release => match records.iter().find_map(|r| r.release_seq) {
                        Some(seq) => format!("Released in release {seq}."),
                        None => "Released; it goes out with the next batch.".into(),
                    },
                }
            }
            SiteCommand::Delivery { delivery, decision } => {
                self.require(person, &[Role::Owner], "Deciding a held delivery").await?;
                let engine =
                    self.inner.updates.as_ref().ok_or_else(|| unavailable("The update engine"))?;
                let outcome = engine
                    .decide_delivery(delivery, decision.into(), person)
                    .await
                    .map_err(site)?;
                match decision {
                    HeldDecision::Discard => "The whole delivery is discarded.".into(),
                    HeldDecision::Release => {
                        format!("Applied: {} changes queued.", outcome.queued + outcome.live)
                    }
                }
            }
            SiteCommand::Acknowledge { alarm } => {
                if self.roles(person).await?.is_empty() {
                    return Err(super::forbidden(
                        "website.ui.not_allowed",
                        "only people with a role acknowledge alarms",
                    ));
                }
                let router = self.inner.alarms.as_ref().ok_or_else(|| unavailable("Alarms"))?;
                router.acknowledge(&alarm, &SubjectRef::human(person)).await.map_err(|_| {
                    not_found("website.site.alarm", format!("no open alarm '{alarm}'"))
                })?;
                "Acknowledged.".into()
            }
        };
        Ok(SiteOutcome { view: self.site_view(person).await?, note: Some(note) })
    }
}

impl From<HeldDecision> for Decision {
    fn from(decision: HeldDecision) -> Self {
        match decision {
            HeldDecision::Release => Decision::Release,
            HeldDecision::Discard => Decision::Discard,
        }
    }
}

fn category_key(category: SpendCategory) -> &'static str {
    match category {
        SpendCategory::Hosting => "hosting",
        SpendCategory::Models => "models",
        SpendCategory::Advertising => "advertising",
        SpendCategory::Domains => "domains",
        _ => "other",
    }
}

/// `drift_detected` → "Drift detected".
fn sentence(code: &str) -> String {
    let text = code.replace(['_', '-', '.'], " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: char) -> FileEntry {
        serde_json::from_value(serde_json::json!({
            "sha256": format!("sha256:{}", sha.to_string().repeat(64)),
            "size": 1,
            "contentType": "text/html"
        }))
        .unwrap()
    }

    #[test]
    fn a_release_lists_the_pages_it_changed() {
        let before = BTreeMap::from([
            ("/index.html".to_string(), entry('a')),
            ("/old/index.html".to_string(), entry('b')),
            ("/sitemap.xml".to_string(), entry('c')),
        ]);
        let after = BTreeMap::from([
            ("/index.html".to_string(), entry('d')),
            ("/new/index.html".to_string(), entry('e')),
            ("/sitemap.xml".to_string(), entry('f')),
        ]);
        let (pages, other) = page_changes(&before, &after);
        let words: Vec<(&str, &str)> =
            pages.iter().map(|p| (p.path.as_str(), p.change.text.as_str())).collect();
        assert_eq!(
            words,
            [
                ("/index.html", "Changed"),
                ("/new/index.html", "New page"),
                ("/old/index.html", "Removed")
            ]
        );
        assert_eq!(other, 1);
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(sentence("drift_detected"), "Drift detected");
        assert_eq!(
            source_words(&ReleaseSource::Rollback { to_seq: 3 }),
            "Rolled back to release 3"
        );
    }
}
