//! The weekly review (spec 3.5 §7.21, §7.11, §7.20 "Weekly review and reports").
//!
//! The review holds KPIs against baselines and targets with trends; what each agent did;
//! cost, revenue and profit and loss; experiments and their evidence; incidents; leads handed
//! off; promotion offers and demotions; P3 alarms; and a short list of questions. Every
//! number comes from a store — the KPI tracker, the kernel's autonomy ladder, the wallet, the
//! incident path, the alarm router — and the rest from the jobs' own records, passed in as
//! [`ReviewInputs`]. It is delivered on the web home and as a document, and summarised on
//! the owner's channel ([`ReviewComposer::send`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{AdkError, Result};
use adk_kernel::channels::ChannelSink;
use adk_kernel::charter::{
    AutonomyChange, AutonomyLadder, CharterStore, KpiPhase, KpiReview, KpiSource, KpiTracker,
    PromotionEligibility, ladder_rank,
};
use adk_payments::wallet::CommerceOutcomes;
use awp_types::Budget;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::alarms::{Alarm, AlarmRouter};
use crate::audit;
use crate::incident::{Incident, IncidentPath};
use crate::team::AgentRole;
use crate::wallet::WalletKpi;

/// Which way a KPI moved over the week, in its direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trend {
    /// Up on the week before.
    Up,
    /// Down on the week before.
    Down,
    /// Unchanged.
    Flat,
    /// No week before to compare.
    Unknown,
}

/// One KPI's line in the review (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KpiLine {
    /// The tracker's week.
    #[serde(flatten)]
    pub review: KpiReview,
    /// The trend on the week before.
    pub trend: Trend,
}

/// Cost, revenue and profit and loss (spec 3.5 §7.14, §7.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    /// The week's settled spend, in major units.
    pub spend: f64,
    /// The week's spend per category.
    pub by_category: BTreeMap<String, f64>,
    /// The week's revenue, when the wallet has commerce outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revenue: Option<f64>,
    /// Revenue less spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profit: Option<f64>,
    /// Spend per lead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_lead: Option<f64>,
    /// Spend per sale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_sale: Option<f64>,
    /// The month's spend to the end of the week.
    pub month_to_date: f64,
    /// The charter's monthly budget, in major units.
    pub monthly_budget: f64,
    /// Whether the month's spend is inside the budget.
    pub within_budget: bool,
}

/// What the jobs recorded that no store keeps (spec 3.5 §7.21).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInputs {
    /// What each agent did.
    pub work: BTreeMap<AgentRole, Vec<String>>,
    /// Experiments and their evidence.
    pub experiments: Vec<String>,
    /// Leads handed off to the owner.
    pub handed_off: Vec<String>,
    /// Questions for the owner.
    pub questions: Vec<String>,
}

/// The weekly review (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyReview {
    /// The site.
    pub site: Uuid,
    /// The Monday the week starts.
    pub week_start: NaiveDate,
    /// The charter version in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter_version: Option<u32>,
    /// KPIs against baselines and targets, with trends.
    pub kpis: Vec<KpiLine>,
    /// What each agent did.
    pub work: BTreeMap<AgentRole, Vec<String>>,
    /// Cost, revenue and profit and loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub money: Option<Money>,
    /// Experiments and their evidence.
    pub experiments: Vec<String>,
    /// The week's incidents.
    pub incidents: Vec<Incident>,
    /// Leads captured in the week.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leads: Option<u64>,
    /// Leads handed off.
    pub handed_off: Vec<String>,
    /// The promotions the jobs earned, for the owner to decide (A-I13).
    pub promotions: Vec<PromotionEligibility>,
    /// The week's demotions, with their reasons.
    pub demotions: Vec<AutonomyChange>,
    /// Alarms that waited for the review.
    pub alarms: Vec<Alarm>,
    /// Questions for the owner.
    pub questions: Vec<String>,
}

