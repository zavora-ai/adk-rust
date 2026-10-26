//! Observability (spec 3.5 §7.24): spans, metrics and the site view.
//!
//! | Span | Emitted by |
//! |---|---|
//! | `website.onboarding.{stage}` | [`crate::onboarding::Stage::span`], [`crate::onboarding::OnboardingProgress`] |
//! | `site.probe` | [`crate::incident::GuardianProbe::run`] |
//! | `website.job.{job}` | the jobs' runs: `website.job.protect` (incident path), `website.job.weekly-review` (review composer) |
//! | `site.release` | `adk-site`'s pipeline: every release, with its sequence, source and items |
//! | `kernel.signal` | [`crate::edge::adapters::KernelSignals`]: every signal the edge files, with its kind, id and emitter |
//! | `awp.request`, `awp.capability.call` | the agent edge |
//!
//! Metrics are `tracing` events whose fields follow the OpenTelemetry metrics layer's
//! prefixes (`histogram.`, `counter.`, `monotonic_counter.`), so `adk-telemetry`'s export
//! turns them into instruments: every KPI source's readings, time to detect and restore,
//! held decisions and their latency, autonomy changes, and spend by category. The site view
//! ([`SiteView`]) is what every surface shows: status, release, KPIs, jobs and their levels,
//! signals and the wallet.

use std::sync::Arc;

use adk_core::{AdkError, Result};
use adk_harness::website::SiteRuntime;
use adk_kernel::PartitionId;
use adk_kernel::charter::{
    AutonomyStore, KpiReview, KpiSource, KpiTracker, SignalFilter, SignalStore,
};
use awp_types::AutonomyLevel;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

use crate::incident::{Incident, StatusRecord, StatusSink};
use crate::wallet::WalletKpi;

/// The site's spans (spec 3.5 §7.24).
pub mod spans {
    /// Prefix of the onboarding stages' spans.
    pub const ONBOARDING: &str = "website.onboarding";
    /// A release: `adk-site`'s pipeline span.
    pub const SITE_RELEASE: &str = adk_site::pipeline::RELEASE_SPAN;
    /// A probe.
    pub const SITE_PROBE: &str = "site.probe";
    /// A request at the edge.
    pub const AWP_REQUEST: &str = "awp.request";
    /// A capability call at the edge.
    pub const AWP_CAPABILITY_CALL: &str = "awp.capability.call";
    /// A signal filed in the back office.
    pub const KERNEL_SIGNAL: &str = "kernel.signal";
    /// Prefix of the jobs' spans.
    pub const JOB: &str = "website.job";
}

/// Records one KPI reading from `source` (spec 3.5 §7.24 "metrics for every KPI source").
pub fn record_kpi(source: &str, kpi: &str, value: f64) {
    tracing::info!(
        website.kpi.source = source,
        website.kpi = kpi,
        histogram.website.kpi.value = value,
        "website.kpi"
    );
}

/// Records an incident's time to detect and, once rolled back, to restore, in milliseconds
/// (spec 3.5 §7.24).
pub fn record_incident(
    incident: &Incident,
    drift_since: DateTime<Utc>,
    restored_at: Option<DateTime<Utc>>,
) {
    let detect = (incident.opened_at - drift_since).num_milliseconds().max(0);
    tracing::info!(website.incident = %incident.id, histogram.website.incident.time_to_detect_ms = detect, "website.incident.detected");
    if let Some(restored) = restored_at {
        let restore = (restored - incident.opened_at).num_milliseconds().max(0);
        tracing::info!(website.incident = %incident.id, histogram.website.incident.time_to_restore_ms = restore, "website.incident.restored");
    }
}

/// Records a held decision and, once answered, its latency (spec 3.5 §7.24).
pub fn record_held_decision(tool: &str, latency: Option<chrono::Duration>) {
    tracing::info!(
        website.tool = tool,
        monotonic_counter.website.decisions.held = 1u64,
        "website.decision.held"
    );
    if let Some(latency) = latency {
        tracing::info!(
            website.tool = tool,
            histogram.website.decisions.latency_ms = latency.num_milliseconds().max(0),
            "website.decision.answered"
        );
    }
}

/// Records an autonomy change (spec 3.5 §7.24).
pub fn record_autonomy_change(job: &str, from: Option<AutonomyLevel>, to: AutonomyLevel) {
    tracing::info!(website.job = job, from = ?from, to = ?to, monotonic_counter.website.autonomy.changes = 1u64, "website.autonomy.changed");
}

/// Records spend in a category, in major units (spec 3.5 §7.24).
pub fn record_spend(category: &str, amount: f64) {
    tracing::info!(
        website.spend.category = category,
        counter.website.spend = amount,
        "website.spend"
    );
}

/// What every surface shows of a site (spec 3.5 §7.24).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteView {
    /// The status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusRecord>,
    /// The release served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<u64>,
    /// This week's KPIs.
    pub kpis: Vec<KpiReview>,
    /// Each job and its level.
    pub jobs: Vec<(String, AutonomyLevel)>,
    /// Signals waiting in the back office.
    pub open_signals: usize,
    /// The month's spend, in major units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_this_month: Option<f64>,
}

/// What [`SiteView::read`] reads.
pub struct SiteViewSources {
    /// The site runtime.
    pub runtime: Arc<SiteRuntime>,
    /// The status.
    pub status: Arc<dyn StatusSink>,
    /// The KPI tracker.
    pub tracker: KpiTracker,
    /// The jobs' levels.
    pub autonomy: Arc<dyn AutonomyStore>,
    /// The signals and the back office's partition.
    pub signals: (Arc<dyn SignalStore>, PartitionId),
    /// The wallet, when the site has one.
    pub wallet: Option<Arc<WalletKpi>>,
}

impl SiteView {
    /// Reads the view at `now`.
    ///
    /// # Errors
    ///
    /// Returns a store's error.
    pub async fn read(sources: &SiteViewSources, now: DateTime<Utc>) -> Result<Self> {
        let store = |e: adk_kernel::KernelError| AdkError::config(e.to_string());
        let site = sources.runtime.site().id;
        let today = now.date_naive();
        let monday =
            today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()));
        let jobs = sources
            .autonomy
            .list(site)
            .await
            .map_err(store)?
            .into_iter()
            .map(|job| (job.job, job.level))
            .collect();
        let (signals, partition) = &sources.signals;
        let open_signals = signals
            .list(&SignalFilter::default().partition(partition.clone()))
            .await
            .map_err(store)?
            .iter()
            .filter(|record| record.open)
            .count();
        let spend_this_month = match &sources.wallet {
            Some(wallet) => {
                let start = today
                    .with_day(1)
                    .and_then(|day| day.and_hms_opt(0, 0, 0))
                    .map_or(now, |start| start.and_utc());
                wallet.read(site, "spend", start, now).await.map_err(store)?
            }
            None => None,
        };
        Ok(Self {
            status: sources.status.get(site).await?,
            release: sources.runtime.pipeline().previous().await.map(|p| p.manifest.seq),
            kpis: sources.tracker.review(monday).await.map_err(store)?,
            jobs,
            open_signals,
            spend_this_month,
        })
    }
}
