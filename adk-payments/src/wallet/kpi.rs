//! Profit and loss, cost per lead and cost per sale as KPI sources (spec 3.5 §7.11,
//! §7.12, §7.14).

use std::sync::{Arc, Mutex};

use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::engine::Wallet;
use super::error::{WALLET_WRONG_SITE_CODE, refused};
use super::policy::{SpendCategory, category_name};

/// Total settled spend, in major units of the wallet's currency.
pub const SPEND_METRIC: &str = "spend";
/// Revenue, in major units.
pub const REVENUE_METRIC: &str = "revenue";
/// Revenue less spend, in major units.
pub const PROFIT_METRIC: &str = "profit";
/// Spend per lead, in major units.
pub const COST_PER_LEAD_METRIC: &str = "cost_per_lead";
/// Spend per sale, in major units.
pub const COST_PER_SALE_METRIC: &str = "cost_per_sale";

/// The commerce side of profit and loss: what the site earned and the leads and sales it
/// produced (spec 3.5 §7.12: the commerce ledger and the lead records).
#[async_trait]
pub trait CommerceOutcomes: Send + Sync {
    /// Revenue in `[from, to)`, in minor units of the wallet's currency.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    async fn revenue(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64>;

    /// Leads in `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    async fn leads(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64>;

    /// Sales in `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    async fn sales(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64>;
}

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Revenue(u64),
    Lead,
    Sale(u64),
}

/// Outcomes in the order they were recorded.
type OutcomeLog = Arc<Mutex<Vec<(DateTime<Utc>, Outcome)>>>;

/// [`CommerceOutcomes`] kept in memory, for tests and fixtures.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::InMemoryCommerceOutcomes;
///
/// let outcomes = InMemoryCommerceOutcomes::new();
/// outcomes.record_lead(chrono::Utc::now());
/// outcomes.record_sale(chrono::Utc::now(), 4_900);
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryCommerceOutcomes {
    events: OutcomeLog,
}

impl InMemoryCommerceOutcomes {
    /// No outcomes yet.
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, at: DateTime<Utc>, outcome: Outcome) {
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((at, outcome));
    }

    /// Records a lead at `at`.
    pub fn record_lead(&self, at: DateTime<Utc>) {
        self.push(at, Outcome::Lead);
    }

    /// Records a sale of `amount` minor units at `at`; it counts as revenue too.
    pub fn record_sale(&self, at: DateTime<Utc>, amount: u64) {
        self.push(at, Outcome::Sale(amount));
    }

    /// Records revenue that is not a sale, such as a donation, at `at`.
    pub fn record_revenue(&self, at: DateTime<Utc>, amount: u64) {
        self.push(at, Outcome::Revenue(amount));
    }

    fn sum(&self, from: DateTime<Utc>, to: DateTime<Utc>, pick: impl Fn(Outcome) -> u64) -> u64 {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(at, _)| *at >= from && *at < to)
            .fold(0u64, |total, (_, outcome)| total.saturating_add(pick(*outcome)))
    }
}

#[async_trait]
impl CommerceOutcomes for InMemoryCommerceOutcomes {
    async fn revenue(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64> {
        Ok(self.sum(from, to, |outcome| match outcome {
            Outcome::Revenue(amount) | Outcome::Sale(amount) => amount,
            Outcome::Lead => 0,
        }))
    }

    async fn leads(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64> {
        Ok(self.sum(from, to, |outcome| u64::from(matches!(outcome, Outcome::Lead))))
    }

    async fn sales(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64> {
        Ok(self.sum(from, to, |outcome| u64::from(matches!(outcome, Outcome::Sale(_)))))
    }
}

/// The wallet as a KPI source for the KPI tracker (spec 3.5 §7.11, §7.12, §7.14): the
/// `spend` source.
///
/// | Metric | Value over `[from, to)`, in major units | Needs |
/// |--------|-------------------------------------------|-------|
/// | `spend` | settled spend | — |
/// | `spend.{category}` | settled spend in the category, such as `spend.models` | — |
/// | `revenue` | revenue | [`CommerceOutcomes`] |
/// | `profit` | revenue less spend | [`CommerceOutcomes`] |
/// | `cost_per_lead` | spend per lead; none without leads | [`CommerceOutcomes`] |
/// | `cost_per_sale` | spend per sale; none without sales | [`CommerceOutcomes`] |
///
/// [`read`](Self::read) has the signature of the kernel's `KpiSource::read`, so the
/// tracker's adapter forwards to it. A wallet answers only for its own site (A-I22).
///
/// # Example
///
/// ```rust,ignore
/// let source = WalletKpiSource::new(wallet, site_id).with_outcomes(Arc::new(outcomes));
/// let profit = source.read(site_id, "profit", month_start, month_end).await?;
/// ```
pub struct WalletKpiSource {
    wallet: Wallet,
    site: Uuid,
    outcomes: Option<Arc<dyn CommerceOutcomes>>,
    name: String,
}

impl WalletKpiSource {
    /// The `spend` source for `wallet`, which serves `site`.
    pub fn new(wallet: Wallet, site: Uuid) -> Self {
        Self { wallet, site, outcomes: None, name: "spend".to_string() }
    }

    /// Adds revenue, leads and sales for `revenue`, `profit` and the costs per outcome.
    #[must_use]
    pub fn with_outcomes(mut self, outcomes: Arc<dyn CommerceOutcomes>) -> Self {
        self.outcomes = Some(outcomes);
        self
    }

    /// Answers under another source name, such as `commerce`.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The source's name, as KPIs name it in their `source`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The value of `metric` for `site` over `[from, to)`; `None` for a metric the source
    /// cannot compute, such as a cost per lead with no leads.
    ///
    /// # Errors
    ///
    /// Returns `wallet.wrong_site` for another site's request, or the wallet's or
    /// outcomes' error.
    pub async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>> {
        if site != self.site {
            return Err(refused(
                WALLET_WRONG_SITE_CODE,
                format!("wallet {} serves another site than {site}", self.wallet.id()),
            ));
        }
        let policy = self.wallet.policy();
        let spend = |category| self.wallet.spent_between(category, from, to);
        if metric == SPEND_METRIC {
            return Ok(Some(policy.to_major(spend(None).await?)));
        }
        if let Some(name) = metric.strip_prefix("spend.") {
            let category: Option<SpendCategory> =
                serde_json::from_value(serde_json::Value::String(name.to_string())).ok();
            return match category {
                Some(category) if category_name(category) == name => {
                    Ok(Some(policy.to_major(spend(Some(category)).await?)))
                }
                _ => Ok(None),
            };
        }
        let Some(outcomes) = &self.outcomes else {
            return Ok(None);
        };
        let total = spend(None).await?;
        let per = |count: u64| (count > 0).then(|| policy.to_major(total) / count as f64);
        Ok(match metric {
            REVENUE_METRIC => Some(policy.to_major(outcomes.revenue(from, to).await?)),
            PROFIT_METRIC => {
                let revenue = outcomes.revenue(from, to).await?;
                Some(policy.to_major(revenue) - policy.to_major(total))
            }
            COST_PER_LEAD_METRIC => per(outcomes.leads(from, to).await?),
            COST_PER_SALE_METRIC => per(outcomes.sales(from, to).await?),
            _ => None,
        })
    }
}
