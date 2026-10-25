//! The publishing pipeline (spec 3.5 §7.8.2 "The pipeline", A-I23, A-I24).
//!
//! | Step | Here |
//! |------|------|
//! | 1 Draft | [`PublishingPipeline::draft`]: the change, held apart from the working copy |
//! | 2 Check | [`PublishingPipeline::check`]: the launch gate's fast tier on the change |
//! | 3 Preview | [`PublishingPipeline::preview`]: a staged, unlogged release on the host's preview, with page diffs |
//! | 4 Approve | An [`ApprovalGate`] the harness implements: nobody, the owner, or a chain of distinct people |
//! | 5 Release | [`ReleaseBuilder`] on an incremental build, logged |
//! | 6 Publish | [`SiteHost::publish`], or `publish_delta` for a delta on a host that takes them |
//! | 7 Announce | An [`Announcer`]: `site.release.published` and `catalog.changed` payloads |
//! | 8 Measure | A [`ChangeTag`] for the KPI history, through a [`ChangeTagSink`] |
//!
//! Corrections, withdrawals (a notice or 410 Gone), per-item reverts, whole-site rollback and
//! scheduled actions run through the same steps. Releases are serialised per site.

mod batch;
mod schedule;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{Clock, SystemClock};
use async_trait::async_trait;
use awp_types::{
    CatalogChanged, CatalogChangesPage, ContentId, ContentItem, ContentState, Digest,
    ReleaseManifest, ReleasePublished, ReleaseSource,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::content::{ContentHistory, ContentStore, HistoryMeta, SiteProject};
use crate::error::{Result, SiteError};
use crate::gate::{GateInput, GateMode, GateReport, LaunchGate, Quarantine};
use crate::generator::{BuildReport, ChangeSet, DependencyGraph, SiteGenerator};
use crate::host::{HostTarget, PublishReport, SiteHost};
use crate::log::SiteLog;
use crate::release::{
    PreviousRelease, ReleaseBuilder, ReleaseOptions, StagedDelta, StagedRelease, file_digest,
    manifest_path,
};

pub use batch::BatchRelease;
pub use schedule::{
    ScheduledAction, ScheduledActionKind, due_actions, next_wake, schedule_actions,
};

/// Where a change came from. Only these author content; nothing from the public partition
/// does (spec 3.5 A-I24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum EntryPoint {
    /// An owner or staff member on a paired device: chat, WhatsApp, voice, email.
    PairedDevice {
        /// The pairing.
        device: String,
    },
    /// A person holding a role in the web editor.
    WebEditor,
    /// An authorised committer to the content repository.
    Repository {
        /// The committer.
        committer: String,
    },
    /// A feed the owner configured.
    Feed {
        /// The feed.
        feed: String,
    },
    /// One of the site's own agents.
    SiteAgent {
        /// The agent's principal.
        agent: String,
    },
    /// A schedule set earlier through one of the above.
    Schedule,
}

/// A change to the site: items to create or replace, with their media.
#[derive(Debug, Clone)]
pub struct Change {
    /// Items in their target state: `Published` (or any draft state), `Corrected`,
    /// `Withdrawn`, or `Scheduled` with a future `publish_at`.
    pub items: Vec<ContentItem>,
    /// Media by repository path, already through the media pipeline.
    pub media: Vec<(String, Bytes)>,
    /// Where it came from.
    pub entry: EntryPoint,
    /// The principal proposing it.
    pub proposer: String,
    /// The job, recorded as the release source.
    pub job: String,
    /// The action class whose approval chain applies, such as `publish`.
    pub action_class: String,
}

/// A change held apart from the working copy until it is released (a draft branch).
#[derive(Debug, Clone)]
pub struct Draft {
    /// The change.
    pub change: Change,
    /// Its items.
    pub ids: BTreeSet<ContentId>,
}

/// How a page changes in a preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageChange {
    /// A new page.
    Added,
    /// A changed page.
    Changed,
    /// A page that leaves the site.
    Removed,
}

/// One page of a preview's diff (spec 3.3 diff before write).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDiff {
    /// The page's file.
    pub path: String,
    /// How it changes.
    pub change: PageChange,
}

/// A staged preview of a draft.
#[derive(Debug, Clone)]
pub struct Preview {
    /// The preview release, signed by its digest and never logged.
    pub staged: StagedRelease,
    /// The fast tier's verdict.
    pub gate: GateReport,
    /// The pages it adds, changes and removes.
    pub diff: Vec<PageDiff>,
    /// The host's report, when the preview was published.
    pub report: Option<PublishReport>,
}

/// What an approver decides on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    /// The items.
    pub items: Vec<ContentId>,
    /// The action class.
    pub action_class: String,
    /// Who proposes it.
    pub proposer: String,
    /// Where it came from.
    pub entry: EntryPoint,
    /// The preview's manifest digest.
    pub preview_digest: Digest,
    /// The page diffs.
    pub diff: Vec<PageDiff>,
}

