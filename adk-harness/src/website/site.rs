//! The site runtime the back-office tools act on: `adk-site`'s publishing pipeline, launch
//! gate, host and media pipeline, the charter store, and the decisions people make
//! (spec 3.5 §7.6, §7.8, §7.8.1, §7.8.2, A-I18, A-I23).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};
use adk_kernel::charter::{
    CharterStore, CharterVersion, CommitDecision, RoleApproval, check_chain, commit_audited,
};
use adk_site::content::{ContentHistory, ContentStore, SiteProject};
use adk_site::gate::{
    ClaimCheck, ClaimFinding, GateInput, GateMode, GateReport, LaunchGate, Quarantine,
};
use adk_site::generator::SiteGenerator;
use adk_site::host::SiteHost;
use adk_site::log::{CHECKPOINT_PATH, SiteLog};
use adk_site::media::MediaPipeline;
use adk_site::pipeline::{
    Announcer, ApprovalDecision, ApprovalGate, ApprovalRequest, ChangeTagSink, Draft, Preview,
    PublishingPipeline,
};
use adk_site::release::PreviousRelease;
use async_trait::async_trait;
use awp_types::{Charter, ContentItem, Digest, Governance, Role};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::facts::{FactScope, FactStatus, FactStore};

/// The approver a release names when the governed path, not an approval chain, allowed it:
/// the job's level let the call run, or the owner approved the held call (spec 3.5 §7.8.2
/// step 4).
pub const GOVERNED_APPROVER: &str = "adk-harness:governed-path";

/// Which site the runtime serves (spec 3.5 §7.19, A-I22).
///
/// `name` is the site's application name: every agent of the site acts as
/// `urn:adk:agent:{name}/{agent}`, and in agency mode no two sites share one.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
///
/// let site = SiteRef::new(uuid::Uuid::nil(), "bakery", "https://bakery.example");
/// assert_eq!(site.principal("steward"), "urn:adk:agent:bakery/steward");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteRef {
    /// The charter's `site_id`.
    pub id: Uuid,
    /// The site's application name.
    pub name: String,
    /// The public origin, such as `https://bakery.example`.
    pub origin: String,
}

impl SiteRef {
    /// A site `id` named `name` served at `origin`.
    pub fn new(id: Uuid, name: impl Into<String>, origin: impl Into<String>) -> Self {
        Self { id, name: name.into(), origin: origin.into() }
    }

    /// The principal of the site's agent `agent`: `urn:adk:agent:{site}/{agent}` (spec 3.5
    /// §7.19).
    pub fn principal(&self, agent: &str) -> String {
        format!("urn:adk:agent:{}/{agent}", self.name)
    }
}

pub(crate) fn site_error(error: adk_site::SiteError) -> AdkError {
    let category = match &error {
        adk_site::SiteError::NotFound(_) | adk_site::SiteError::ReleaseNotFound { .. } => {
            ErrorCategory::NotFound
        }
        adk_site::SiteError::InvalidContent { .. } | adk_site::SiteError::GateFailed(_) => {
            ErrorCategory::InvalidInput
        }
        adk_site::SiteError::ServerCode { .. } => ErrorCategory::Forbidden,
        _ => ErrorCategory::Internal,
    };
    AdkError::new(ErrorComponent::Tool, category, "website.site", error.to_string())
}

pub(crate) fn kernel_error(error: adk_kernel::KernelError) -> AdkError {
    let category = match &error {
        adk_kernel::KernelError::Unauthorized { .. } => ErrorCategory::Forbidden,
        adk_kernel::KernelError::Config { .. } => ErrorCategory::InvalidInput,
        _ => ErrorCategory::Internal,
    };
    AdkError::new(ErrorComponent::Tool, category, "website.kernel", error.to_string())
}

