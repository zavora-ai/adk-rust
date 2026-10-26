//! The wallet wired into the site (spec 3.5 §7.14, §7.11, §7.12, §7.21).
//!
//! | Part | Wiring |
//! |---|---|
//! | [`WalletKpi`] | `adk-payments`' `WalletKpiSource` as the kernel's `KpiSource`: spend, revenue, profit, cost per lead and per sale for the KPI tracker and the weekly review |
//! | [`SiteOutcomes`] | `CommerceOutcomes` over the site's records: leads from the back office's `lead_captured` signals, revenue and sales from a commerce ledger |
//! | Alarms | `WalletAlarmHook` is [`AlarmRouter`](crate::alarms::AlarmRouter): an anomaly is P1, a reconciliation mismatch P2 |
//! | [`finance_tools`] | The Finance agent's `wallet_pay` and `wallet_status`, which [`TeamKit::wallet`](crate::team::TeamKit::wallet) gives it |

use std::sync::Arc;

use adk_auth::{ScopeGuard, StaticScopeResolver};
use adk_core::{Result, Tool};
use adk_kernel::PartitionId;
use adk_kernel::charter::{KpiSource, SignalFilter, SignalStore};
use adk_payments::auth::PAYMENT_EXECUTE_SCOPE;
use adk_payments::wallet::{CommerceOutcomes, Wallet, WalletKpiSource, WalletToolset};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The Finance agent's wallet tools (spec 3.5 §7.14, §7.19): `wallet_pay` and
/// `wallet_status` over the owner's `wallet`.
///
/// The tools hold the payment scope (`payments:payment:execute`) because the owner gave
/// the Finance agent this wallet: the mandates' caps, limits, payees and rails, the
/// anomaly rule and the governed path's holds decide each payment, and no tool raises a cap,
/// adds a payee or adds a payment method (A-I16).
///
/// # Example
///
/// ```rust,ignore
/// let tools = finance_tools(wallet);
/// let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
/// assert_eq!(names, ["wallet_pay", "wallet_status"]);
/// ```
pub fn finance_tools(wallet: Wallet) -> Vec<Arc<dyn Tool>> {
    let guard = ScopeGuard::new(StaticScopeResolver::new(vec![PAYMENT_EXECUTE_SCOPE.to_string()]));
    WalletToolset::new(wallet).with_scope_guard(guard).tools()
}

/// The wallet as a kernel KPI source (spec 3.5 §7.11, §7.14): a forward to
/// `WalletKpiSource::read`, which answers only for its own site (A-I22).
///
/// # Example
///
/// ```rust,ignore
/// let tracker = KpiTracker::new(site, charters, kpis)
///     .with_source(Arc::new(WalletKpi::new(WalletKpiSource::new(wallet, site))));
/// ```
pub struct WalletKpi(pub WalletKpiSource);

impl WalletKpi {
    /// The source over `source`.
    pub fn new(source: WalletKpiSource) -> Self {
        Self(source)
    }
}

#[async_trait]
impl KpiSource for WalletKpi {
    fn name(&self) -> &str {
        self.0.name()
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        self.0.read(site, metric, from, to).await.map_err(|error| adk_kernel::KernelError::Store {
            backend: "wallet",
            reason: error.to_string(),
        })
    }
}

/// The commerce side of profit and loss over the site's own records (spec 3.5 §7.12, §7.14):
/// leads are the back office's `lead_captured` signals; revenue and sales come from the
/// commerce ledger when one is set, and are zero otherwise.
pub struct SiteOutcomes {
    signals: Arc<dyn SignalStore>,
    partition: PartitionId,
    commerce: Option<Arc<dyn CommerceOutcomes>>,
}

impl SiteOutcomes {
    /// Leads from `signals` filed in the back office `partition`.
    pub fn new(signals: Arc<dyn SignalStore>, partition: impl Into<PartitionId>) -> Self {
        Self { signals, partition: partition.into(), commerce: None }
    }

    /// Revenue and sales from `commerce`.
    #[must_use]
    pub fn with_commerce(mut self, commerce: Arc<dyn CommerceOutcomes>) -> Self {
        self.commerce = Some(commerce);
        self
    }
}

#[async_trait]
impl CommerceOutcomes for SiteOutcomes {
    async fn revenue(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64> {
        match &self.commerce {
            Some(commerce) => commerce.revenue(from, to).await,
            None => Ok(0),
        }
    }

    async fn leads(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64> {
        let filter =
            SignalFilter::default().partition(self.partition.clone()).kind("lead_captured");
        let records = self
            .signals
            .list(&filter)
            .await
            .map_err(|error| adk_core::AdkError::config(error.to_string()))?;
        Ok(records
            .iter()
            .filter(|record| record.last_at >= from && record.last_at < to)
            .map(|record| u64::from(record.count.max(1)))
            .sum())
    }

    async fn sales(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<u64> {
        match &self.commerce {
            Some(commerce) => commerce.sales(from, to).await,
            None => Ok(0),
        }
    }
}
