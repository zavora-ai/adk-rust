//! The commerce, action and spend ledgers as KPI sources (spec 3.5 §7.12, §7.14): metering
//! wrappers count what each ledger records per day, and the sources read the counts.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, Clock, ReservationId, Result, SpendKey, SpendLedger,
    SpendPeriod, SystemClock, is_not_run,
};
use adk_kernel::KernelError;
use adk_kernel::charter::KpiSource;
use adk_payments::wallet::CommerceOutcomes;
use adk_server::awp::BeaconStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{check_site, total};

/// Adds `count` to `metrics` on today's date, logging a failure: metering never fails the
/// call it counts.
async fn meter(store: &dyn BeaconStore, clock: &dyn Clock, metrics: &[String], count: u64) {
    let day = clock.now().date_naive();
    for metric in metrics {
        if let Err(error) = store.add(day, metric, count).await {
            tracing::warn!(metric = %metric, error = %error, "a ledger count was not recorded");
        }
    }
}

/// An action ledger that counts each completed call per day (spec 3.5 §7.12): give it to the
/// edge as its action ledger (`AwpEdge::builder(..).action_ledger(..)`) and every
/// `non_idempotent` capability call — an order, a booking, a quote — is counted under its
/// capability and outcome for the `commerce` source. It forwards every call to the ledger it
/// wraps, so the exactly-once guarantee is the inner ledger's (A-I7).
///
/// Outcomes are `completed`, `failed`, and `not_run` for a call the kill switch refused.
#[derive(Clone)]
pub struct MeteredActionLedger {
    inner: Arc<dyn ActionLedger>,
    counts: Arc<dyn BeaconStore>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for MeteredActionLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeteredActionLedger").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl MeteredActionLedger {
    /// `inner`, counted in `counts`, on the system clock.
    pub fn new(inner: Arc<dyn ActionLedger>, counts: Arc<dyn BeaconStore>) -> Self {
        Self { inner, counts, clock: Arc::new(SystemClock) }
    }

    /// Dates counts with `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl ActionLedger for MeteredActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let label = match &outcome {
            ActionOutcome::Succeeded { .. } => "completed",
            ActionOutcome::Failed { error } if is_not_run(error) => "not_run",
            ActionOutcome::Failed { .. } => "failed",
            _ => "other",
        };
        self.inner.complete(key, outcome).await?;
        match self.inner.get(key).await {
            Ok(Some(record)) => {
                let tool = record.tool_name;
                let metrics = [
                    "actions".to_string(),
                    format!("actions:{tool}"),
                    format!("actions:{tool}:{label}"),
                ];
                meter(self.counts.as_ref(), self.clock.as_ref(), &metrics, 1).await;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = %error, "the action ledger could not be read to count a call");
            }
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

/// The commerce and action ledgers as the `commerce` KPI source (spec 3.5 §7.12):
///
/// | Metric | Value over the window |
/// |---|---|
/// | `actions`, `actions:{capability}`, `actions:{capability}:{outcome}` | Ledgered capability calls counted by [`MeteredActionLedger`], such as `actions:order:completed` |
/// | `leads`, `sales` | Leads and sales from the site's `CommerceOutcomes`, when set |
///
/// Revenue, profit and the costs per lead and per sale are the wallet's
/// ([`WalletKpi`](crate::wallet::WalletKpi)).
#[derive(Clone)]
pub struct CommerceKpi {
    site: Uuid,
    counts: Arc<dyn BeaconStore>,
    outcomes: Option<Arc<dyn CommerceOutcomes>>,
}

impl std::fmt::Debug for CommerceKpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommerceKpi").field("site", &self.site).finish_non_exhaustive()
    }
}

impl CommerceKpi {
    /// `site`'s ledgered calls in `counts`.
    pub fn new(site: Uuid, counts: Arc<dyn BeaconStore>) -> Self {
        Self { site, counts, outcomes: None }
    }

    /// Reads leads and sales from `outcomes`, such as [`SiteOutcomes`](crate::wallet::SiteOutcomes).
    #[must_use]
    pub fn with_outcomes(mut self, outcomes: Arc<dyn CommerceOutcomes>) -> Self {
        self.outcomes = Some(outcomes);
        self
    }
}