/// A claim check over the harness's long-term facts (spec 3.5 A-I18): every fact an item
/// cites must be `Known` in the site's scope, so a claim from an `Assumed` fact never
/// publishes.
///
/// # Example
///
/// ```rust,ignore
/// let gate = LaunchGate::new().with_claim_check(Arc::new(FactClaimCheck::new(facts, scope)));
/// ```
pub struct FactClaimCheck {
    facts: Arc<dyn FactStore>,
    scope: FactScope,
}

impl FactClaimCheck {
    /// Checks claims against the facts of `scope` in `facts`.
    pub fn new(facts: Arc<dyn FactStore>, scope: FactScope) -> Self {
        Self { facts, scope }
    }
}

#[async_trait]
impl ClaimCheck for FactClaimCheck {
    async fn unsupported_claims(&self, item: &ContentItem) -> adk_site::Result<Vec<ClaimFinding>> {
        let mut findings = Vec::new();
        for cited in &item.sources {
            let fact = self
                .facts
                .get(&cited.0)
                .await
                .map_err(|e| adk_site::SiteError::Pipeline(format!("facts: {e}")))?
                .filter(|fact| self.scope.admits(&fact.scope));
            let reason = match fact.map(|fact| fact.status) {
                Some(FactStatus::Known) => continue,
                Some(FactStatus::Assumed) => "the fact is Assumed; the owner has not confirmed it",
                Some(_) => "the fact is no longer live",
                None => "the fact is not in the site's facts",
            };
            findings.push(ClaimFinding {
                claim: cited.0.clone(),
                fact: Some(cited.clone()),
                reason: reason.to_string(),
            });
        }
        Ok(findings)
    }
}

/// A decision people make on a release under an approval chain (spec 3.5 A-I23).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum ChainVote {
    Approve(RoleApproval),
    Reject { by: String, reason: String },
}

/// The release approval gate of a site (spec 3.5 §7.8.2 step 4, A-I23).
///
/// | The charter | What decides |
/// |---|---|
/// | names an approval chain for the action class | the people who hold its roles, each a distinct person and none the proposer, recorded with [`approve`](Self::approve) |
/// | names none | the governed path: the call ran because the job's level allowed it or the owner approved it, recorded as [`GOVERNED_APPROVER`] |
pub struct SiteApprovals {
    site: Uuid,
    charters: Option<Arc<dyn CharterStore>>,
    votes: Mutex<BTreeMap<Digest, Vec<ChainVote>>>,
}

impl std::fmt::Debug for SiteApprovals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteApprovals").field("site", &self.site).finish_non_exhaustive()
    }
}

impl SiteApprovals {
    fn new(site: Uuid, charters: Option<Arc<dyn CharterStore>>) -> Self {
        Self { site, charters, votes: Mutex::new(BTreeMap::new()) }
    }

    /// Records a person's approval of the preview `digest` in `approval.role`.
    pub async fn approve(&self, digest: &Digest, approval: RoleApproval) {
        self.votes
            .lock()
            .await
            .entry(digest.clone())
            .or_default()
            .push(ChainVote::Approve(approval));
    }

    /// Records a person's refusal of the preview `digest`.
    pub async fn reject(&self, digest: &Digest, by: &SubjectRef, reason: &str) {
        self.votes
            .lock()
            .await
            .entry(digest.clone())
            .or_default()
            .push(ChainVote::Reject { by: by.id.clone(), reason: reason.to_string() });
    }

    async fn governance(&self, action_class: &str) -> Result<Option<Governance>> {
        let Some(charters) = &self.charters else { return Ok(None) };
        let current = charters.current(self.site).await.map_err(kernel_error)?;
        Ok(current.map(|version| version.charter.governance).filter(|governance| {
            governance.approval_chains.iter().any(|chain| chain.action.as_str() == action_class)
        }))
    }
}