/// An approver's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ApprovalDecision {
    /// Approved by these principals, distinct and none of them the proposer.
    Approved {
        /// The approvers.
        approvers: Vec<String>,
        /// The decision record, for the release source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },
    /// Refused.
    Rejected {
        /// Who refused.
        by: String,
        /// Why.
        reason: String,
    },
    /// Waiting for people; resume with [`PublishingPipeline::complete`].
    Pending {
        /// The pending decision.
        ticket: String,
    },
}

/// Decides whether a change publishes: the job's autonomy level and the charter's approval
/// chain (spec 3.5 §7.8.2 step 4, A-I23). The harness implements it over its approvals.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Decides on `request`.
    ///
    /// # Errors
    ///
    /// Returns an error when the decision cannot be made or recorded.
    async fn decide(&self, request: &ApprovalRequest) -> Result<ApprovalDecision>;
}

/// An [`ApprovalGate`] that approves everything as one named principal: a job acting within
/// its autonomy, and tests.
#[derive(Debug, Clone)]
pub struct AutoApprove(pub String);

#[async_trait]
impl ApprovalGate for AutoApprove {
    async fn decide(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision> {
        Ok(ApprovalDecision::Approved { approvers: vec![self.0.clone()], decision: None })
    }
}

/// What a release tells subscribers and search engines (spec 3.5 §7.8.2 step 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Announcement {
    /// `site.release.published`.
    pub release: ReleasePublished,
    /// `catalog.changed`, when items changed: one per release, the first 1,000 ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogChanged>,
    /// The further pages of changed ids that `catalog.next` links, in order; the edge serves
    /// them at each page's URL (spec 3.5 §7.8.3 "Events and engines").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_pages: Vec<CatalogChangesPage>,
    /// The sitemap to ping engines with.
    pub sitemap: String,
    /// The changed page files.
    pub pages: Vec<String>,
}

/// Sends announcements: the edge's events, sitemap pings, newsletters by playbook.
#[async_trait]
pub trait Announcer: Send + Sync {
    /// Announces a release.
    ///
    /// # Errors
    ///
    /// Returns an error when the announcement cannot be queued; the release stands.
    async fn announce(&self, announcement: &Announcement) -> Result<()>;
}

/// A change tagged in the KPI history so its effect shows in the weekly review (spec 3.5
/// §7.8.2 step 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTag {
    /// The release.
    pub seq: u64,
    /// When it was published.
    pub at: DateTime<Utc>,
    /// The items it changed.
    pub items: Vec<ContentId>,
    /// What produced it.
    pub source: ReleaseSource,
}

/// Receives change tags: the kernel's KPI store.
#[async_trait]
pub trait ChangeTagSink: Send + Sync {
    /// Records a tag.
    ///
    /// # Errors
    ///
    /// Returns an error when the tag cannot be stored; the release stands.
    async fn tag(&self, tag: &ChangeTag) -> Result<()>;
}

/// How to withdraw an item (spec 3.5 §7.8.2 "Corrections, withdrawals and takedowns").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Withdrawal {
    /// The URL answers 410 Gone.
    Gone,
    /// The page is replaced by a notice with this text.
    Notice(String),
}

/// A published release.
#[derive(Debug, Clone)]
pub struct Published {
    /// The release.
    pub staged: StagedRelease,
    /// The host's report.
    pub report: PublishReport,
    /// What was announced.
    pub announcement: Announcement,
    /// The KPI tag.
    pub tag: ChangeTag,
}

/// How a change ended.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum PipelineOutcome {
    /// Live in a signed, logged release.
    Published(Published),
    /// The launch gate held it; nothing changed.
    Held(GateReport),
    /// Waiting for approval.
    Pending {
        /// The pending decision.
        ticket: String,
        /// The draft to complete.
        draft: Draft,
        /// The preview the approvers see.
        preview: Preview,
    },
    /// Refused by an approver.
    Rejected {
        /// Who refused.
        by: String,
        /// Why.
        reason: String,
    },
    /// Saved as scheduled; it publishes when its time comes.
    Scheduled {
        /// The next scheduled time.
        at: Option<DateTime<Utc>>,
    },
}

impl PipelineOutcome {
    /// The published release, if the change went live.
    pub fn published(&self) -> Option<&Published> {
        match self {
            Self::Published(published) => Some(published),
            _ => None,
        }
    }
}

struct State {
    previous: Option<PreviousRelease>,
    graph: Option<DependencyGraph>,
    preview_count: u64,
    /// The build report of what `work/build` holds, for the scheduled full checks.
    report: Option<BuildReport>,
}

