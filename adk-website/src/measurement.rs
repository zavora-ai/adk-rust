//! KPI sources that are not drivers (spec 3.5 §7.12, §7.11): what the KPI tracker and the
//! experiments read besides the analytics, search-console, email and advertising drivers.
//!
//! | Source | Name | Metrics | Counts come from |
//! |---|---|---|---|
//! | [`BeaconKpi`] | `beacon` | `pageviews`, `pageviews:{path}`, `events:{name}`, `visitors` | The edge's cookieless beacon (`AwpEdge::beacon`) |
//! | [`HostLogs`] | `host_logs` | `requests`, `bytes`, `status:{2xx…5xx}`, `not_found`, `pageviews`, `pageviews:{path}`, `crawlers`, `agents`, `agents:{name}`, `machine_views`, `agents.machine_views` | Host access logs in the Common, Combined or W3C extended format; no address is kept |
//! | [`CommerceKpi`] | `commerce` | `actions`, `actions:{capability}`, `actions:{capability}:{outcome}`, `leads`, `sales` | The edge's action ledger through [`MeteredActionLedger`]; leads and sales from the site's `CommerceOutcomes` |
//! | [`SpendLedgerKpi`] | `spend` | `spend`, `spend.vendor:{vendor}`, `spend.agent:{agent}`, in USD | The runtime's spend ledger through [`MeteredSpendLedger`] |
//! | [`AgentSearchKpi`] | `agents` | `calls`, `calls:{capability}`, `agent_calls`, `agent_calls:{binding}`, `agent_calls:{capability}`, `held` | The edge's capability-call audit records through [`EdgeMeter`]: agent calls are those of a verified requester, a trust tier above `public` (A-I6) |
//! | [`AeoKpi`] | `aeo` | `aeo.queries`, `aeo.citations`, `aeo.citations:{provider}`, `aeo.top3`, `aeo.citation_rate`, `pages.answer_citations` | [`AeoTracker`]'s questions through `web_search` providers, at Observe |
//!
//! Search-console performance is `adk-drivers`' `SearchMetrics` (`search`), and the wallet's
//! spend, revenue, profit and cost per lead and per sale are [`crate::wallet::WalletKpi`]
//! (`spend`; register one of the two `spend` sources, or rename one with `named`).
//!
//! Every count is a daily total in a `BeaconStore` (`InMemoryBeaconStore`, or a durable
//! store the deployment provides), read over the whole days a window covers: a source answers
//! `None` for a window that covers no whole day. Each source serves one site and refuses
//! another's (A-I22).
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_kernel::charter::KpiSource;
//! use adk_server::awp::InMemoryBeaconStore;
//! use adk_website::measurement::HostLogs;
//! use chrono::{TimeZone, Utc};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_kernel::Result<()> {
//! let site = uuid::Uuid::new_v4();
//! let logs = HostLogs::new(site, Arc::new(InMemoryBeaconStore::new()));
//! logs.ingest(
//!     "203.0.113.9 - - [24/Oct/2026:09:15:02 +0000] \"GET /pricing/?ref=mail HTTP/1.1\" 200 5120 \"-\" \"Mozilla/5.0\"\n\
//!      203.0.113.7 - - [24/Oct/2026:09:16:40 +0000] \"GET /pricing/ HTTP/1.1\" 200 5120 \"-\" \"Mozilla/5.0 (compatible; GPTBot/1.2)\"",
//! )
//! .await?;
//! let (from, to) = (
//!     Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap(),
//!     Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap(),
//! );
//! assert_eq!(logs.read(site, "pageviews:/pricing/", from, to).await?, Some(1.0));
//! assert_eq!(logs.read(site, "agents:gptbot", from, to).await?, Some(1.0));
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_kernel::KernelError;
use adk_server::awp::{BeaconCounts, BeaconStore};
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

mod aeo;
mod agents;
mod beacon;
mod host_logs;
mod ledgers;

pub use aeo::{AeoKpi, AeoObservation, AeoReport, AeoTracker};
pub use agents::{AgentSearchKpi, EdgeMeter};
pub use beacon::BeaconKpi;
pub use host_logs::{HostLogParser, HostLogs, IngestReport, LogLine};
pub use ledgers::{CommerceKpi, MeteredActionLedger, MeteredSpendLedger, SpendLedgerKpi};

/// Refuses a read for another site than the one a source serves (A-I22).
fn check_site(source: &str, serves: Uuid, asked: Uuid) -> adk_kernel::Result<()> {
    if serves == asked {
        Ok(())
    } else {
        Err(KernelError::Unauthorized {
            reason: format!(
                "the {source} KPI source serves site {serves}, not {asked}; each site reads its \
                 own measurements (A-I22)"
            ),
        })
    }
}

/// The total of `metric` in `counts` over the whole days `[from, to)` covers.
async fn total(
    backend: &'static str,
    counts: &Arc<dyn BeaconStore>,
    metric: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> adk_kernel::Result<Option<f64>> {
    BeaconCounts::new(Arc::clone(counts))
        .read(metric, from, to)
        .await
        .map_err(|error| KernelError::Store { backend, reason: error.to_string() })
}

/// Daily counts gathered in memory, then added to a store in one call per day and metric.
#[derive(Debug, Default)]
struct Tally(BTreeMap<(NaiveDate, String), u64>);

impl Tally {
    fn add(&mut self, day: NaiveDate, metric: impl Into<String>, count: u64) {
        if count > 0 {
            *self.0.entry((day, metric.into())).or_default() += count;
        }
    }

    async fn flush(self, backend: &'static str, store: &dyn BeaconStore) -> adk_kernel::Result<()> {
        for ((day, metric), count) in self.0 {
            store
                .add(day, &metric, count)
                .await
                .map_err(|error| KernelError::Store { backend, reason: error.to_string() })?;
        }
        Ok(())
    }
}