#[async_trait]
impl ApprovalGate for SiteApprovals {
    async fn decide(&self, request: &ApprovalRequest) -> adk_site::Result<ApprovalDecision> {
        let governance = self
            .governance(&request.action_class)
            .await
            .map_err(|e| adk_site::SiteError::Pipeline(e.to_string()))?;
        let Some(governance) = governance else {
            return Ok(ApprovalDecision::Approved {
                approvers: vec![GOVERNED_APPROVER.to_string()],
                decision: None,
            });
        };
        let votes =
            self.votes.lock().await.get(&request.preview_digest).cloned().unwrap_or_default();
        if let Some(ChainVote::Reject { by, reason }) =
            votes.iter().find(|vote| matches!(vote, ChainVote::Reject { .. }))
        {
            return Ok(ApprovalDecision::Rejected { by: by.clone(), reason: reason.clone() });
        }
        let approvals: Vec<RoleApproval> = votes
            .into_iter()
            .filter_map(|vote| match vote {
                ChainVote::Approve(approval) => Some(approval),
                ChainVote::Reject { .. } => None,
            })
            .collect();
        let action = awp_types::ActionClass::from(request.action_class.as_str());
        match check_chain(&governance, &action, &approvals, Some(&request.proposer)) {
            Ok(()) => Ok(ApprovalDecision::Approved {
                approvers: approvals.iter().map(|a| a.principal.id.clone()).collect(),
                decision: Some(format!("chain:{}", request.preview_digest)),
            }),
            Err(refusal) => {
                tracing::info!(website.release.preview = %request.preview_digest, reason = %refusal, "release waits for its approval chain");
                Ok(ApprovalDecision::Pending { ticket: request.preview_digest.to_string() })
            }
        }
    }
}

/// A request for every required signer of a charter version (spec 3.5 §7.6, §7.18 stage 5,
/// A-I23): a decision card per signer, and a commit only when the chain is satisfied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharterSignRequest {
    /// The request's id.
    pub id: String,
    /// The draft to commit.
    pub draft: Charter,
    /// The roles that must sign, one distinct person each.
    pub roles: Vec<Role>,
    /// The signatures so far.
    pub signatures: Vec<RoleApproval>,
    /// Who asked: the agent that drafted it.
    pub requested_by: String,
    /// When.
    pub requested_at: DateTime<Utc>,
    /// The committed version, once every signer signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed: Option<u32>,
}

/// A request that the owner promote a job (spec 3.5 §7.11, A-I13): the weekly review's
/// offer, decided only by an owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotionRequest {
    /// The request's id.
    pub id: String,
    /// The job.
    pub job: String,
    /// Why the agent asks: the evidence behind the offer.
    pub reason: String,
    /// Who asked.
    pub requested_by: String,
    /// When.
    pub requested_at: DateTime<Utc>,
}

/// The decisions a site waits on (spec 3.5 §7.6, §7.11).
#[derive(Default)]
pub(crate) struct SiteRequests {
    pub(crate) charters: Mutex<BTreeMap<String, CharterSignRequest>>,
    pub(crate) promotions: Mutex<BTreeMap<String, PromotionRequest>>,
}

/// A draft the back office holds until it is published (spec 3.5 §7.8.2 step 1).
pub(crate) struct DraftRecord {
    pub(crate) draft: Draft,
    pub(crate) preview: Option<Preview>,
}

/// Everything the back-office tools act on (spec 3.5 §7.8, §7.8.2): build one with
/// [`SiteRuntime::builder`].
pub struct SiteRuntime {
    pub(crate) site: SiteRef,
    pub(crate) base: SiteProject,
    pub(crate) store: Arc<dyn ContentStore>,
    pub(crate) generator: Arc<dyn SiteGenerator>,
    pub(crate) host: Arc<dyn SiteHost>,
    pub(crate) pipeline: PublishingPipeline,
    pub(crate) gate: LaunchGate,
    pub(crate) quarantine: Quarantine,
    pub(crate) media: MediaPipeline,
    pub(crate) work: PathBuf,
    pub(crate) approvals: Arc<SiteApprovals>,
    pub(crate) charters: Option<Arc<dyn CharterStore>>,
    pub(crate) facts: Option<(Arc<dyn FactStore>, FactScope)>,
    pub(crate) verifier: Option<Arc<dyn super::verify::SiteVerifier>>,
    pub(crate) crawler: Option<Arc<dyn crate::web::RequestSigner>>,
    pub(crate) audit: Option<adk_core::audit_log::AuditTrail>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) drafts: Mutex<BTreeMap<String, DraftRecord>>,
    pub(crate) charter_drafts: Mutex<BTreeMap<String, Charter>>,
    pub(crate) requests: SiteRequests,
    pub(crate) next: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for SiteRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteRuntime").field("site", &self.site).finish_non_exhaustive()
    }
}