/// The ISO 4217 minor-unit exponent of `currency`.
fn minor_unit_exponent(currency: &str) -> i32 {
    const ZERO: &[&str] = &[
        "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VND", "VUV",
        "XAF", "XOF", "XPF",
    ];
    const THREE: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];
    let currency = currency.to_ascii_uppercase();
    if ZERO.contains(&currency.as_str()) {
        0
    } else if THREE.contains(&currency.as_str()) {
        3
    } else {
        2
    }
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |v| format!("{v:.2}"))
}

impl WeeklyReview {
    /// The review as a Markdown document for the web home and as a file.
    pub fn to_markdown(&self) -> String {
        let mut out = format!("# Weekly review — week of {}\n\n", self.week_start);
        out.push_str("## KPIs\n\n| KPI | This week | Last week | Baseline | Target | Trend |\n|---|---|---|---|---|---|\n");
        for line in &self.kpis {
            let k = &line.review;
            let baseline = match (&k.phase, k.baseline) {
                (KpiPhase::MeasureOnly { until }, None) => {
                    format!("measuring until {}", until.date_naive())
                }
                (_, baseline) => number(baseline),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {baseline} | {} | {:?} |",
                k.name,
                number(k.week),
                number(k.previous_week),
                number(k.target),
                line.trend
            );
        }
        out.push_str("\n## Work\n\n");
        for (agent, items) in &self.work {
            for item in items {
                let _ = writeln!(out, "- **{agent}**: {item}");
            }
        }
        if let Some(money) = &self.money {
            let _ = write!(
                out,
                "\n## Money\n\n- Spend this week: {:.2}\n- Month to date: {:.2} of a {:.2} budget ({})\n",
                money.spend,
                money.month_to_date,
                money.monthly_budget,
                if money.within_budget { "within budget" } else { "over budget" }
            );
            for (category, spend) in &money.by_category {
                let _ = writeln!(out, "- {category}: {spend:.2}");
            }
            if let Some(profit) = money.profit {
                let _ = writeln!(
                    out,
                    "- Revenue {}, profit {profit:.2}, cost per lead {}, cost per sale {}",
                    number(money.revenue),
                    number(money.cost_per_lead),
                    number(money.cost_per_sale)
                );
            }
        }
        let list = |out: &mut String, title: &str, items: Vec<String>| {
            if !items.is_empty() {
                let _ = writeln!(out, "\n## {title}\n");
                for item in items {
                    let _ = writeln!(out, "- {item}");
                }
            }
        };
        list(&mut out, "Experiments", self.experiments.clone());
        list(
            &mut out,
            "Incidents",
            self.incidents
                .iter()
                .map(|i| {
                    let state = if i.closed.is_some() { "closed" } else { "open" };
                    format!(
                        "{} ({state}): {} file(s) drifted; rollback to release {}",
                        i.id,
                        i.drift.len(),
                        i.rollback.to_release
                    )
                })
                .collect(),
        );
        let mut leads = self.handed_off.clone();
        if let Some(count) = self.leads {
            leads.insert(0, format!("{count} captured this week"));
        }
        list(&mut out, "Leads", leads);
        list(
            &mut out,
            "Autonomy",
            self.promotions
                .iter()
                .map(|p| {
                    format!(
                        "Offer: promote {} to {:?} after {} unchanged approvals — your decision",
                        p.job,
                        p.next.unwrap_or(p.level),
                        p.streak
                    )
                })
                .chain(
                    self.demotions.iter().map(|d| {
                        format!("Lowered {} to {:?}: {}", d.job, d.to, d.reason.describe())
                    }),
                )
                .collect(),
        );
        list(&mut out, "Alarms", self.alarms.iter().map(|a| a.summary.clone()).collect());
        list(&mut out, "Questions for you", self.questions.clone());
        out
    }