/// The publishing pipeline of one site (spec 3.5 §7.8.2).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::pipeline::{AutoApprove, PublishingPipeline};
/// # use adk_site::{content::{SiteProject, ContentStore}, generator::BuiltinGenerator, host::SiteHost, log::SiteLog};
///
/// # async fn demo(base: SiteProject, store: Arc<dyn ContentStore>, host: Arc<dyn SiteHost>, log: Arc<SiteLog>) {
/// let pipeline = PublishingPipeline::builder(base, store, Arc::new(BuiltinGenerator), host, log, "/var/lib/site/work")
///     .approvals(Arc::new(AutoApprove("urn:adk:agent:site/steward".into())))
///     .build();
/// # let _ = pipeline;
/// # }
/// ```
pub struct PublishingPipeline {
    base: SiteProject,
    store: Arc<dyn ContentStore>,
    history: Option<Arc<dyn ContentHistory>>,
    generator: Arc<dyn SiteGenerator>,
    host: Arc<dyn SiteHost>,
    log: Arc<SiteLog>,
    gate: LaunchGate,
    approvals: Arc<dyn ApprovalGate>,
    announcer: Option<Arc<dyn Announcer>>,
    tags: Option<Arc<dyn ChangeTagSink>>,
    builder: ReleaseBuilder,
    clock: Arc<dyn Clock>,
    quarantine: Quarantine,
    work: PathBuf,
    state: Mutex<State>,
    changes_url: Option<String>,
}

/// Builds a [`PublishingPipeline`].
pub struct PublishingPipelineBuilder {
    pipeline: PublishingPipeline,
}

impl PublishingPipelineBuilder {
    /// Records every saved version in `history`.
    pub fn history(mut self, history: Arc<dyn ContentHistory>) -> Self {
        self.pipeline.history = Some(history);
        self
    }

    /// The launch gate.
    pub fn gate(mut self, gate: LaunchGate) -> Self {
        self.pipeline.gate = gate;
        self
    }

    /// The approval gate.
    pub fn approvals(mut self, approvals: Arc<dyn ApprovalGate>) -> Self {
        self.pipeline.approvals = approvals;
        self
    }

    /// The announcer.
    pub fn announcer(mut self, announcer: Arc<dyn Announcer>) -> Self {
        self.pipeline.announcer = Some(announcer);
        self
    }

    /// The KPI tag sink.
    pub fn tags(mut self, tags: Arc<dyn ChangeTagSink>) -> Self {
        self.pipeline.tags = Some(tags);
        self
    }

    /// The clock for build times and manifests.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.pipeline.builder = self.pipeline.builder.clone().with_clock(clock.clone());
        self.pipeline.clock = clock;
        self
    }

    /// The release builder.
    pub fn release_builder(mut self, builder: ReleaseBuilder) -> Self {
        self.pipeline.builder = builder.with_clock(self.pipeline.clock.clone());
        self
    }

    /// The quarantine the gate checks.
    pub fn quarantine(mut self, quarantine: Quarantine) -> Self {
        self.pipeline.quarantine = quarantine;
        self
    }

    /// Where the edge serves the pages of changed ids a `catalog.changed` event links:
    /// page `n` of release `seq` is `<url>/<seq>/<n>`. Defaults to
    /// `<base_url>/awp/v2/events/changes`.
    pub fn changes_url(mut self, url: impl Into<String>) -> Self {
        self.pipeline.changes_url = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// Continues after `previous`, the release the host serves.
    pub fn previous(mut self, previous: PreviousRelease) -> Self {
        self.pipeline.state.get_mut().previous = Some(previous);
        self
    }

    /// The pipeline.
    pub fn build(self) -> PublishingPipeline {
        self.pipeline
    }
}

/// One `catalog.changed` for a release's changed items: the first 1,000 ids, then pages of
/// 1,000 linked by `next` at `<changes_url>/<seq>/<page>` (spec 3.5 §7.8.3 "Events and
/// engines").
fn catalog_changes(
    changes_url: &str,
    seq: u64,
    items: &[ContentId],
) -> (Option<CatalogChanged>, Vec<CatalogChangesPage>) {
    if items.is_empty() {
        return (None, Vec::new());
    }
    let chunks: Vec<&[ContentId]> = items.chunks(CatalogChanged::PAGE_SIZE).collect();
    let next = |page: usize| (page <= chunks.len()).then(|| format!("{changes_url}/{seq}/{page}"));
    let catalog = CatalogChanged {
        release_seq: seq,
        items: chunks[0].to_vec(),
        total: items.len() as u64,
        next: next(2),
    };
    let pages = chunks
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, chunk)| CatalogChangesPage { items: chunk.to_vec(), next: next(index + 2) })
        .collect();
    (Some(catalog), pages)
}

