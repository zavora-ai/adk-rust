//! Publishing a batch and the scheduled full checks (spec 3.5 §7.8.3 "Tiered checks",
//! "Releases at scale", "Events and engines").
//!
//! A batch is released without a preview: its changes are already within the charter's rules
//! (exceptions were held before it formed). The fast tier runs on the changed items and
//! their dependents; a failing item is held on its own and the rest of the batch publishes,
//! while a server-code finding refuses the whole release (A-I1). The batch becomes one
//! release — a delta, or a snapshot every `snapshot_every` releases or once a day — with one
//! site log entry, one checkpoint and one `catalog.changed`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::Clock;
use awp_types::{ContentId, ContentItem, ReleaseSource};
use tracing::Instrument as _;

use super::{Published, PublishingPipeline};
use crate::content::{ContentHistory, ContentStore, HistoryMeta, SiteConfig, SiteProject};
use crate::error::{Result, SiteError};
use crate::gate::{Finding, GateInput, GateMode, GateReport};
use crate::generator::{BuildReport, ChangeSet};
use crate::host::SiteHost;
use crate::release::{PreviousRelease, ReleaseOptions};

/// What publishing a batch did.
#[derive(Debug, Clone)]
pub struct BatchRelease {
    /// The release, when any item published.
    pub published: Option<Published>,
    /// Items the fast tier held, with the findings that held them.
    pub held: BTreeMap<ContentId, Vec<Finding>>,
    /// The fast tier's verdict on the items that published.
    pub gate: Option<GateReport>,
}

/// Fast-tier rounds before a batch whose failures cannot be traced to items is held whole.
const MAX_ROUNDS: usize = 3;

impl PublishingPipeline {
    /// The working copy releases are built from.
    pub fn content_store(&self) -> Arc<dyn ContentStore> {
        self.store.clone()
    }

    /// The content history, when one is configured.
    pub fn content_history(&self) -> Option<Arc<dyn ContentHistory>> {
        self.history.clone()
    }

    /// The site configuration releases are built under.
    pub fn site_config(&self) -> &SiteConfig {
        &self.base.config
    }

    /// The host releases are published to.
    pub fn site_host(&self) -> Arc<dyn SiteHost> {
        self.host.clone()
    }

    /// The clock that stamps builds and manifests.
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    /// Publishes a batch of item versions as one release (spec 3.5 §7.8.3).
    ///
    /// `superseded` versions were replaced within the batch's window; they are recorded in
    /// the content history and never released. A fast-tier failure holds the failing item
    /// and the rest proceeds; failures that name no item hold the whole batch.
    ///
    /// # Errors
    ///
    /// [`SiteError::GateFailed`] when the build holds server code (A-I1): the release is
    /// refused whole and the working copy is restored; build, log and host errors.
    pub async fn publish_batch(
        &self,
        items: Vec<ContentItem>,
        superseded: Vec<ContentItem>,
        source: ReleaseSource,
        actor: &str,
    ) -> Result<BatchRelease> {
        let span = super::release_span(&source, items.len());
        self.publish_batch_in_span(items, superseded, source, actor).instrument(span).await
    }