    /// One line for the owner's channel.
    pub fn summary(&self) -> String {
        let moved = self.kpis.iter().filter(|k| k.trend == Trend::Up).count();
        let mut parts =
            vec![format!("Week of {}: {moved} of {} KPIs up", self.week_start, self.kpis.len())];
        if let Some(money) = &self.money {
            parts.push(format!("spend {:.2}", money.spend));
        }
        if !self.incidents.is_empty() {
            parts.push(format!("{} incident(s)", self.incidents.len()));
        }
        if !self.promotions.is_empty() {
            parts.push(format!("{} promotion offer(s)", self.promotions.len()));
        }
        if !self.questions.is_empty() {
            parts.push(format!("{} question(s) for you", self.questions.len()));
        }
        parts.join("; ")
    }
}

/// Composes and sends the weekly review (spec 3.5 §7.21).
pub struct ReviewComposer {
    site: Uuid,
    tracker: KpiTracker,
    ladder: AutonomyLadder,
    charters: Arc<dyn CharterStore>,
    wallet: Option<Arc<WalletKpi>>,
    outcomes: Option<Arc<dyn CommerceOutcomes>>,
    incidents: Option<Arc<IncidentPath>>,
    alarms: Option<Arc<AlarmRouter>>,
    audit: Option<AuditTrail>,
}

impl ReviewComposer {
    /// A composer for `site` over the KPI tracker, the kernel's ladder and the charter store.
    pub fn new(
        site: Uuid,
        tracker: KpiTracker,
        ladder: AutonomyLadder,
        charters: Arc<dyn CharterStore>,
    ) -> Self {
        Self {
            site,
            tracker,
            ladder,
            charters,
            wallet: None,
            outcomes: None,
            incidents: None,
            alarms: None,
            audit: None,
        }
    }

    /// Reports cost, revenue and profit from the wallet.
    #[must_use]
    pub fn with_wallet(mut self, wallet: Arc<WalletKpi>) -> Self {
        self.wallet = Some(wallet);
        self
    }

    /// Reports the leads captured, from the site's commerce outcomes.
    #[must_use]
    pub fn with_outcomes(mut self, outcomes: Arc<dyn CommerceOutcomes>) -> Self {
        self.outcomes = Some(outcomes);
        self
    }

    /// Reports the incident path's incidents.
    #[must_use]
    pub fn with_incidents(mut self, incidents: Arc<IncidentPath>) -> Self {
        self.incidents = Some(incidents);
        self
    }

    /// Carries the alarms that waited for the review.
    #[must_use]
    pub fn with_alarms(mut self, alarms: Arc<AlarmRouter>) -> Self {
        self.alarms = Some(alarms);
        self
    }