#[async_trait]
impl KpiSource for CommerceKpi {
    fn name(&self) -> &str {
        "commerce"
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        check_site("commerce", self.site, site)?;
        let outcome = |error: adk_core::AdkError| KernelError::Store {
            backend: "commerce",
            reason: error.to_string(),
        };
        match (metric, &self.outcomes) {
            ("leads", Some(outcomes)) => {
                Ok(Some(outcomes.leads(from, to).await.map_err(outcome)? as f64))
            }
            ("sales", Some(outcomes)) => {
                Ok(Some(outcomes.sales(from, to).await.map_err(outcome)? as f64))
            }
            ("leads" | "sales", None) => Ok(None),
            _ => total("commerce", &self.counts, metric, from, to).await,
        }
    }
}

/// A spend ledger that counts committed spend per day (spec 3.5 §7.12, §7.14): the runtime's
/// model and tool spend, in micro-USD, by vendor and agent, for the `spend` source. Every
/// call goes to the ledger it wraps, so caps are the inner ledger's.
///
/// A reservation made before a restart and committed after it is counted under `spend` only.
pub struct MeteredSpendLedger {
    inner: Arc<dyn SpendLedger>,
    counts: Arc<dyn BeaconStore>,
    clock: Arc<dyn Clock>,
    held: Mutex<HashMap<ReservationId, SpendKey>>,
}

impl std::fmt::Debug for MeteredSpendLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeteredSpendLedger").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl MeteredSpendLedger {
    /// `inner`, counted in `counts`, on the system clock.
    pub fn new(inner: Arc<dyn SpendLedger>, counts: Arc<dyn BeaconStore>) -> Self {
        Self { inner, counts, clock: Arc::new(SystemClock), held: Mutex::new(HashMap::new()) }
    }

    /// Dates counts with `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl SpendLedger for MeteredSpendLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let id = self.inner.reserve(key, amount_micro_usd).await?;
        self.held.lock().await.insert(id.clone(), key.clone());
        Ok(id)
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let key = self.held.lock().await.get(&id).cloned();
        self.inner.commit(id.clone(), actual_micro_usd).await?;
        self.held.lock().await.remove(&id);
        let mut metrics = vec!["spend".to_string()];
        if let Some(key) = key {
            if let Some(vendor) = key.vendor {
                metrics.push(format!("spend.vendor:{vendor}"));
            }
            if let Some(agent) = key.agent {
                metrics.push(format!("spend.agent:{agent}"));
            }
        }
        meter(self.counts.as_ref(), self.clock.as_ref(), &metrics, actual_micro_usd).await;
        Ok(())
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        self.held.lock().await.remove(&id);
        self.inner.release(id).await
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        self.inner.spent(key, period).await
    }
}

/// The spend ledger as the `spend` KPI source (spec 3.5 §7.12, §7.14): `spend`,
/// `spend.vendor:{vendor}` and `spend.agent:{agent}` over the window, in USD, from the counts
/// [`MeteredSpendLedger`] keeps. A site with a wallet reads `spend` from
/// [`WalletKpi`](crate::wallet::WalletKpi) instead, or renames this source with
/// [`named`](Self::named).
#[derive(Clone)]
pub struct SpendLedgerKpi {
    site: Uuid,
    counts: Arc<dyn BeaconStore>,
    name: String,
}

impl std::fmt::Debug for SpendLedgerKpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpendLedgerKpi")
            .field("site", &self.site)
            .field("name", &self.name)
            .finish()
    }
}

impl SpendLedgerKpi {
    /// `site`'s metered spend in `counts`, named `spend`.
    pub fn new(site: Uuid, counts: Arc<dyn BeaconStore>) -> Self {
        Self { site, counts, name: "spend".to_string() }
    }

    /// Answers under another source name, such as `spend_ledger`.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

#[async_trait]
impl KpiSource for SpendLedgerKpi {
    fn name(&self) -> &str {
        &self.name
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        check_site("spend", self.site, site)?;
        if metric != "spend" && !metric.starts_with("spend.") {
            return Ok(None);
        }
        Ok(total("spend", &self.counts, metric, from, to).await?.map(|micro| micro / 1_000_000.0))
    }
}
