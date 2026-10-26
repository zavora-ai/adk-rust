//! The edge's beacon as the `beacon` KPI source (spec 3.5 §7.12, Q4).

use adk_kernel::KernelError;
use adk_kernel::charter::KpiSource;
use adk_server::awp::BeaconCounts;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::check_site;

/// The edge's cookieless beacon as the kernel's `beacon` KPI source (spec 3.5 §7.12): a
/// forward to `AwpEdge::beacon().read`, its store errors as the kernel's.
///
/// | Metric | Counts |
/// |---|---|
/// | `pageviews`, `pageviews:{path}` | Page views, all or of one path |
/// | `events:{name}` | One event, such as an experiment's `events:exp.headline.b.exposure` |
/// | `visitors` | Visitors under a live `persistent_profile` receipt, distinct per day |
///
/// The beacon counts nothing under `Sec-GPC: 1` and keeps no cookie, address or identifier
/// without a receipt; this source reads only its daily totals.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::charter::KpiSource;
/// use adk_server::awp::{BeaconCounts, BeaconStore, InMemoryBeaconStore};
/// use adk_website::measurement::BeaconKpi;
/// use chrono::{Duration, TimeZone, Utc};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let site = uuid::Uuid::new_v4();
/// let store = Arc::new(InMemoryBeaconStore::new());
/// let day = Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap();
/// store.add(day.date_naive(), "events:signup", 4).await.unwrap();
/// let beacon = BeaconKpi::new(site, BeaconCounts::new(store));
/// assert_eq!(beacon.read(site, "events:signup", day, day + Duration::days(1)).await?, Some(4.0));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct BeaconKpi {
    site: Uuid,
    counts: BeaconCounts,
}

impl BeaconKpi {
    /// The beacon `counts` of `site`'s edge.
    pub fn new(site: Uuid, counts: BeaconCounts) -> Self {
        Self { site, counts }
    }
}

#[async_trait]
impl KpiSource for BeaconKpi {
    fn name(&self) -> &str {
        "beacon"
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        check_site("beacon", self.site, site)?;
        self.counts
            .read(metric, from, to)
            .await
            .map_err(|error| KernelError::Store { backend: "beacon", reason: error.to_string() })
    }
}