    /// Where `website.review.sent` goes.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    async fn money(
        &self,
        wallet: &WalletKpi,
        budget: &Budget,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Money> {
        let read = |metric: &'static str, from: DateTime<Utc>| async move {
            wallet
                .read(self.site, metric, from, to)
                .await
                .map_err(|error| AdkError::config(error.to_string()))
        };
        let mut by_category = BTreeMap::new();
        for (category, metric) in [
            ("hosting", "spend.hosting"),
            ("models", "spend.models"),
            ("advertising", "spend.advertising"),
            ("domains", "spend.domains"),
            ("other", "spend.other"),
        ] {
            if let Some(value) = read(metric, from).await?.filter(|value| *value > 0.0) {
                by_category.insert(category.to_string(), value);
            }
        }
        let month_start = to
            .date_naive()
            .with_day(1)
            .and_then(|day| day.and_hms_opt(0, 0, 0))
            .map_or(from, |start| start.and_utc());
        let month_to_date = read("spend", month_start).await?.unwrap_or(0.0);
        // Budget caps are minor units; the wallet reports major units.
        let monthly_budget = budget.caps.values().map(|cap| *cap as f64).sum::<f64>()
            / 10f64.powi(minor_unit_exponent(&budget.currency));
        Ok(Money {
            spend: read("spend", from).await?.unwrap_or(0.0),
            by_category,
            revenue: read("revenue", from).await?,
            profit: read("profit", from).await?,
            cost_per_lead: read("cost_per_lead", from).await?,
            cost_per_sale: read("cost_per_sale", from).await?,
            month_to_date,
            monthly_budget,
            within_budget: month_to_date <= monthly_budget,
        })
    }

    /// Composes the review of the week starting `week_start` (a Monday) with what the jobs
    /// recorded in `inputs` (spec 3.5 §7.21).
    ///
    /// # Errors
    ///
    /// Returns a store's error.
    #[tracing::instrument(skip_all, name = "website.job.weekly-review", fields(site.id = %self.site, week = %week_start))]
    pub async fn compose(
        &self,
        week_start: NaiveDate,
        inputs: ReviewInputs,
    ) -> Result<WeeklyReview> {
        let store = |e: adk_kernel::KernelError| AdkError::config(e.to_string());
        let from = week_start
            .and_hms_opt(0, 0, 0)
            .map(|t| t.and_utc())
            .ok_or_else(|| AdkError::config("invalid week start"))?;
        let to = from + Duration::days(7);
        let current = self.charters.current(self.site).await.map_err(store)?;
        let kpis = self
            .tracker
            .review(week_start)
            .await
            .map_err(store)?
            .into_iter()
            .map(|review| {
                let direction = current.as_ref().and_then(|c| {
                    c.charter.kpis.iter().find(|k| k.id == review.kpi).map(|k| k.direction)
                });
                let trend = match (review.week, review.previous_week) {
                    (Some(week), Some(previous)) if (week - previous).abs() < f64::EPSILON => {
                        Trend::Flat
                    }
                    (Some(week), Some(previous)) => {
                        let up = week > previous;
                        let better = matches!(direction, Some(awp_types::Direction::Down)) != up;
                        if better { Trend::Up } else { Trend::Down }
                    }
                    _ => Trend::Unknown,
                };
                KpiLine { review, trend }
            })
            .collect();
        let promotions = self.ladder.offers(self.site).await.map_err(store)?;
        let mut demotions = Vec::new();
        for job in self.ladder.store().list(self.site).await.map_err(store)? {
            for change in self.ladder.store().history(self.site, &job.job).await.map_err(store)? {
                let lowered =
                    change.from.is_some_and(|from| ladder_rank(change.to) < ladder_rank(from));
                if lowered && change.at >= from && change.at < to {
                    demotions.push(change);
                }
            }
        }
        let money = match (&self.wallet, &current) {
            (Some(wallet), Some(current)) => {
                Some(self.money(wallet, &current.charter.budget, from, to).await?)
            }
            _ => None,
        };
        let leads = match &self.outcomes {
            Some(outcomes) => Some(outcomes.leads(from, to).await?),
            None => None,
        };
        let incidents = match &self.incidents {
            Some(path) => path
                .incidents()
                .await
                .into_iter()
                .filter(|i| i.opened_at < to && i.closed.as_ref().is_none_or(|c| c.at >= from))
                .collect(),
            None => Vec::new(),
        };
        let alarms = match &self.alarms {
            Some(router) => router.take_review_items().await,
            None => Vec::new(),
        };
        Ok(WeeklyReview {
            site: self.site,
            week_start,
            charter_version: current.map(|c| c.version()),
            kpis,
            work: inputs.work,
            money,
            experiments: inputs.experiments,
            incidents,
            leads,
            handed_off: inputs.handed_off,
            promotions,
            demotions,
            alarms,
            questions: inputs.questions,
        })
    }

    /// Sends `review`'s summary to the owner's `thread` on `sink` and records
    /// `website.review.sent`; returns the document for the web home.
    ///
    /// # Errors
    ///
    /// Returns the channel's error.
    pub async fn send(
        &self,
        review: &WeeklyReview,
        sink: &dyn ChannelSink,
        thread: &str,
    ) -> Result<String> {
        sink.send_message(thread, &review.summary())
            .await
            .map_err(|error| AdkError::config(error.to_string()))?;
        audit::record(
            self.audit.as_ref(),
            audit::REVIEW_SENT,
            AuditActor::default(),
            json!({
                "site": self.site,
                "week": review.week_start,
                "charterVersion": review.charter_version,
                "channel": sink.channel(),
            }),
        )
        .await;
        Ok(review.to_markdown())
    }
}