/// Builds a [`SiteRuntime`].
pub struct SiteRuntimeBuilder {
    site: SiteRef,
    base: SiteProject,
    store: Arc<dyn ContentStore>,
    generator: Arc<dyn SiteGenerator>,
    host: Arc<dyn SiteHost>,
    log: Arc<SiteLog>,
    work: PathBuf,
    history: Option<Arc<dyn ContentHistory>>,
    announcer: Option<Arc<dyn Announcer>>,
    tags: Option<Arc<dyn ChangeTagSink>>,
    previous: Option<PreviousRelease>,
    quarantine: Quarantine,
    media: MediaPipeline,
    charters: Option<Arc<dyn CharterStore>>,
    facts: Option<(Arc<dyn FactStore>, FactScope)>,
    verifier: Option<Arc<dyn super::verify::SiteVerifier>>,
    crawler: Option<Arc<dyn crate::web::RequestSigner>>,
    audit: Option<adk_core::audit_log::AuditTrail>,
    clock: Arc<dyn Clock>,
    gate: LaunchGate,
}

impl SiteRuntimeBuilder {
    /// Records every saved version, which per-item reverts need.
    #[must_use]
    pub fn history(mut self, history: Arc<dyn ContentHistory>) -> Self {
        self.history = Some(history);
        self
    }

    /// Announces releases.
    #[must_use]
    pub fn announcer(mut self, announcer: Arc<dyn Announcer>) -> Self {
        self.announcer = Some(announcer);
        self
    }

