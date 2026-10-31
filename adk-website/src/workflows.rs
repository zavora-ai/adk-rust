//! The site's model-free jobs as kernel runs (spec 3.5 §7.20, §7.11, §7.21): the Guardian's
//! probe ([`PROBE_WORKFLOW`]), the hosting job's upkeep check ([`WATCH_WORKFLOW`]) and the
//! weekly review's composition ([`DIGEST_WORKFLOW`]), a push to the content repository
//! ([`REPOSITORY_WORKFLOW`]), and the playbook-learning run ([`LEARNING_WORKFLOW`]).
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
pub use crate::jobs::{DIGEST_WORKFLOW, PROBE_WORKFLOW, WATCH_WORKFLOW};
pub use crate::learning::LEARNING_WORKFLOW;
pub use crate::repository::REPOSITORY_WORKFLOW;
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
    watches: BTreeMap<Uuid, Arc<crate::upkeep::UpkeepCheck>>,
    digests: BTreeMap<Uuid, DigestTarget>,
    repositories: BTreeMap<Uuid, Arc<crate::repository::RepositoryEntry>>,
    learnings: BTreeMap<Uuid, Arc<crate::learning::PlaybookLearning>>,
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

    /// Runs `site`'s daily upkeep check (spec 3.5 §7.20).
    #[must_use]
    pub fn watch(mut self, site: Uuid, check: Arc<crate::upkeep::UpkeepCheck>) -> Self {
        self.watches.insert(site, check);
        self
    }

    /// Takes `site`'s pushes to its content repository (spec 3.5 §7.8.2).
    #[must_use]
    pub fn repository(
        mut self,
        site: Uuid,
        entry: Arc<crate::repository::RepositoryEntry>,
    ) -> Self {
        self.repositories.insert(site, entry);
        self
    }

    /// Runs `site`'s weekly playbook learning (spec 3.5 §7.15).
    #[must_use]
    pub fn learning(
        mut self,
        site: Uuid,
        learning: Arc<crate::learning::PlaybookLearning>,
    ) -> Self {
        self.learnings.insert(site, learning);
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
        matches!(
            workflow_id,
            PROBE_WORKFLOW
                | WATCH_WORKFLOW
                | DIGEST_WORKFLOW
                | REPOSITORY_WORKFLOW
                | LEARNING_WORKFLOW
        )
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
        let work =
            async {
                match workflow_id {
                    PROBE_WORKFLOW => {
                        let probe = self
                            .probes
                            .get(&site)
                            .ok_or_else(|| format!("no probe is registered for site {site}"))?;
                        let outcome = probe.run().await;
                        // The probe runs every five minutes for as long as the site serves;
                        // each run reads its own requests so they do not accumulate.
                        let requests = probe.http.take_connections();
                        let mut value = serde_json::to_value(outcome.map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                        value["requests"] = json!(requests.len());
                        Ok(value)
                    }
                    WATCH_WORKFLOW => {
                        let check = self.watches.get(&site).ok_or_else(|| {
                            format!("no upkeep check is registered for site {site}")
                        })?;
                        let report = check.run().await.map_err(|e| e.to_string())?;
                        serde_json::to_value(report).map_err(|e| e.to_string())
                    }
                    DIGEST_WORKFLOW => {
                        let target = self.digests.get(&site).ok_or_else(|| {
                            format!("no weekly review is registered for site {site}")
                        })?;
                        let today = Utc::now().date_naive();
                        let this_monday = today
                            - Duration::days(i64::from(today.weekday().num_days_from_monday()));
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
                    REPOSITORY_WORKFLOW => {
                        let entry = self.repositories.get(&site).ok_or_else(|| {
                            format!("no content repository is configured for site {site}")
                        })?;
                        entry.execute(&input).await.map_err(|e| e.to_string())
                    }
                    LEARNING_WORKFLOW => {
                        let learning = self.learnings.get(&site).ok_or_else(|| {
                            format!("no playbook learning is registered for site {site}")
                        })?;
                        learning.run().await.map_err(|e| e.to_string())
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