    async fn publish_batch_in_span(
        &self,
        items: Vec<ContentItem>,
        superseded: Vec<ContentItem>,
        source: ReleaseSource,
        actor: &str,
    ) -> Result<BatchRelease> {
        let mut state = self.state.lock().await;
        if let Some(history) = &self.history {
            for item in &superseded {
                let meta = HistoryMeta {
                    at: Some(self.clock.now()),
                    actor: Some(actor.to_string()),
                    release_seq: None,
                    note: Some("superseded within its batch window".into()),
                };
                history.record(item, meta).await?;
            }
        }
        let mut pending: BTreeMap<ContentId, ContentItem> =
            items.into_iter().map(|item| (item.id.clone(), item)).collect();
        if pending.is_empty() {
            return Ok(BatchRelease { published: None, held: BTreeMap::new(), gate: None });
        }
        let mut before: BTreeMap<ContentId, Option<ContentItem>> = BTreeMap::new();
        for id in pending.keys() {
            before.insert(id.clone(), self.store.get(id).await?);
        }
        let mut held: BTreeMap<ContentId, Vec<Finding>> = BTreeMap::new();
        let seq = state.previous.as_ref().map_or(1, |p| p.manifest.seq + 1);
        let build = self.work.join("build");
        let mut graph = state.graph.clone();
        let mut written = BTreeSet::new();
        let mut changed: BTreeSet<ContentId> = pending.keys().cloned().collect();
        for item in pending.values() {
            self.store.put(item).await?;
        }
        let mut outcome: Option<(BuildReport, GateReport)> = None;
        for round in 0..MAX_ROUNDS {
            let (project, report) = match self.build_batch(&build, seq, &graph, &changed).await {
                Ok(built) => built,
                Err(error) => {
                    // An item the generator refuses (an A-I26 lane violation, say) leaves
                    // the working copy as it was; the next release rebuilds in full.
                    self.restore(&pending, &before).await?;
                    state.graph = None;
                    state.report = None;
                    return Err(error);
                }
            };
            graph = Some(report.graph.clone());
            written.extend(report.written.iter().cloned());
            let ids: BTreeSet<ContentId> = pending.keys().cloned().collect();
            let gate = self
                .gate
                .run(
                    GateMode::Fast,
                    &GateInput {
                        project: &project,
                        out: &build,
                        report: &report,
                        changed: Some(&ids),
                        quarantine: &self.quarantine,
                    },
                )
                .await?;
            if gate.refuses_release() {
                self.restore(&pending, &before).await?;
                let (_, report) = self.build_batch(&build, seq, &graph, &ids).await?;
                state.graph = Some(report.graph.clone());
                state.report = Some(report);
                tracing::warn!(
                    site.batch.items = ids.len(),
                    "site.release.refused: server code (A-I1)"
                );
                return Err(SiteError::GateFailed(gate.summary()));
            }
            if gate.passed() {
                outcome = Some((report, gate));
                break;
            }
            let mut failing: BTreeMap<ContentId, Vec<Finding>> = BTreeMap::new();
            for (_, finding) in gate.blocking() {
                if let Some(id) = finding.item.as_ref().filter(|id| pending.contains_key(*id)) {
                    failing.entry(id.clone()).or_default().push(finding.clone());
                }
            }
            if failing.is_empty() || round + 1 == MAX_ROUNDS {
                // Findings no item explains, or still failing after the last round: hold the
                // whole batch.
                let findings: Vec<Finding> = gate.blocking().map(|(_, f)| f.clone()).collect();
                for id in pending.keys() {
                    held.insert(id.clone(), findings.clone());
                }
                failing = pending.keys().map(|id| (id.clone(), Vec::new())).collect();
            }
            let reverted: BTreeMap<ContentId, ContentItem> = failing
                .keys()
                .filter_map(|id| pending.remove(id).map(|item| (id.clone(), item)))
                .collect();
            self.restore(&reverted, &before).await?;
            for (id, findings) in failing {
                if !findings.is_empty() {
                    held.insert(id, findings);
                }
            }
            changed = reverted.keys().cloned().collect();
            tracing::info!(
                site.batch.held = held.len(),
                site.batch.remaining = pending.len(),
                "fast tier held items"
            );
            if pending.is_empty() {
                let (_, report) = self.build_batch(&build, seq, &graph, &changed).await?;
                state.graph = Some(report.graph.clone());
                state.report = Some(report);
                return Ok(BatchRelease { published: None, held, gate: None });
            }
        }
        let Some((report, gate)) = outcome else {
            return Err(SiteError::Pipeline("the batch's fast checks did not settle".into()));
        };
        state.graph = graph;
        let options = ReleaseOptions {
            source: source.clone(),
            generator: report.generator.clone(),
            charter_version: self.base.config.charter_version,
            redirects: report.redirects.clone(),
            written: state.previous.as_ref().map(|_| written),
            force_snapshot: false,
        };
        state.report = Some(report);
        let staged = self.release_logged(&build, state.previous.as_ref(), options, actor).await?;
        state.previous = Some(PreviousRelease::from_staged(&staged));
        drop(state);
        if let Some(history) = &self.history {
            for item in pending.values() {
                let meta = HistoryMeta {
                    at: Some(self.clock.now()),
                    actor: Some(actor.to_string()),
                    release_seq: Some(staged.manifest.seq),
                    note: None,
                };
                history.record(item, meta).await?;
            }
        }
        tracing::info!(
            site.release.seq = staged.manifest.seq,
            site.batch.items = pending.len(),
            site.batch.held = held.len(),
            "site.batch.published"
        );
        let released: Vec<ContentItem> = pending.into_values().collect();
        let ids = released.iter().map(|item| item.id.clone()).collect();
        let published = self.publish_staged(staged, ids, source, actor).await?;
        self.record_content(&released, published.staged.manifest.seq, actor).await;
        Ok(BatchRelease { published: Some(published), held, gate: Some(gate) })
    }

    /// Builds `changed` incrementally from `graph`, or in full without one.
    async fn build_batch(
        &self,
        build: &std::path::Path,
        seq: u64,
        graph: &Option<crate::generator::DependencyGraph>,
        changed: &BTreeSet<ContentId>,
    ) -> Result<(SiteProject, BuildReport)> {
        let project = self.project(self.store.list().await?, seq, false).await?;
        let report = match graph {
            Some(previous) => {
                let changes = ChangeSet { items: changed.clone(), previous: previous.clone() };
                self.generator.build_incremental(&project, build, &changes).await?
            }
            None => self.generator.build(&project, build).await?,
        };
        Ok((project, report))
    }

    /// Puts back the versions the store held before a batch.
    async fn restore(
        &self,
        items: &BTreeMap<ContentId, ContentItem>,
        before: &BTreeMap<ContentId, Option<ContentItem>>,
    ) -> Result<()> {
        for id in items.keys() {
            match before.get(id).cloned().flatten() {
                Some(old) => self.store.put(&old).await?,
                None => {
                    self.store.remove(id).await?;
                }
            }
        }
        Ok(())
    }

    /// Runs the launch gate's full tier over the site as released: the scheduled checks
    /// (spec 3.5 §7.8.3 "Tiered checks" — default every six hours or 500 releases; see
    /// [`FullCheckSchedule`](crate::updates::FullCheckSchedule)).
    ///
    /// Gate extensions registered for the full tier, such as `awp verify` against
    /// production, run with it.
    ///
    /// # Errors
    ///
    /// Build and gate errors.
    pub async fn full_check(&self) -> Result<GateReport> {
        let mut state = self.state.lock().await;
        let seq = state.previous.as_ref().map_or(1, |p| p.manifest.seq);
        let build = self.work.join("build");
        let project = self.project(self.store.list().await?, seq, false).await?;
        let report = match state.report.clone() {
            Some(report) => report,
            None => {
                let report = self.generator.build(&project, &build).await?;
                state.graph = Some(report.graph.clone());
                state.report = Some(report.clone());
                report
            }
        };
        let verdict = self
            .gate
            .run(
                GateMode::Full,
                &GateInput {
                    project: &project,
                    out: &build,
                    report: &report,
                    changed: None,
                    quarantine: &self.quarantine,
                },
            )
            .await?;
        tracing::info!(
            site.release.seq = seq,
            passed = verdict.passed(),
            "scheduled full checks ran"
        );
        Ok(verdict)
    }
}