/// Approval chains are satisfied only by distinct principals, and the proposer of a release
/// cannot also be its approver (spec 3.5 A-I23).
fn check_approvers(proposer: &str, approvers: &[String]) -> Result<()> {
    if approvers.is_empty() {
        return Err(SiteError::Pipeline("an approval names no approver".into()));
    }
    if approvers.iter().any(|a| a == proposer) {
        return Err(SiteError::Pipeline(format!(
            "'{proposer}' proposed the change and cannot approve it (A-I23)"
        )));
    }
    let distinct: BTreeSet<&String> = approvers.iter().collect();
    if distinct.len() != approvers.len() {
        return Err(SiteError::Pipeline(
            "an approval chain needs distinct principals (A-I23)".into(),
        ));
    }
    Ok(())
}

impl PublishingPipeline {
    /// Starts a pipeline. `base` carries the configuration, theme, terms, capabilities and
    /// documents; items come from `store` at each build. `work` holds the builds.
    pub fn builder(
        base: SiteProject,
        store: Arc<dyn ContentStore>,
        generator: Arc<dyn SiteGenerator>,
        host: Arc<dyn SiteHost>,
        log: Arc<SiteLog>,
        work: impl Into<PathBuf>,
    ) -> PublishingPipelineBuilder {
        PublishingPipelineBuilder {
            pipeline: Self {
                base,
                store,
                history: None,
                generator,
                host,
                log,
                gate: LaunchGate::new(),
                approvals: Arc::new(AutoApprove("unattended".into())),
                announcer: None,
                tags: None,
                builder: ReleaseBuilder::new(),
                clock: Arc::new(SystemClock),
                quarantine: Quarantine::default(),
                work: work.into(),
                state: Mutex::new(State {
                    previous: None,
                    graph: None,
                    preview_count: 0,
                    report: None,
                }),
                changes_url: None,
            },
        }
    }

    /// The release the pipeline last released.
    pub async fn previous(&self) -> Option<PreviousRelease> {
        self.state.lock().await.previous.clone()
    }

    async fn project(
        &self,
        items: Vec<ContentItem>,
        seq: u64,
        preview: bool,
    ) -> Result<SiteProject> {
        let mut project = self.base.clone();
        project.items = items;
        project.items.sort_by(|a, b| a.id.cmp(&b.id));
        for item in &project.items {
            for media in &item.media {
                if !project.media.contains_key(&media.path)
                    && let Some(bytes) = self.store.read_media(&media.path).await?
                {
                    project.media.insert(media.path.clone(), bytes);
                }
            }
        }
        project.as_of = self.clock.now();
        project.release_seq = seq;
        project.redirects_format = self.host.redirects_format();
        project.preview = preview;
        Ok(project)
    }

    /// The working copy with `draft` applied, draft states shown as they will publish.
    async fn candidate(&self, draft: &Draft) -> Result<Vec<ContentItem>> {
        let mut items: BTreeMap<ContentId, ContentItem> =
            self.store.list().await?.into_iter().map(|i| (i.id.clone(), i)).collect();
        for item in &draft.change.items {
            let mut item = item.clone();
            if matches!(
                item.state,
                ContentState::Draft | ContentState::InReview | ContentState::Approved
            ) {
                item.state = ContentState::Approved;
            }
            items.insert(item.id.clone(), item);
        }
        Ok(items.into_values().collect())
    }

    /// Step 1: holds a change apart from the working copy and records it in the history.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidContent`] when an item is in a language the site does not have.
    pub async fn draft(&self, change: Change) -> Result<Draft> {
        let languages: BTreeSet<String> =
            self.base.config.languages.iter().map(|l| l.as_str().to_ascii_lowercase()).collect();
        for item in &change.items {
            if !languages.contains(&item.language.as_str().to_ascii_lowercase()) {
                return Err(SiteError::InvalidContent {
                    path: item.id.0.clone(),
                    message: format!(
                        "language '{}' is not one of the site's languages",
                        item.language
                    ),
                });
            }
            if let Some(history) = &self.history {
                let meta = HistoryMeta {
                    at: Some(self.clock.now()),
                    actor: Some(change.proposer.clone()),
                    ..HistoryMeta::default()
                };
                history.record(item, meta).await?;
            }
        }
        let ids = change.items.iter().map(|i| i.id.clone()).collect();
        tracing::info!(site.content.items = change.items.len(), site.content.proposer = %change.proposer, "site.content.drafted");
        Ok(Draft { change, ids })
    }

    async fn build_candidate(
        &self,
        draft: &Draft,
        dir: &Path,
    ) -> Result<(SiteProject, BuildReport)> {
        let seq = self.state.lock().await.previous.as_ref().map_or(1, |p| p.manifest.seq + 1);
        let mut project = self.project(self.candidate(draft).await?, seq, false).await?;
        for (path, bytes) in &draft.change.media {
            project.media.insert(path.clone(), bytes.clone());
        }
        let report = self.generator.build(&project, dir).await?;
        Ok((project, report))
    }

