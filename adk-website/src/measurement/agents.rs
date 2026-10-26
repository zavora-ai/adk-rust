//! Agent search at the edge as the `agents` KPI source (spec 3.5 §7.12 "Discoverability
//! metrics"): verified agent discovery and capability calls counted from the edge's own audit
//! records, verified agents apart.

use std::sync::Arc;

use adk_core::audit_log::{AuditRecordDraft, AuditRecorder};
use adk_core::{Clock, Result, SystemClock};
use adk_kernel::charter::KpiSource;
use adk_server::awp::BeaconStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::{check_site, total};

/// The edge's audit recorder with a meter (spec 3.5 §7.12): every record goes to the
/// recorder it wraps first, then each `awp.capability.called` and `awp.capability.held`
/// record adds to the day's call counts by capability, binding and trust tier, and each
/// `awp.request.verified` record — a verified agent fetching the Agent Card, the manifest or
/// the MCP Server Card — to the day's discovery counts by surface. Give it to the edge as its
/// audit trail (`AwpEdge::builder(..).audit(AuditTrail::new(meter))`).
///
/// A call is an agent call when its requester holds a trust tier above `public`, which takes
/// a verified credential (A-I6): claimed identities do not count.
pub struct EdgeMeter {
    inner: Arc<dyn AuditRecorder>,
    counts: Arc<dyn BeaconStore>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for EdgeMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeMeter").finish_non_exhaustive()
    }
}

impl EdgeMeter {
    /// `inner`, with its capability calls counted in `counts`, on the system clock.
    pub fn new(inner: Arc<dyn AuditRecorder>, counts: Arc<dyn BeaconStore>) -> Self {
        Self { inner, counts, clock: Arc::new(SystemClock) }
    }

    /// Dates counts with `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The counts one record adds.
    fn metrics(draft: &AuditRecordDraft) -> Vec<String> {
        let field = |name: &str| draft.ext.get(name).and_then(Value::as_str).unwrap_or_default();
        let held = match draft.action.as_str() {
            "awp.capability.called" => false,
            "awp.capability.held" => true,
            "awp.request.verified" => {
                let surface = field("awp.surface");
                if surface.is_empty() {
                    return Vec::new();
                }
                return vec!["agent_discovery".to_string(), format!("agent_discovery:{surface}")];
            }
            _ => return Vec::new(),
        };
        let (capability, binding, tier) =
            (field("awp.capability"), field("awp.binding"), field("awp.tier"));
        let mut metrics = vec!["calls".to_string(), format!("calls:{capability}")];
        if held {
            metrics.push("held".to_string());
        }
        if !tier.is_empty() && tier != "public" {
            metrics.push("agent_calls".to_string());
            metrics.push(format!("agent_calls:{binding}"));
            metrics.push(format!("agent_calls:{capability}"));
        }
        metrics
    }
}

#[async_trait]
impl AuditRecorder for EdgeMeter {
    fn digest(&self, value: &Value) -> String {
        self.inner.digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        let metrics = Self::metrics(&draft);
        self.inner.record(draft).await?;
        let day = self.clock.now().date_naive();
        for metric in metrics {
            if let Err(error) = self.counts.add(day, &metric, 1).await {
                tracing::warn!(metric = %metric, error = %error, "an edge call count was not recorded");
            }
        }
        Ok(())
    }
}

/// Agent search at the edge as the `agents` KPI source (spec 3.5 §7.12): the counts
/// [`EdgeMeter`] keeps.
///
/// | Metric | Counts |
/// |---|---|
/// | `calls`, `calls:{capability}` | Capability calls over A2A, MCP and HTTP, held ones included |
/// | `held` | Calls held for the owner |
/// | `agent_calls`, `agent_calls:{binding}`, `agent_calls:{capability}` | Calls by verified requesters (trust above `public`), all, by binding (`a2a`, `mcp`, `http`) and by capability |
/// | `agent_discovery`, `agent_discovery:{surface}` | Verified agents' fetches of the Agent Card (`agent_card`), the capability manifest (`manifest`) and the MCP Server Card (`mcp_server_card`) |
///
/// Requests for machine views by self-declared AI agents are in
/// [`HostLogs`](super::HostLogs)' `agents.machine_views`.
#[derive(Clone)]
pub struct AgentSearchKpi {
    site: Uuid,
    counts: Arc<dyn BeaconStore>,
}

impl std::fmt::Debug for AgentSearchKpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSearchKpi").field("site", &self.site).finish_non_exhaustive()
    }
}

impl AgentSearchKpi {
    /// `site`'s edge call counts in `counts`.
    pub fn new(site: Uuid, counts: Arc<dyn BeaconStore>) -> Self {
        Self { site, counts }
    }
}

#[async_trait]
impl KpiSource for AgentSearchKpi {
    fn name(&self) -> &str {
        "agents"
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        check_site("agents", self.site, site)?;
        total("agents", &self.counts, metric, from, to).await
    }
}
