//! The site's model-free jobs as kernel runs (spec 3.5 §7.20, §7.11, §7.21): the Guardian's
//! probe ([`PROBE_WORKFLOW`]) and the weekly review's composition ([`DIGEST_WORKFLOW`]).
//! Register [`SiteWorkflows`] with `KernelBuilder::workflows` and each process's trigger from
//! [`crate::jobs::processes`]; the other site processes run their job's agent.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_kernel::channels::ChannelSink;
use adk_server::background::{WorkflowExecutor, WorkflowState};
use async_trait::async_trait;
use chrono::{Datelike, Duration, Utc};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::incident::GuardianProbe;
pub use crate::jobs::{DIGEST_WORKFLOW, PROBE_WORKFLOW};
use crate::review::{ReviewComposer, ReviewInputs};

/// Where a site's weekly review goes.
pub struct DigestTarget {
    /// The composer.
    pub composer: Arc<ReviewComposer>,
    /// The owner's channel.
    pub sink: Arc<dyn ChannelSink>,
    /// The owner's thread on it.
    pub thread: String,
}

/// The executor of the site's model-free jobs, one probe and one digest per site (spec 3.5
/// §7.20).
#[derive(Default)]
pub struct SiteWorkflows {
    probes: BTreeMap<Uuid, Arc<GuardianProbe>>,
    digests: BTreeMap<Uuid, DigestTarget>,
}

impl SiteWorkflows {
    /// No sites yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `site`'s probe.
    #[must_use]
    pub fn probe(mut self, site: Uuid, probe: Arc<GuardianProbe>) -> Self {
        self.probes.insert(site, probe);
        self
    }

    /// Sends `site`'s weekly review to `target`.
    #[must_use]
    pub fn digest(mut self, site: Uuid, target: DigestTarget) -> Self {
        self.digests.insert(site, target);
        self
    }
}

#[async_trait]
impl WorkflowExecutor for SiteWorkflows {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        matches!(workflow_id, PROBE_WORKFLOW | DIGEST_WORKFLOW)
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> Result<Value, String> {
        let site = input
            .get("site")
            .and_then(Value::as_str)
            .and_then(|site| site.parse::<Uuid>().ok())
            .ok_or_else(|| {
                format!("a `{workflow_id}` run names its site as {{\"site\": <uuid>}}")
            })?;
        let work = async {
            match workflow_id {
                PROBE_WORKFLOW => {
                    let probe = self
                        .probes
                        .get(&site)
                        .ok_or_else(|| format!("no probe is registered for site {site}"))?;
                    let outcome = probe.run().await.map_err(|e| e.to_string())?;
                    serde_json::to_value(outcome).map_err(|e| e.to_string())
                }
                DIGEST_WORKFLOW => {
                    let target = self
                        .digests
                        .get(&site)
                        .ok_or_else(|| format!("no weekly review is registered for site {site}"))?;
                    let today = Utc::now().date_naive();
                    let this_monday =
                        today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
                    let review = target
                        .composer
                        .compose(this_monday - Duration::days(7), ReviewInputs::default())
                        .await
                        .map_err(|e| e.to_string())?;
                    let document = target
                        .composer
                        .send(&review, target.sink.as_ref(), &target.thread)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(json!({ "summary": review.summary(), "document": document }))
                }
                other => Err(format!("{other} is not a website workflow")),
            }
        };
        tokio::select! {
            () = cancel_token.cancelled() => Err("cancelled".to_string()),
            result = work => result,
        }
    }
}