    async fn fast_gate(
        &self,
        project: &SiteProject,
        dir: &Path,
        report: &BuildReport,
        ids: &BTreeSet<ContentId>,
    ) -> Result<GateReport> {
        self.gate
            .run(
                GateMode::Fast,
                &GateInput {
                    project,
                    out: dir,
                    report,
                    changed: Some(ids),
                    quarantine: &self.quarantine,
                },
            )
            .await
    }

    async fn preview_dir(&self) -> PathBuf {
        let mut state = self.state.lock().await;
        state.preview_count += 1;
        self.work.join(format!("preview-{}", state.preview_count))
    }

    /// Step 2: the launch gate's per-change checks on the draft.
    ///
    /// # Errors
    ///
    /// Build and gate errors.
    pub async fn check(&self, draft: &Draft) -> Result<GateReport> {
        let dir = self.preview_dir().await;
        let (project, report) = self.build_candidate(draft, &dir).await?;
        let verdict = self.fast_gate(&project, &dir, &report, &draft.ids).await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        verdict
    }

    /// Step 3: a staged preview release with page diffs, published to the host's preview.
    ///
    /// # Errors
    ///
    /// Build, gate, staging and host errors; [`SiteError::ServerCode`] when the preview holds
    /// server code (A-I1).
    pub async fn preview(&self, draft: &Draft) -> Result<Preview> {
        let dir = self.preview_dir().await;
        let (project, report) = self.build_candidate(draft, &dir).await?;
        let gate = self.fast_gate(&project, &dir, &report, &draft.ids).await?;
        let previous = self.state.lock().await.previous.clone();
        let options = ReleaseOptions {
            source: ReleaseSource::Edit { job: draft.change.job.clone(), decision: None },
            generator: report.generator.clone(),
            charter_version: self.base.config.charter_version,
            redirects: report.redirects.clone(),
            written: None,
            force_snapshot: true,
        };
        let staged = self.builder.stage(&dir, previous.as_ref(), options).await?;
        let before = previous.map(|p| p.files).unwrap_or_default();
        let mut diff = Vec::new();
        for (path, entry) in &staged.files {
            if !path.ends_with(".html") {
                continue;
            }
            match before.get(path) {
                None => diff.push(PageDiff { path: path.clone(), change: PageChange::Added }),
                Some(old) if old != entry => {
                    diff.push(PageDiff { path: path.clone(), change: PageChange::Changed })
                }
                Some(_) => {}
            }
        }
        for path in before.keys().filter(|p| p.ends_with(".html") && !staged.files.contains_key(*p))
        {
            diff.push(PageDiff { path: path.clone(), change: PageChange::Removed });
        }
        let report = if gate.passed() {
            Some(self.host.publish(&staged, HostTarget::Preview).await?)
        } else {
            None
        };
        Ok(Preview { staged, gate, diff, report })
    }

    /// Step 4: asks the approval gate and checks the answer against A-I23.
    ///
    /// # Errors
    ///
    /// [`SiteError::Pipeline`] when an approval names no approver, names the proposer, or
    /// repeats a principal; errors from the gate.
    pub async fn approve(&self, draft: &Draft, preview: &Preview) -> Result<ApprovalDecision> {
        let request = ApprovalRequest {
            items: draft.ids.iter().cloned().collect(),
            action_class: draft.change.action_class.clone(),
            proposer: draft.change.proposer.clone(),
            entry: draft.change.entry.clone(),
            preview_digest: preview.staged.digest.clone(),
            diff: preview.diff.clone(),
        };
        let decision = self.approvals.decide(&request).await?;
        if let ApprovalDecision::Approved { approvers, .. } = &decision {
            check_approvers(&draft.change.proposer, approvers)?;
        }
        Ok(decision)
    }

    /// Runs every step for a change: draft, check, preview, approve, release, publish,
    /// announce and measure.
    ///
    /// # Errors
    ///
    /// [`SiteError::ServerCode`] when the change would ship server code (A-I1); build,
    /// gate, approval, log and host errors.
    pub async fn run(&self, change: Change) -> Result<PipelineOutcome> {
        let draft = self.draft(change).await?;
        let preview = self.preview(&draft).await?;
        if preview.gate.refuses_release() {
            return Err(SiteError::GateFailed(preview.gate.summary()));
        }
        if !preview.gate.passed() {
            return Ok(PipelineOutcome::Held(preview.gate));
        }
        match self.approve(&draft, &preview).await? {
            ApprovalDecision::Approved { decision, .. } => {
                let outcome = self.complete(&draft, decision).await;
                let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
                outcome
            }
            ApprovalDecision::Rejected { by, reason } => {
                Ok(PipelineOutcome::Rejected { by, reason })
            }
            ApprovalDecision::Pending { ticket } => {
                Ok(PipelineOutcome::Pending { ticket, draft, preview })
            }
        }
    }

