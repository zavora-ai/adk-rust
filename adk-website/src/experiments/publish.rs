//! Serving the control alone as soon as an experiment stops (spec 3.5 §7.12, A-I15).

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_harness::website::SiteRuntime;
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use async_trait::async_trait;
use awp_types::ContentId;

use super::{ExperimentRecord, ExperimentStopHook};

/// Republishes the control page's content item through the site's publishing pipeline when
/// an experiment stops (spec 3.5 §7.12): the build no longer carries the stopped variant, so
/// the release that follows serves the control alone and takes the variant's file down. The
/// release is an ordinary one — signed, logged, through the launch gate and the charter's
/// approval chain for `publish` — proposed by `proposer`, the experiments job's agent.
///
/// A plan without an [`item`](super::ExperimentPlan::item) is left to the next release.
///
/// # Example
///
/// ```rust,ignore
/// let runner = ExperimentRunner::new(site.id, charters, experiments)
///     .with_stop_hook(Arc::new(RepublishOnStop::new(runtime.clone(), AgentRole::Growth.principal(&site))));
/// ```
pub struct RepublishOnStop {
    runtime: Arc<SiteRuntime>,
    proposer: String,
}

impl std::fmt::Debug for RepublishOnStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepublishOnStop").field("proposer", &self.proposer).finish_non_exhaustive()
    }
}

impl RepublishOnStop {
    /// Republishes through `runtime`'s pipeline as `proposer`.
    pub fn new(runtime: Arc<SiteRuntime>, proposer: impl Into<String>) -> Self {
        Self { runtime, proposer: proposer.into() }
    }
}

#[async_trait]
impl ExperimentStopHook for RepublishOnStop {
    async fn stopped(&self, record: &ExperimentRecord) -> Result<()> {
        let Some(id) = &record.plan.item else { return Ok(()) };
        let failed = |message: String| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Unavailable,
                "website.experiment.republish",
                message,
            )
        };
        let item = self
            .runtime
            .store()
            .get(&ContentId(id.clone()))
            .await
            .map_err(|error| failed(error.to_string()))?
            .ok_or_else(|| {
                failed(format!("content item {id} of experiment {} is gone", record.plan.id))
            })?;
        let change = Change {
            items: vec![item],
            media: Vec::new(),
            entry: EntryPoint::SiteAgent { agent: self.proposer.clone() },
            proposer: self.proposer.clone(),
            job: record.plan.job.clone(),
            action_class: "publish".into(),
        };
        let outcome =
            self.runtime.pipeline().run(change).await.map_err(|error| failed(error.to_string()))?;
        match outcome {
            PipelineOutcome::Published(published) => {
                tracing::info!(experiment = %record.plan.id, site.release.seq = published.staged.manifest.seq, "control served alone after the experiment stopped");
                Ok(())
            }
            PipelineOutcome::Held(gate) => Err(failed(format!(
                "the launch gate held the release without experiment {}: {}",
                record.plan.id,
                gate.summary()
            ))),
            PipelineOutcome::Pending { ticket, .. } => {
                tracing::info!(experiment = %record.plan.id, ticket = %ticket, "the release without the stopped experiment waits for its approval chain");
                Ok(())
            }
            PipelineOutcome::Rejected { by, reason } => Err(failed(format!(
                "{by} rejected the release without experiment {}: {reason}",
                record.plan.id
            ))),
            PipelineOutcome::Scheduled { .. } => Ok(()),
        }
    }
}