    /// Tags releases in the KPI history.
    #[must_use]
    pub fn tags(mut self, tags: Arc<dyn ChangeTagSink>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Continues after the release the host serves.
    #[must_use]
    pub fn previous(mut self, previous: PreviousRelease) -> Self {
        self.previous = Some(previous);
        self
    }

    /// The sanitiser's quarantine.
    #[must_use]
    pub fn quarantine(mut self, quarantine: Quarantine) -> Self {
        self.quarantine = quarantine;
        self
    }

    /// The media pipeline (default policy otherwise).
    #[must_use]
    pub fn media(mut self, media: MediaPipeline) -> Self {
        self.media = media;
        self
    }

    /// The charter store: approval chains, charter drafts and their commits.
    #[must_use]
    pub fn charters(mut self, charters: Arc<dyn CharterStore>) -> Self {
        self.charters = Some(charters);
        self
    }

    /// The site's long-term facts in `scope`: claims are checked against them (A-I18) and
    /// discovery and interview answers are recorded in them.
    #[must_use]
    pub fn facts(mut self, facts: Arc<dyn FactStore>, scope: FactScope) -> Self {
        self.facts = Some((facts, scope));
        self
    }

    /// The verifier `awp_verify` runs.
    #[must_use]
    pub fn verifier(mut self, verifier: Arc<dyn super::verify::SiteVerifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Signs the crawls `site_discover` makes (spec 3.5 §7.3 "Outbound").
    #[must_use]
    pub fn crawler_signer(mut self, signer: Arc<dyn crate::web::RequestSigner>) -> Self {
        self.crawler = Some(signer);
        self
    }

    /// Where the `website.*` records go.
    #[must_use]
    pub fn audit(mut self, trail: adk_core::audit_log::AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// The clock.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// A launch gate with red lines or extensions; the claim check over the site's facts is
    /// added to it.
    #[must_use]
    pub fn gate(mut self, gate: LaunchGate) -> Self {
        self.gate = gate;
        self
    }

    /// The runtime.
    pub fn build(self) -> Arc<SiteRuntime> {
        let approvals = Arc::new(SiteApprovals::new(self.site.id, self.charters.clone()));
        let gate = match &self.facts {
            Some((facts, scope)) => self
                .gate
                .with_claim_check(Arc::new(FactClaimCheck::new(facts.clone(), scope.clone()))),
            None => self.gate,
        };
        let mut pipeline = PublishingPipeline::builder(
            self.base.clone(),
            self.store.clone(),
            self.generator.clone(),
            self.host.clone(),
            self.log,
            self.work.join("pipeline"),
        )
        .gate(gate.clone())
        .approvals(approvals.clone())
        .quarantine(self.quarantine.clone())
        .clock(self.clock.clone());
        if let Some(history) = self.history {
            pipeline = pipeline.history(history);
        }
        if let Some(announcer) = self.announcer {
            pipeline = pipeline.announcer(announcer);
        }
        if let Some(tags) = self.tags {
            pipeline = pipeline.tags(tags);
        }
        if let Some(previous) = self.previous {
            pipeline = pipeline.previous(previous);
        }
        Arc::new(SiteRuntime {
            site: self.site,
            base: self.base,
            store: self.store,
            generator: self.generator,
            host: self.host,
            pipeline: pipeline.build(),
            gate,
            quarantine: self.quarantine,
            media: self.media,
            work: self.work,
            approvals,
            charters: self.charters,
            facts: self.facts,
            verifier: self.verifier,
            crawler: self.crawler,
            audit: self.audit,
            clock: self.clock,
            drafts: Mutex::new(BTreeMap::new()),
            charter_drafts: Mutex::new(BTreeMap::new()),
            requests: SiteRequests::default(),
            next: std::sync::atomic::AtomicU64::new(1),
        })
    }
}

impl SiteRuntime {
    /// A runtime for `site` over `adk-site`'s parts: `base` carries the configuration,
    /// theme, terms and capabilities; items come from `store`; `work` holds builds.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runtime = SiteRuntime::builder(site, base, store, Arc::new(BuiltinGenerator), host, log, work)
    ///     .charters(charters)
    ///     .facts(facts, scope)
    ///     .build();
    /// ```
    pub fn builder(
        site: SiteRef,
        base: SiteProject,
        store: Arc<dyn ContentStore>,
        generator: Arc<dyn SiteGenerator>,
        host: Arc<dyn SiteHost>,
        log: Arc<SiteLog>,
        work: impl Into<PathBuf>,
    ) -> SiteRuntimeBuilder {
        SiteRuntimeBuilder {
            site,
            base,
            store,
            generator,
            host,
            log,
            work: work.into(),
            history: None,
            announcer: None,
            tags: None,
            previous: None,
            quarantine: Quarantine::default(),
            media: MediaPipeline::default(),
            charters: None,
            facts: None,
            verifier: None,
            crawler: None,
            audit: None,
            clock: Arc::new(SystemClock),
            gate: LaunchGate::new(),
        }
    }

    /// The site.
    pub fn site(&self) -> &SiteRef {
        &self.site
    }

    /// The publishing pipeline.
    pub fn pipeline(&self) -> &PublishingPipeline {
        &self.pipeline
    }

    /// The content store.
    pub fn store(&self) -> &Arc<dyn ContentStore> {
        &self.store
    }

    /// The host.
    pub fn host(&self) -> &Arc<dyn SiteHost> {
        &self.host
    }

    /// The release approval gate, where people record their approvals under a chain.
    pub fn approvals(&self) -> &Arc<SiteApprovals> {
        &self.approvals
    }

    /// Whether the site has been launched: a production release is logged or served
    /// (spec 3.5 A-I21).
    pub async fn launched(&self) -> bool {
        if self.pipeline.previous().await.is_some() {
            return true;
        }
        matches!(self.host.fetch(CHECKPOINT_PATH).await, Ok(Some(_)))
    }

    pub(crate) fn next_id(&self, prefix: &str) -> String {
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{prefix}-{n}")
    }

    /// Runs the launch gate's full tier over the working copy as it would publish now
    /// (spec 3.5 §7.8.1).
    ///
    /// # Errors
    ///
    /// Build and gate errors.
    pub async fn run_launch_gate(&self) -> Result<GateReport> {
        let mut project = self.base.clone();
        project.items = self.store.list().await.map_err(site_error)?;
        project.items.sort_by(|a, b| a.id.cmp(&b.id));
        for item in &project.items {
            for media in &item.media {
                if !project.media.contains_key(&media.path)
                    && let Some(bytes) =
                        self.store.read_media(&media.path).await.map_err(site_error)?
                {
                    project.media.insert(media.path.clone(), bytes);
                }
            }
        }
        project.as_of = self.clock.now();
        project.redirects_format = self.host.redirects_format();
        let out = self.work.join(self.next_id("gate"));
        let report = self.generator.build(&project, &out).await.map_err(site_error)?;
        let verdict = self
            .gate
            .run(
                GateMode::Full,
                &GateInput {
                    project: &project,
                    out: &out,
                    report: &report,
                    changed: None,
                    quarantine: &self.quarantine,
                },
            )
            .await
            .map_err(site_error);
        let _ = tokio::fs::remove_dir_all(&out).await;
        verdict
    }

    /// The charter signing requests that wait.
    pub async fn charter_requests(&self) -> Vec<CharterSignRequest> {
        self.requests.charters.lock().await.values().cloned().collect()
    }

    /// The promotion requests that wait for the owner.
    pub async fn promotion_requests(&self) -> Vec<PromotionRequest> {
        self.requests.promotions.lock().await.values().cloned().collect()
    }

    /// Records `approval` — a person signing in a role — on the charter request `id`, and
    /// commits the draft through `commit_audited` once every required signer has signed,
    /// each a distinct person (spec 3.5 §7.6, A-I23). No agent signs, and the agent that
    /// asked never commits.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown request; `Forbidden` for a signature no person made or in a
    /// role the signer does not hold; the store's errors.
    pub async fn sign_charter(
        &self,
        id: &str,
        approval: RoleApproval,
        decision_id: &str,
    ) -> Result<Option<CharterVersion>> {
        let charters = self.charters.as_ref().ok_or_else(|| {
            AdkError::config("signing a charter needs the site's charter store (spec 3.5 §7.6)")
        })?;
        let mut requests = self.requests.charters.lock().await;
        let request = requests.get_mut(id).ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.charter.request_not_found",
                format!("no charter signing request '{id}'"),
            )
        })?;
        if super::is_agent(&approval.principal) {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "website.charter.agent_signer",
                format!(
                    "{} is not a person: no agent signs a charter (spec 3.5 §7.6, A-I23)",
                    approval.principal.id
                ),
            ));
        }
        let governing = match charters.current(self.site.id).await.map_err(kernel_error)? {
            Some(current) => current.charter.governance,
            None => request.draft.governance.clone(),
        };
        if !governing.holds(&approval.principal.id, approval.role) {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "website.charter.not_a_signer",
                format!("{} does not hold the {:?} role", approval.principal.id, approval.role),
            ));
        }
        request.signatures.retain(|held| held.principal.id != approval.principal.id);
        request.signatures.push(approval);
        if check_chain(&governing, &adk_kernel::charter::CHARTER_ACTION, &request.signatures, None)
            .is_err()
        {
            return Ok(None);
        }
        let mut decision = CommitDecision::new(decision_id, self.clock.now());
        for signature in &request.signatures {
            decision = decision.with_approval(signature.clone());
        }
        let version =
            commit_audited(charters.as_ref(), request.draft.clone(), decision, self.audit.as_ref())
                .await
                .map_err(kernel_error)?;
        request.committed = Some(version.version());
        Ok(Some(version))
    }
}