    /// Steps 5 to 8 for an approved draft: release, publish, announce, measure.
    ///
    /// # Errors
    ///
    /// As [`run`](Self::run).
    pub async fn complete(
        &self,
        draft: &Draft,
        decision: Option<String>,
    ) -> Result<PipelineOutcome> {
        let now = self.clock.now();
        let mut items = Vec::new();
        for item in &draft.change.items {
            let mut item = item.clone();
            if matches!(
                item.state,
                ContentState::Draft
                    | ContentState::InReview
                    | ContentState::Approved
                    | ContentState::Scheduled
            ) {
                let future =
                    item.schedule.as_ref().and_then(|s| s.publish_at).is_some_and(|at| at > now);
                item.state = if future { ContentState::Scheduled } else { ContentState::Published };
            }
            items.push(item);
        }
        for (path, bytes) in &draft.change.media {
            self.store.write_media(path, bytes.clone()).await?;
        }
        if items.iter().all(|i| i.state == ContentState::Scheduled) {
            for item in &items {
                self.store.put(item).await?;
            }
            let all = self.store.list().await?;
            tracing::info!(site.content.items = items.len(), "site.content.scheduled");
            return Ok(PipelineOutcome::Scheduled { at: next_wake(&all, now) });
        }
        let source = ReleaseSource::Edit { job: draft.change.job.clone(), decision };
        self.release_items(items, source, &draft.change.proposer).await
    }

    /// Writes items to the working copy and releases, restoring the copy when the gate
    /// holds the change.
    async fn release_items(
        &self,
        items: Vec<ContentItem>,
        source: ReleaseSource,
        actor: &str,
    ) -> Result<PipelineOutcome> {
        let mut state = self.state.lock().await;
        let ids: BTreeSet<ContentId> = items.iter().map(|i| i.id.clone()).collect();
        let mut before = Vec::new();
        for item in &items {
            before.push((item.id.clone(), self.store.get(&item.id).await?));
            self.store.put(item).await?;
        }
        let seq = state.previous.as_ref().map_or(1, |p| p.manifest.seq + 1);
        let project = self.project(self.store.list().await?, seq, false).await?;
        let build = self.work.join("build");
        let report = match &state.graph {
            Some(graph) => {
                let changes = ChangeSet { items: ids.clone(), previous: graph.clone() };
                self.generator.build_incremental(&project, &build, &changes).await?
            }
            None => self.generator.build(&project, &build).await?,
        };
        let gate = self.fast_gate(&project, &build, &report, &ids).await?;
        if !gate.passed() {
            for (id, old) in before {
                match old {
                    Some(old) => self.store.put(&old).await?,
                    None => {
                        self.store.remove(&id).await?;
                    }
                }
            }
            state.graph = None;
            state.report = None;
            if gate.refuses_release() {
                return Err(SiteError::GateFailed(gate.summary()));
            }
            return Ok(PipelineOutcome::Held(gate));
        }
        state.graph = Some(report.graph.clone());
        state.report = Some(report.clone());
        let options = ReleaseOptions {
            source: source.clone(),
            generator: report.generator.clone(),
            charter_version: self.base.config.charter_version,
            redirects: report.redirects.clone(),
            written: state.previous.as_ref().map(|_| report.written.clone()),
            force_snapshot: false,
        };
        let staged =
            self.builder.release(&build, state.previous.as_ref(), options, &self.log).await?;
        state.previous = Some(PreviousRelease::from_staged(&staged));
        drop(state);
        if let Some(history) = &self.history {
            for item in &items {
                let meta = HistoryMeta {
                    at: Some(self.clock.now()),
                    actor: Some(actor.to_string()),
                    release_seq: Some(staged.manifest.seq),
                    note: None,
                };
                history.record(item, meta).await?;
            }
        }
        let published = self.publish_staged(staged, ids.into_iter().collect(), source).await?;
        Ok(PipelineOutcome::Published(published))
    }

    async fn publish_staged(
        &self,
        staged: StagedRelease,
        items: Vec<ContentId>,
        source: ReleaseSource,
    ) -> Result<Published> {
        let report = if !staged.manifest.is_snapshot() && self.host.limits().delta_upload {
            self.host
                .publish_delta(&StagedDelta::new(staged.clone())?, HostTarget::Production)
                .await?
        } else {
            self.host.publish(&staged, HostTarget::Production).await?
        };
        tracing::info!(
            site.release.seq = staged.manifest.seq,
            site.release.uploaded = report.uploaded.len(),
            "site.release.published"
        );
        let base_url = self.base.config.base_url();
        let changes_url =
            self.changes_url.clone().unwrap_or_else(|| format!("{base_url}/awp/v2/events/changes"));
        let (catalog, catalog_pages) = catalog_changes(&changes_url, staged.manifest.seq, &items);
        let announcement = Announcement {
            release: ReleasePublished {
                seq: staged.manifest.seq,
                manifest: format!("{base_url}/{}", manifest_path(staged.manifest.seq)),
                digest: staged.digest.clone(),
                base: staged.manifest.base,
            },
            catalog,
            catalog_pages,
            sitemap: format!("{base_url}/sitemap.xml"),
            pages: staged.changed.iter().filter(|p| p.ends_with(".html")).cloned().collect(),
        };
        if let Some(announcer) = &self.announcer
            && let Err(error) = announcer.announce(&announcement).await
        {
            tracing::warn!(error = %error, "release announcement failed");
        }
        let tag = ChangeTag { seq: staged.manifest.seq, at: self.clock.now(), items, source };
        if let Some(tags) = &self.tags
            && let Err(error) = tags.tag(&tag).await
        {
            tracing::warn!(error = %error, "change tag failed");
        }
        Ok(Published { staged, report, announcement, tag })
    }

    /// A correction: a new release whose page carries a visible correction note (spec 3.5
    /// §7.8.2).
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an unknown item; as [`run`](Self::run).
    pub async fn correct(
        &self,
        id: &ContentId,
        edit: impl FnOnce(&mut ContentItem) + Send,
        note: &str,
        proposer: &str,
        entry: EntryPoint,
    ) -> Result<PipelineOutcome> {
        let mut item =
            self.store.get(id).await?.ok_or_else(|| SiteError::NotFound(id.0.clone()))?;
        edit(&mut item);
        let at = self.clock.now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let corrections = item
            .fields
            .entry("corrections".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = corrections {
            list.push(serde_json::json!({ "at": at, "note": note }));
        }
        item.state = ContentState::Corrected;
        let change = Change {
            items: vec![item],
            media: Vec::new(),
            entry,
            proposer: proposer.into(),
            job: "correction".into(),
            action_class: "publish".into(),
        };
        self.run(change).await
    }

    /// A withdrawal: the page answers 410 Gone or is replaced by a notice.
    ///
    /// # Errors
    ///
    /// [`SiteError::NotFound`] for an unknown item; as [`run`](Self::run).
    pub async fn withdraw(
        &self,
        id: &ContentId,
        how: Withdrawal,
        proposer: &str,
        entry: EntryPoint,
    ) -> Result<PipelineOutcome> {
        let mut item =
            self.store.get(id).await?.ok_or_else(|| SiteError::NotFound(id.0.clone()))?;
        let withdrawal = match how {
            Withdrawal::Gone => serde_json::json!({ "mode": "gone" }),
            Withdrawal::Notice(text) => serde_json::json!({ "mode": "notice", "text": text }),
        };
        item.fields.insert("withdrawal".into(), withdrawal);
        item.state = ContentState::Withdrawn;
        let change = Change {
            items: vec![item],
            media: Vec::new(),
            entry,
            proposer: proposer.into(),
            job: "withdrawal".into(),
            action_class: "publish".into(),
        };
        self.run(change).await
    }

    /// Undoes one item: publishes the version release `to` served, or the version before
    /// its latest release, and keeps every other change (spec 3.5 §7.8.3 "Undo per item").
    ///
    /// # Errors
    ///
    /// [`SiteError::Pipeline`] without a history or an earlier version; as [`run`](Self::run).
    pub async fn revert_item(
        &self,
        id: &ContentId,
        to: Option<u64>,
        proposer: &str,
        entry: EntryPoint,
    ) -> Result<PipelineOutcome> {
        let history = self
            .history
            .as_ref()
            .ok_or_else(|| SiteError::Pipeline("reverting needs a content history".into()))?;
        let version = match to {
            Some(seq) => history.at_release(id, seq).await?,
            None => history.previous_release(id).await?,
        }
        .ok_or_else(|| {
            SiteError::Pipeline(format!("item '{id}' has no earlier released version"))
        })?;
        let change = Change {
            items: vec![version.item],
            media: Vec::new(),
            entry,
            proposer: proposer.into(),
            job: "revert".into(),
            action_class: "publish".into(),
        };
        self.run(change).await
    }

    /// Rolls the whole site back to release `to_seq`: the host serves its files at once
    /// (A-I11), then a new release with the same files records the rollback in the log.
    ///
    /// # Errors
    ///
    /// [`SiteError::ReleaseNotFound`] when the host does not serve that release's manifest;
    /// digest, log and host errors.
    pub async fn rollback(&self, to_seq: u64) -> Result<Published> {
        let fetch_manifest = |seq: u64| async move {
            let bytes = self.host.fetch(&manifest_path(seq)).await?.ok_or_else(|| {
                SiteError::ReleaseNotFound { seq, message: "the host serves no manifest".into() }
            })?;
            serde_json::from_slice::<ReleaseManifest>(&bytes)
                .map_err(|e| SiteError::serde("release manifest", e))
        };
        let target = fetch_manifest(to_seq).await?;
        let mut chain = vec![target.clone()];
        while let Some(base) = chain.last().and_then(|m| m.base) {
            chain.push(fetch_manifest(base).await?);
        }
        let mut files = BTreeMap::new();
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
        }
        self.host.rollback(&target).await?;
        tracing::warn!(site.rollback.to = to_seq, "site.rollback");
        let dir = self.work.join(format!("rollback-{to_seq}-{}", self.clock.now().timestamp()));
        for (path, entry) in &files {
            let bytes = self.host.fetch(path).await?.ok_or_else(|| SiteError::ReleaseNotFound {
                seq: to_seq,
                message: format!("{path} is missing after the rollback"),
            })?;
            if file_digest(&bytes) != entry.sha256 {
                return Err(SiteError::DigestMismatch {
                    path: path.clone(),
                    expected: entry.sha256.to_string(),
                    actual: file_digest(&bytes).to_string(),
                });
            }
            let local = dir.join(path.trim_start_matches('/'));
            if let Some(parent) = local.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| SiteError::io(parent, e))?;
            }
            tokio::fs::write(&local, &bytes).await.map_err(|e| SiteError::io(&local, e))?;
        }
        let mut state = self.state.lock().await;
        let source = ReleaseSource::Rollback { to_seq };
        let options = ReleaseOptions {
            source: source.clone(),
            generator: target.generator.clone(),
            charter_version: target.charter_version,
            redirects: target.redirects.clone(),
            written: None,
            force_snapshot: true,
        };
        let staged =
            self.builder.release(&dir, state.previous.as_ref(), options, &self.log).await?;
        state.previous = Some(PreviousRelease::from_staged(&staged));
        // The working copy did not roll back; the next release rebuilds from it in full.
        state.graph = None;
        state.report = None;
        drop(state);
        self.publish_staged(staged, Vec::new(), source).await
    }

    /// Runs the scheduled actions due after `after` and up to now: publications, embargo
    /// ends, expiries and reverts, each released without a new approval (the schedule was
    /// approved when it was set).
    ///
    /// # Errors
    ///
    /// As [`run`](Self::run).
    pub async fn run_due(&self, after: Option<DateTime<Utc>>) -> Result<Vec<PipelineOutcome>> {
        let now = self.clock.now();
        let items = self.store.list().await?;
        let mut changed = Vec::new();
        for action in due_actions(&items, after, now) {
            let Some(mut item) = items.iter().find(|i| i.id == action.item).cloned() else {
                continue;
            };
            match action.kind {
                ScheduledActionKind::Publish => item.state = ContentState::Published,
                ScheduledActionKind::LiftEmbargo | ScheduledActionKind::Expire => {}
                ScheduledActionKind::Revert => {
                    let history = self.history.as_ref().ok_or_else(|| {
                        SiteError::Pipeline("a revert needs a content history".into())
                    })?;
                    match history.previous_release(&item.id).await? {
                        Some(version) => item = version.item,
                        None => continue,
                    }
                }
            }
            changed.retain(|c: &ContentItem| c.id != item.id);
            changed.push(item);
        }
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        let outcome = self
            .release_items(
                changed,
                ReleaseSource::Edit { job: "schedule".into(), decision: None },
                "schedule",
            )
            .await?;
        Ok(vec![outcome])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_changes_are_paginated_beyond_a_thousand() {
        let ids: Vec<ContentId> = (0..2_500).map(|i| ContentId(format!("p{i}"))).collect();
        let (catalog, pages) = catalog_changes("https://edge.example/changes", 7, &ids);
        let catalog = catalog.unwrap();
        assert_eq!((catalog.items.len(), catalog.total), (1_000, 2_500));
        assert_eq!(catalog.next.as_deref(), Some("https://edge.example/changes/7/2"));
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].next.as_deref(), Some("https://edge.example/changes/7/3"));
        assert_eq!((pages[1].items.len(), pages[1].next.as_deref()), (500, None));
        let all: Vec<ContentId> =
            catalog.items.into_iter().chain(pages.into_iter().flat_map(|p| p.items)).collect();
        assert_eq!(all, ids);
        let (small, none) = catalog_changes("u", 1, &ids[..3]);
        assert_eq!((small.unwrap().next, none.len()), (None, 0));
        assert_eq!(catalog_changes("u", 1, &[]).0, None);
    }

    #[test]
    fn approvers_must_be_distinct_and_not_the_proposer() {
        assert!(check_approvers("editor", &["publisher".into()]).is_ok());
        assert!(check_approvers("editor", &["editor".into()]).is_err());
        assert!(check_approvers("editor", &["publisher".into(), "publisher".into()]).is_err());
        assert!(check_approvers("editor", &[]).is_err());
    }
}
