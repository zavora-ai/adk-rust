//! The site runtime the back-office tools act on: `adk-site`'s publishing pipeline, launch
//! gate, host and media pipeline, the charter store, and the decisions people make
//! (spec 3.5 §7.6, §7.8, §7.8.1, §7.8.2, A-I18, A-I23).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalAction, ApprovalPolicy, ApprovalScope, ApprovalService, ApprovalState,
    ApproverSelector, Clock, ErrorCategory, ErrorComponent, GovernanceControl,
    InMemoryApprovalStore, InboxQuery, Result, StoreApprovalService, SystemClock,
    ToolConfirmationRequest,
};
use adk_kernel::charter::{
    CHARTER_ACTION, CharterDirectory, CharterStore, CharterVersion, CommitDecision, RoleApproval,
    chain_policy, commit_audited, group_role,
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
    Announcer, ApprovalDecision, ApprovalGate, ApprovalRequest, ChangeTagSink, Draft, EntryPoint,
    Preview, PublishingPipeline,
};
use adk_site::release::PreviousRelease;
use async_trait::async_trait;
use awp_types::{Charter, ContentItem, Digest, Governance, Role};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::drafts::{
    DraftKind, InMemorySiteDraftStore, SiteDraftStore, StoredDraft, StoredRecord, conflict,
    store_error,
};
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

/// The tool name of a release's approval request (spec 3.5 §7.8.2 step 4).
pub const SITE_RELEASE_TOOL: &str = "site_release";

/// The tool name of a charter signing request (spec 3.5 §7.6).
pub const CHARTER_SIGN_TOOL: &str = "charter_sign";

/// The tool name of a job promotion request (spec 3.5 §7.11, A-I13).
pub const JOB_PROMOTION_TOOL: &str = "job_promotion";

/// The session the site's approval requests are kept under, in the site's application.
const APPROVAL_SESSION: &str = "site-approvals";

/// What a release vote binds to (spec 3.5 §7.8.2 step 4, A-I23): the site and the
/// [`change_digest`](adk_site::pipeline::change_digest) of the change — its action class,
/// proposer, items and media, and the release and generator it was previewed on.
///
/// A rebuild of the same change on the same base gives the same fingerprint, so a vote
/// survives a restart and a new preview; an edit, or another release becoming the base,
/// gives a new fingerprint and a new request, so earlier votes stop counting.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::ReleaseFingerprint;
/// use awp_types::Digest;
///
/// let site = uuid::Uuid::nil();
/// let change = Digest::from_sha256_bytes([7; 32]);
/// let fingerprint = ReleaseFingerprint::new(site, &change);
/// assert_eq!(fingerprint, ReleaseFingerprint::new(site, &change));
/// assert!(fingerprint.request_id(site).starts_with("site-release-"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReleaseFingerprint(String);

impl ReleaseFingerprint {
    /// The fingerprint of `change` on `site`.
    pub fn new(site: Uuid, change: &Digest) -> Self {
        use sha2::Digest as _;

        let digest = sha2::Sha256::digest(format!("{site}\u{1f}{change}").as_bytes());
        Self(Digest::from_sha256_bytes(digest.into()).to_string())
    }

    /// The fingerprint as `sha256:<hex>`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of the release's approval request: `site-release-{site}-{first 16 hex digits}`.
    pub fn request_id(&self, site: Uuid) -> String {
        let hex = self.0.trim_start_matches("sha256:");
        format!("site-release-{site}-{}", &hex[..16.min(hex.len())])
    }
}

/// The site's decisions people make, as requests in its approval service (spec 3.5 §7.6,
/// §7.8.2 step 4, §7.11, A-I23): release chains, charter signatures and job promotions.
///
/// | The charter | What decides a release |
/// |---|---|
/// | names an approval chain for the action class | one [`ApprovalRequest`](adk_core::ApprovalRequest) per [`ReleaseFingerprint`], with one slot per role of the chain, filled by distinct people who hold the role, none the proposer |
/// | names none | the governed path: the call ran because the job's level allowed it or the owner approved it, recorded as [`GOVERNED_APPROVER`] |
///
/// Every surface — the back office, the web desk, the kernel's inbox, a chat card — decides
/// the same request through the service, which takes each person's roles from the charter
/// in force ([`CharterDirectory`](adk_kernel::charter::CharterDirectory)), writes every vote
/// to its audit trail, and settles concurrent votes by compare-and-swap. An early approval
/// leaves the request pending and records no `site.content.approved`.
pub struct SiteApprovals {
    site: SiteRef,
    charters: Option<Arc<dyn CharterStore>>,
    service: Arc<dyn ApprovalService>,
}

impl std::fmt::Debug for SiteApprovals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteApprovals").field("site", &self.site).finish_non_exhaustive()
    }
}

impl SiteApprovals {
    /// The approval service the requests live in.
    pub fn service(&self) -> &Arc<dyn ApprovalService> {
        &self.service
    }

    /// The request `id`.
    ///
    /// # Errors
    ///
    /// The approval service's error.
    pub async fn get(&self, id: &str) -> Result<Option<adk_core::ApprovalRequest>> {
        self.service.get(id).await
    }

    /// Records `by`'s approval of the request `id` read at `version`: a release, a charter
    /// signature or a promotion (spec 3.5 A-I23).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let request = runtime.approvals().get(&ticket).await?.expect("the release's request");
    /// let after = runtime.approvals().approve(&ticket, request.version, &SubjectRef::human("eva")).await?;
    /// assert_eq!(after.state, ApprovalState::Pending, "the publisher has not approved yet");
    /// ```
    ///
    /// # Errors
    ///
    /// `website.approval.refused` for an agent; the service's `approval.not_an_approver` for
    /// a person the charter gives none of the request's roles, `approval.self_approval` for
    /// the proposer, `approval.already_decided` for a second approval by one person, and
    /// `approval.version_conflict` when the request changed since `version`.
    pub async fn approve(
        &self,
        id: &str,
        version: u64,
        by: &SubjectRef,
    ) -> Result<adk_core::ApprovalRequest> {
        refuse_agent(by)?;
        self.service.decide(id, version, adk_core::ApprovalDecision::approve(by.clone())).await
    }

    /// Records `by`'s refusal of the request `id` read at `version`; only a person who may
    /// approve it may refuse it.
    ///
    /// # Errors
    ///
    /// As [`approve`](Self::approve).
    pub async fn reject(
        &self,
        id: &str,
        version: u64,
        by: &SubjectRef,
        reason: &str,
    ) -> Result<adk_core::ApprovalRequest> {
        refuse_agent(by)?;
        self.service
            .decide(id, version, adk_core::ApprovalDecision::reject(by.clone(), reason))
            .await
    }

    /// The site's requests with `tool` in `states`, oldest first.
    ///
    /// # Errors
    ///
    /// The approval service's error.
    pub async fn list(
        &self,
        tool: &str,
        states: &[ApprovalState],
    ) -> Result<Vec<adk_core::ApprovalRequest>> {
        let mut query = InboxQuery::default()
            .with_app_name(self.site.name.clone())
            .with_states(states.iter().copied())
            .with_limit(adk_core::DEFAULT_APPROVAL_PAGE);
        let site = self.site.id.to_string();
        let mut requests = Vec::new();
        loop {
            let page = self.service.inbox(query.clone()).await?;
            requests.extend(page.items.into_iter().filter(|request| {
                request.tool_name == tool
                    && request.args_redacted.get("site").and_then(Value::as_str)
                        == Some(site.as_str())
            }));
            match page.next_cursor {
                Some(next) => query = query.with_cursor(Some(next)),
                None => return Ok(requests),
            }
        }
    }

    /// Submits the request `id` for `tool` with `args` under `policy`, or returns the one
    /// already submitted under `id`: a request is created once, so every replica and every
    /// retry reaches the same record.
    async fn submit(
        &self,
        id: String,
        tool: &str,
        args: Value,
        summary: String,
        policy: ApprovalPolicy,
        requester: SubjectRef,
    ) -> Result<adk_core::ApprovalRequest> {
        let owner = match self.governing().await? {
            Some(governance) => governance.holders(Role::Owner).next().map(str::to_string),
            None => None,
        };
        let scope = ApprovalScope::new(
            self.site.name.clone(),
            owner.unwrap_or_else(|| "owner".to_string()),
            APPROVAL_SESSION,
        );
        let call = ToolConfirmationRequest::new(tool, args);
        let request = adk_core::ApprovalRequest::new(id, scope, &call, policy)
            .with_summary(summary)
            .with_requester(requester)
            .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject]);
        self.service.submit(request).await
    }

    /// The governance of the charter in force, when the runtime knows the charters.
    async fn governing(&self) -> Result<Option<Governance>> {
        let Some(charters) = &self.charters else { return Ok(None) };
        let current = charters.current(self.site.id).await.map_err(kernel_error)?;
        Ok(current.map(|version| version.charter.governance))
    }
}

/// Refuses `by` when it is an agent: no agent approves, signs or holds a role (spec 3.5
/// §7.6, A-I23).
fn refuse_agent(by: &SubjectRef) -> Result<()> {
    if super::is_agent(by) {
        return Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Forbidden,
            "website.approval.refused",
            format!("{} is not a person, and no agent holds a role (spec 3.5 A-I23)", by.id),
        ));
    }
    Ok(())
}

/// The subject of `principal`: an agent for an agent identity, a person otherwise.
fn subject_of(principal: &str) -> SubjectRef {
    if adk_kernel::charter::is_agent_principal(principal) {
        SubjectRef::new(adk_core::principal::SubjectKind::Agent, principal)
    } else {
        SubjectRef::human(principal)
    }
}

/// The people who fill `request`'s slots, each with the role their slot names.
fn role_approvals(request: &adk_core::ApprovalRequest) -> Vec<RoleApproval> {
    request
        .slot_holders()
        .into_iter()
        .filter_map(|(slot, holder)| {
            let ApproverSelector::Group(group) = slot else { return None };
            let decision = holder?;
            Some(RoleApproval::new(
                group_role(group)?,
                decision.authority().clone(),
                decision.decided_at,
            ))
        })
        .collect()
}

#[async_trait]
impl ApprovalGate for SiteApprovals {
    async fn decide(&self, request: &ApprovalRequest) -> adk_site::Result<ApprovalDecision> {
        let pipeline = |error: AdkError| adk_site::SiteError::Pipeline(error.to_string());
        let action = awp_types::ActionClass::from(request.action_class.as_str());
        let governance = self.governing().await.map_err(pipeline)?.filter(|governance| {
            governance.approval_chains.iter().any(|chain| chain.action == action)
        });
        let Some(governance) = governance else {
            return Ok(ApprovalDecision::Approved {
                approvers: vec![GOVERNED_APPROVER.to_string()],
                decision: None,
            });
        };
        let fingerprint = ReleaseFingerprint::new(self.site.id, &request.change);
        let id = fingerprint.request_id(self.site.id);
        let items: Vec<&str> = request.items.iter().map(|item| item.0.as_str()).collect();
        let summary = match items.as_slice() {
            [one] => format!("Publish {one}"),
            many => format!("Publish {} items", many.len()),
        };
        let args = json!({
            "site": self.site.id,
            "actionClass": request.action_class,
            "proposer": request.proposer,
            "items": items,
            "fingerprint": fingerprint.as_str(),
            "diff": request.diff,
        });
        let stored = self
            .submit(
                id.clone(),
                SITE_RELEASE_TOOL,
                args,
                summary,
                chain_policy(&governance, &action),
                subject_of(&request.proposer),
            )
            .await
            .map_err(pipeline)?;
        Ok(match stored.state {
            ApprovalState::Pending => {
                tracing::info!(website.release.request = %id, website.release.open_slots = stored.open_slots().len(), "release waits for its approval chain");
                ApprovalDecision::Pending { ticket: id }
            }
            ApprovalState::Approved => ApprovalDecision::Approved {
                approvers: role_approvals(&stored)
                    .into_iter()
                    .map(|approval| approval.principal.id)
                    .collect(),
                decision: Some(id),
            },
            ApprovalState::Rejected | ApprovalState::Responded => {
                let last = stored.decisions.last();
                ApprovalDecision::Rejected {
                    by: last.map_or_else(String::new, |decision| decision.approver.id.clone()),
                    reason: last.and_then(|decision| decision.reason.clone()).unwrap_or_default(),
                }
            }
            ApprovalState::Expired | ApprovalState::Cancelled => ApprovalDecision::Rejected {
                by: "approval-service".to_string(),
                reason: stored
                    .note
                    .clone()
                    .unwrap_or_else(|| format!("the request {}", stored.state.as_str())),
            },
        })
    }
}

/// A charter signing request (spec 3.5 §7.6, §7.18 stage 5, A-I23): an approval request
/// with one slot per role of the charter-change chain, committed only when distinct people
/// fill every slot. Read from the approval service with [`SiteRuntime::charter_requests`];
/// a record of this shape in the draft store is converted by
/// [`SiteRuntime::restore_drafts`].
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
    /// The approval request's version, the compare-and-swap token for the next signature.
    #[serde(default)]
    pub version: u64,
}

impl CharterSignRequest {
    /// The view of the approval request `request`.
    fn of(request: &adk_core::ApprovalRequest) -> Result<Self> {
        let draft: Charter =
            read(request.args_redacted.get("charter").cloned().unwrap_or_default())?;
        Ok(Self {
            id: request.id.clone(),
            draft,
            roles: request
                .policy
                .slots
                .iter()
                .filter_map(|slot| match slot {
                    ApproverSelector::Group(group) => group_role(group),
                    ApproverSelector::Subject(_) => None,
                })
                .collect(),
            signatures: role_approvals(request),
            requested_by: request.requester.as_ref().map(|r| r.id.clone()).unwrap_or_default(),
            requested_at: request.created_at,
            committed: None,
            version: request.version,
        })
    }
}

/// A request that the owner promote a job (spec 3.5 §7.11, A-I13): the weekly review's
/// offer, an approval request decided only by an owner. Read from the approval service with
/// [`SiteRuntime::promotion_requests`].
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
    /// The approval request's version.
    #[serde(default)]
    pub version: u64,
}

impl PromotionRequest {
    /// The view of the approval request `request`.
    fn of(request: &adk_core::ApprovalRequest) -> Self {
        let text = |key: &str| {
            request.args_redacted.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
        };
        Self {
            id: request.id.clone(),
            job: text("job"),
            reason: text("reason"),
            requested_by: request.requester.as_ref().map(|r| r.id.clone()).unwrap_or_default(),
            requested_at: request.created_at,
            version: request.version,
        }
    }
}

/// Everything the back-office tools act on (spec 3.5 §7.8, §7.8.2): build one with
/// [`SiteRuntime::builder`].
pub struct SiteRuntime {
    pub(crate) site: SiteRef,
    pub(crate) base: SiteProject,
    pub(crate) store: Arc<dyn ContentStore>,
    pub(crate) generator: Arc<dyn SiteGenerator>,
    pub(crate) host: Arc<dyn SiteHost>,
    pub(crate) pipeline: Arc<PublishingPipeline>,
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
    /// The previews this process staged, by draft id, with the draft version each shows.
    pub(crate) previews: Mutex<BTreeMap<String, (u64, Preview)>>,
    /// Held across a release's preview, approval and completion in this process.
    pub(crate) publishing: Mutex<()>,
    pub(crate) draft_store: Arc<dyn SiteDraftStore>,
    pub(crate) governance: Option<GovernanceControl>,
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
    draft_store: Option<Arc<dyn SiteDraftStore>>,
    approvals: Option<Arc<dyn ApprovalService>>,
    governance: Option<GovernanceControl>,
    require_durable: bool,
}

impl SiteRuntimeBuilder {
    /// Keeps the site's decisions — release chains, charter signatures, job promotions — as
    /// requests in `service` (spec 3.5 §7.8.2 step 4, A-I23), such as the kernel's, so the
    /// back office, the web desk, the kernel's inbox and chat cards decide one record. The
    /// service must take its groups from a
    /// [`CharterDirectory`](adk_kernel::charter::CharterDirectory) for this site, so only the
    /// people the charter names fill a chain's roles.
    ///
    /// Without one the runtime keeps them in a [`StoreApprovalService`] in memory, with the
    /// directory, the runtime's clock and its audit trail.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let service = StoreApprovalService::new(Arc::new(approval_store))
    ///     .with_directory(Arc::new(CharterDirectory::new(charters.clone())))
    ///     .with_audit(trail.clone());
    /// let runtime = SiteRuntime::builder(site, base, store, generator, host, log, work)
    ///     .charters(charters)
    ///     .approvals(Arc::new(service))
    ///     .build()?;
    /// ```
    #[must_use]
    pub fn approvals(mut self, service: Arc<dyn ApprovalService>) -> Self {
        self.approvals = Some(service);
        self
    }

    /// Holds publishing while `control` is frozen (spec 3.5 §7.8.2, D5): a release whose
    /// approval chain is satisfied — by the back office or by a person in the desk — waits
    /// until the freeze lifts. Decisions are still recorded while frozen.
    #[must_use]
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Refuses to build with a store the runtime would keep in memory: the draft store and
    /// the approval service must be set, as a served site's are (spec 3.5 §7.8.2 step 1).
    #[must_use]
    pub fn require_durable(mut self) -> Self {
        self.require_durable = true;
        self
    }

    /// Keeps the back office's drafts — content drafts, charter drafts, charter signing and
    /// promotion requests — in `store`, so they survive a restart and every runtime over the
    /// store sees the same ones (spec 3.5 §7.8.2 step 1); an [`InMemorySiteDraftStore`] by
    /// default. The runtime reads the store at every call and writes it with a
    /// compare-and-swap, so two runtimes editing one draft never overwrite each other.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runtime = SiteRuntime::builder(site, base, store, generator, host, log, work)
    ///     .drafts(Arc::new(InMemorySiteDraftStore::default()))
    ///     .build()?;
    /// runtime.restore_drafts().await?;
    /// ```
    #[must_use]
    pub fn drafts(mut self, store: Arc<dyn SiteDraftStore>) -> Self {
        self.draft_store = Some(store);
        self
    }

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

    /// Where the `website.*` records go, with the pipeline's `site.content.*` and
    /// `site.release.*` records (spec 3.5 §9.3).
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
    ///
    /// # Errors
    ///
    /// `website.runtime.not_durable` under [`require_durable`](Self::require_durable) when
    /// the draft store or the approval service is not set.
    pub fn build(self) -> Result<Arc<SiteRuntime>> {
        if self.require_durable {
            let held: Vec<&str> = [
                (self.draft_store.is_none(), "drafts (SiteRuntimeBuilder::drafts)"),
                (self.approvals.is_none(), "approvals (SiteRuntimeBuilder::approvals)"),
            ]
            .into_iter()
            .filter_map(|(missing, store)| missing.then_some(store))
            .collect();
            if !held.is_empty() {
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::InvalidInput,
                    "website.runtime.not_durable",
                    format!(
                        "the site runtime would keep {} in memory, and a restart would lose \
                         them; set a SQLite, PostgreSQL or Redis store for each",
                        held.join(" and ")
                    ),
                ));
            }
        }
        let service = match self.approvals {
            Some(service) => service,
            None => {
                let mut service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                    .with_clock(self.clock.clone());
                if let Some(charters) = &self.charters {
                    let directory = CharterDirectory::new(charters.clone()).for_site(self.site.id);
                    service = service.with_directory(Arc::new(directory));
                }
                if let Some(trail) = &self.audit {
                    service = service.with_audit(trail.clone());
                }
                Arc::new(service) as Arc<dyn ApprovalService>
            }
        };
        let approvals = Arc::new(SiteApprovals {
            site: self.site.clone(),
            charters: self.charters.clone(),
            service,
        });
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
        // The pipeline writes the `site.content.*` and `site.release.*` records (spec 3.5 §9.3).
        if let Some(trail) = &self.audit {
            pipeline = pipeline.audit(trail.clone());
        }
        Ok(Arc::new(SiteRuntime {
            site: self.site,
            base: self.base,
            store: self.store,
            generator: self.generator,
            host: self.host,
            pipeline: Arc::new(pipeline.build()),
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
            previews: Mutex::new(BTreeMap::new()),
            publishing: Mutex::new(()),
            draft_store: self
                .draft_store
                .unwrap_or_else(|| Arc::new(InMemorySiteDraftStore::default())),
            governance: self.governance,
        }))
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
    ///     .build()?;
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
            draft_store: None,
            approvals: None,
            governance: None,
            require_durable: false,
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

    /// The publishing pipeline, shared: an `adk-site` `UpdateEngine` publishes through it,
    /// so its batches and the back office's releases follow one another (spec 3.5 §7.8.3).
    pub fn shared_pipeline(&self) -> Arc<PublishingPipeline> {
        Arc::clone(&self.pipeline)
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

    /// The newest content draft that entered through `entry`, with its id and its items
    /// (spec 3.5 §7.8.2 "Entry points", §5 S18): the draft a paired person's turn made, which
    /// the photos their message carried join. `None` when that entry has no draft waiting or
    /// the draft store cannot be read.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_site::pipeline::EntryPoint;
    ///
    /// # async fn example(runtime: &adk_harness::website::SiteRuntime) {
    /// let device = EntryPoint::PairedDevice { device: "ch-5c1e09a2d4b7f3e".into() };
    /// if let Some((draft, items)) = runtime.newest_draft_from(&device).await {
    ///     println!("{draft} drafts {}", items[0].id.0);
    /// }
    /// # }
    /// ```
    pub async fn newest_draft_from(
        &self,
        entry: &EntryPoint,
    ) -> Option<(String, Vec<awp_types::ContentItem>)> {
        let records = match self.draft_store.list(self.site.id, DraftKind::Content).await {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(website.site = %self.site.name, error = %error, "the draft store cannot be read");
                return None;
            }
        };
        // The store lists in creation order, so the last match is the newest.
        records.into_iter().rev().find_map(|record| {
            let draft = read::<StoredDraft>(record.value).ok()?;
            (draft.entry == *entry).then_some((record.id, draft.items))
        })
    }

    /// Whether the site has been launched: a production release is logged or served
    /// (spec 3.5 A-I21).
    pub async fn launched(&self) -> bool {
        if self.pipeline.previous().await.is_some() {
            return true;
        }
        matches!(self.host.fetch(CHECKPOINT_PATH).await, Ok(Some(_)))
    }

    /// Checks that the drafts the runtime's draft store holds for the site — content drafts
    /// and charter drafts — read back, and converts the charter signing and promotion
    /// requests an earlier version kept there into requests in the approval service
    /// (spec 3.5 §7.8.2 step 1, A-I23). Each becomes the request
    /// `site-charter-{site}-legacy-{id}` or `site-promotion-{site}-legacy-{id}`, keeps the
    /// signatures of people who still hold their role, and leaves the draft store; a request
    /// its signatures already satisfy is committed. A second run converts nothing twice. The
    /// runtime reads the store at every call, so nothing is copied into memory. Returns how
    /// many drafts and requests the store held.
    ///
    /// # Errors
    ///
    /// The draft store's or the approval service's error, or a stored record that does not
    /// read back.
    pub async fn restore_drafts(&self) -> Result<usize> {
        let site = self.site.id;
        let mut held = 0;
        for record in self.draft_store.list(site, DraftKind::Content).await? {
            read::<StoredDraft>(record.value)?.into_draft()?;
            held += 1;
        }
        for record in self.draft_store.list(site, DraftKind::Charter).await? {
            read::<Charter>(record.value)?;
            held += 1;
        }
        for record in self.draft_store.list(site, DraftKind::CharterSign).await? {
            let legacy: CharterSignRequest = read(record.value)?;
            if legacy.committed.is_none() {
                let id = format!("site-charter-{site}-legacy-{}", record.id);
                let mut request = self
                    .submit_charter_signing(id, None, legacy.draft, &legacy.requested_by)
                    .await?;
                for signature in legacy.signatures {
                    match self
                        .approvals
                        .approve(&request.id, request.version, &signature.principal)
                        .await
                    {
                        Ok(after) => request = after,
                        Err(error) => {
                            tracing::info!(website.request = %request.id, error = %error, "a signature that no longer counts was not carried over");
                        }
                    }
                }
                if request.state == ApprovalState::Approved {
                    self.commit_signed(&request).await?;
                }
            }
            self.draft_store.remove(site, DraftKind::CharterSign, &record.id).await?;
            held += 1;
        }
        for record in self.draft_store.list(site, DraftKind::Promotion).await? {
            let legacy: PromotionRequest = read(record.value)?;
            let id = format!("site-promotion-{site}-legacy-{}", record.id);
            self.submit_promotion(id, &legacy.job, &legacy.reason, &legacy.requested_by).await?;
            self.draft_store.remove(site, DraftKind::Promotion, &record.id).await?;
            held += 1;
        }
        if held > 0 {
            tracing::info!(website.site = %self.site.name, website.drafts.held = held, "drafts held in the draft store");
        }
        Ok(held)
    }

    /// Keeps `value` as a new record of `kind` under an id the store allocates.
    pub(crate) async fn create(
        &self,
        kind: DraftKind,
        value: &impl Serialize,
    ) -> Result<StoredRecord> {
        let value = serde_json::to_value(value).map_err(store_error)?;
        self.draft_store.create(self.site.id, kind, &value).await
    }

    /// The record `id` of `kind` and the version it was read at.
    pub(crate) async fn record<T: serde::de::DeserializeOwned>(
        &self,
        kind: DraftKind,
        id: &str,
    ) -> Result<Option<(u64, T)>> {
        match self.draft_store.get(self.site.id, kind, id).await? {
            Some(record) => Ok(Some((record.version, read(record.value)?))),
            None => Ok(None),
        }
    }

    /// Writes `value` over the record `id` of `kind` read at `version`.
    ///
    /// # Errors
    ///
    /// `website.drafts.conflict` when the record changed or was removed since it was read.
    pub(crate) async fn replace(
        &self,
        kind: DraftKind,
        id: &str,
        version: u64,
        value: &impl Serialize,
    ) -> Result<()> {
        let value = serde_json::to_value(value).map_err(store_error)?;
        if self.draft_store.replace(self.site.id, kind, id, &value, version).await? {
            Ok(())
        } else {
            Err(conflict(kind, id))
        }
    }

    /// The content draft `id` and the version it was read at.
    pub(crate) async fn content_draft(&self, id: &str) -> Result<Option<(u64, Draft)>> {
        match self.record::<StoredDraft>(DraftKind::Content, id).await? {
            Some((version, stored)) => Ok(Some((version, stored.into_draft()?))),
            None => Ok(None),
        }
    }

    /// Writes `draft` over the content draft `id` read at `version`; its preview no longer
    /// shows it.
    ///
    /// # Errors
    ///
    /// `website.drafts.conflict` when the draft changed or was removed since it was read.
    pub(crate) async fn replace_draft(&self, id: &str, version: u64, draft: &Draft) -> Result<()> {
        self.previews.lock().await.remove(id);
        self.replace(DraftKind::Content, id, version, &StoredDraft::of(draft)).await
    }

    /// Forgets the content draft `id`: it was published or refused. A failure is logged; the
    /// release stands.
    pub(crate) async fn forget_draft(&self, id: &str) {
        self.previews.lock().await.remove(id);
        if let Err(error) = self.draft_store.remove(self.site.id, DraftKind::Content, id).await {
            tracing::warn!(website.draft = id, error = %error, "a finished draft stays in the draft store");
        }
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
        let out = self.work.join(format!("gate-{}", Uuid::new_v4()));
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

    /// Whether publishing is held: the runtime's governance control is frozen (spec 3.5
    /// §7.8.2, D5). A release whose chain is satisfied waits until the freeze lifts.
    pub fn is_frozen(&self) -> bool {
        self.governance.as_ref().is_some_and(GovernanceControl::is_frozen)
    }

    /// Asks every required signer to sign the charter draft `draft` the back office keeps
    /// (spec 3.5 §7.6, §7.18 stage 5, A-I23), as [`request_charter`](Self::request_charter)
    /// does for its content.
    ///
    /// # Errors
    ///
    /// `website.charter.draft_not_found` for an unknown draft; as
    /// [`request_charter`](Self::request_charter).
    pub async fn request_charter_signing(
        &self,
        draft: &str,
        requested_by: &str,
    ) -> Result<adk_core::ApprovalRequest> {
        let (_, charter) =
            self.record::<Charter>(DraftKind::Charter, draft).await?.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::InvalidInput,
                    "website.charter.draft_not_found",
                    format!("no charter draft '{draft}'"),
                )
            })?;
        self.request_charter(&charter, Some(draft), requested_by).await
    }

    /// Asks every required signer to sign `charter` as the site's next version (spec 3.5
    /// §7.6, §7.18 stage 5, A-I23): one approval request per content and version in force,
    /// with a slot for each role of the charter-change chain of the charter in force — or of
    /// `charter`, for a site's first. The back office and the web desk asking for the same
    /// content reach the same request; an edit gets a new one, so earlier signatures stop
    /// counting.
    ///
    /// # Errors
    ///
    /// `website.charter.invalid` for a charter that fails `Charter::validate()`; the stores'
    /// errors.
    pub async fn request_charter(
        &self,
        charter: &Charter,
        draft: Option<&str>,
        requested_by: &str,
    ) -> Result<adk_core::ApprovalRequest> {
        if let Err(error) = charter.validate() {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "website.charter.invalid",
                format!("the draft cannot be committed: {error}"),
            ));
        }
        let id = self.charter_request_id(charter).await?;
        self.submit_charter_signing(id, draft, charter.clone(), requested_by).await
    }

    /// The signing request for `charter`, when one was asked for, with the version it
    /// committed.
    ///
    /// # Errors
    ///
    /// The stores' errors.
    pub async fn charter_request(&self, charter: &Charter) -> Result<Option<CharterSignRequest>> {
        let id = self.charter_request_id(charter).await?;
        let Some(request) = self.approvals.get(&id).await? else { return Ok(None) };
        let mut view = CharterSignRequest::of(&request)?;
        if let Some(charters) = &self.charters {
            view.committed = charters
                .history(self.site.id)
                .await
                .map_err(kernel_error)?
                .iter()
                .find(|version| version.decision.decision_id == id)
                .map(CharterVersion::version);
        }
        Ok(Some(view))
    }

    /// The id of the signing request for `charter` as the next version:
    /// `site-charter-{site}-v{next}-{first 16 hex digits of the content's digest}`.
    async fn charter_request_id(&self, charter: &Charter) -> Result<String> {
        let next = match &self.charters {
            Some(charters) => charters
                .current(self.site.id)
                .await
                .map_err(kernel_error)?
                .map_or(1, |current| current.version() + 1),
            None => 1,
        };
        let content = serde_json::to_vec(charter).map_err(store_error)?;
        let digest = adk_site::release::file_digest(&content).to_string();
        let hex = digest.trim_start_matches("sha256:");
        Ok(format!("site-charter-{}-v{next}-{}", self.site.id, &hex[..16.min(hex.len())]))
    }

    /// Submits the charter signing request `id` for `charter`.
    async fn submit_charter_signing(
        &self,
        id: String,
        draft: Option<&str>,
        charter: Charter,
        requested_by: &str,
    ) -> Result<adk_core::ApprovalRequest> {
        let governing = match &self.charters {
            Some(charters) => charters
                .current(self.site.id)
                .await
                .map_err(kernel_error)?
                .map(|current| current.charter.governance),
            None => None,
        }
        .unwrap_or_else(|| charter.governance.clone());
        let summary = match &charter.mission {
            mission if mission.is_empty() => "Sign the charter".to_string(),
            mission => format!("Sign the charter: {mission}"),
        };
        let args = json!({ "site": self.site.id, "draft": draft, "charter": charter });
        self.approvals
            .submit(
                id,
                CHARTER_SIGN_TOOL,
                args,
                summary,
                chain_policy(&governing, &CHARTER_ACTION),
                subject_of(requested_by),
            )
            .await
    }

    /// Asks the owner to promote `job` (spec 3.5 §7.11, A-I13): an approval request only an
    /// owner decides. While one waits for the job, asking again returns it.
    ///
    /// # Errors
    ///
    /// The approval service's error.
    pub async fn request_promotion(
        &self,
        job: &str,
        reason: &str,
        requested_by: &str,
    ) -> Result<adk_core::ApprovalRequest> {
        let waiting = self.approvals.list(JOB_PROMOTION_TOOL, &[ApprovalState::Pending]).await?;
        if let Some(open) = waiting
            .into_iter()
            .find(|request| request.args_redacted.get("job").and_then(Value::as_str) == Some(job))
        {
            return Ok(open);
        }
        let id = format!("site-promotion-{}-{}", self.site.id, Uuid::new_v4().simple());
        self.submit_promotion(id, job, reason, requested_by).await
    }

    /// Submits the promotion request `id`.
    async fn submit_promotion(
        &self,
        id: String,
        job: &str,
        reason: &str,
        requested_by: &str,
    ) -> Result<adk_core::ApprovalRequest> {
        let owner = ApproverSelector::group(adk_kernel::charter::role_group(Role::Owner));
        self.approvals
            .submit(
                id,
                JOB_PROMOTION_TOOL,
                json!({ "site": self.site.id, "job": job, "reason": reason }),
                format!("Promote the {job} job: {reason}"),
                ApprovalPolicy::for_slots([owner]),
                subject_of(requested_by),
            )
            .await
    }

    /// The charter signing requests that wait.
    pub async fn charter_requests(&self) -> Vec<CharterSignRequest> {
        match self.approvals.list(CHARTER_SIGN_TOOL, &[ApprovalState::Pending]).await {
            Ok(requests) => requests
                .iter()
                .filter_map(|request| match CharterSignRequest::of(request) {
                    Ok(view) => Some(view),
                    Err(error) => {
                        tracing::warn!(website.request = %request.id, error = %error, "a charter signing request does not read back");
                        None
                    }
                })
                .collect(),
            Err(error) => {
                tracing::warn!(website.site = %self.site.name, error = %error, "the approval service cannot be read");
                Vec::new()
            }
        }
    }

    /// The promotion requests that wait for the owner.
    pub async fn promotion_requests(&self) -> Vec<PromotionRequest> {
        match self.approvals.list(JOB_PROMOTION_TOOL, &[ApprovalState::Pending]).await {
            Ok(requests) => requests.iter().map(PromotionRequest::of).collect(),
            Err(error) => {
                tracing::warn!(website.site = %self.site.name, error = %error, "the approval service cannot be read");
                Vec::new()
            }
        }
    }

    /// Records `approval` — a person signing in a role — on the charter signing request
    /// `id`, and commits the draft through `commit_audited` once distinct people fill every
    /// role of the chain (spec 3.5 §7.6, A-I23). The commit's decision is the request. No
    /// agent signs, and the agent that asked never commits.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown request; `Forbidden` for a signature no person made or in a
    /// role the signer does not hold; the approval service's errors, such as
    /// `approval.already_decided` for a second signature by one person; the stores' errors.
    pub async fn sign_charter(
        &self,
        id: &str,
        approval: RoleApproval,
    ) -> Result<Option<CharterVersion>> {
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
        let charters = self.charters.as_ref().ok_or_else(|| {
            AdkError::config("signing a charter needs the site's charter store (spec 3.5 §7.6)")
        })?;
        let request = self
            .approvals
            .get(id)
            .await?
            .filter(|request| request.tool_name == CHARTER_SIGN_TOOL)
            .ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::NotFound,
                    "website.charter.request_not_found",
                    format!("no charter signing request '{id}'"),
                )
            })?;
        let governing = match charters.current(self.site.id).await.map_err(kernel_error)? {
            Some(current) => current.charter.governance,
            None => CharterSignRequest::of(&request)?.draft.governance,
        };
        if !governing.holds(&approval.principal.id, approval.role) {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "website.charter.not_a_signer",
                format!("{} does not hold the {:?} role", approval.principal.id, approval.role),
            ));
        }
        let signed = self.approvals.approve(id, request.version, &approval.principal).await?;
        if signed.state != ApprovalState::Approved {
            return Ok(None);
        }
        self.commit_signed(&signed).await
    }

    /// Commits every charter whose signing request people approved on any surface — the
    /// desk, the kernel's inbox, a chat card — and that is not committed yet. Returns the
    /// versions committed now.
    ///
    /// # Errors
    ///
    /// The stores' errors.
    pub async fn settle_charters(&self) -> Result<Vec<CharterVersion>> {
        let mut committed = Vec::new();
        for request in self.approvals.list(CHARTER_SIGN_TOOL, &[ApprovalState::Approved]).await? {
            committed.extend(self.commit_signed(&request).await?);
        }
        Ok(committed)
    }

    /// Commits the charter of the approved signing request `request`, its signers as the
    /// decision's approvals and the request as the decision; `None` when a version already
    /// carries that decision.
    async fn commit_signed(
        &self,
        request: &adk_core::ApprovalRequest,
    ) -> Result<Option<CharterVersion>> {
        let charters = self.charters.as_ref().ok_or_else(|| {
            AdkError::config("committing a charter needs the site's charter store (spec 3.5 §7.6)")
        })?;
        let committed = || async {
            let history = charters.history(self.site.id).await.map_err(kernel_error)?;
            Ok::<_, AdkError>(
                history.iter().any(|version| version.decision.decision_id == request.id),
            )
        };
        if committed().await? {
            return Ok(None);
        }
        let view = CharterSignRequest::of(request)?;
        let mut decision = CommitDecision::new(request.id.clone(), self.clock.now());
        for signature in view.signatures {
            decision = decision.with_approval(signature);
        }
        match commit_audited(charters.as_ref(), view.draft, decision, self.audit.as_ref()).await {
            Ok(version) => Ok(Some(version)),
            // Another replica committed the same decision first.
            Err(_) if committed().await? => Ok(None),
            Err(error) => Err(kernel_error(error)),
        }
    }
}

/// Reads a stored record as `T`.
fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(store_error)
}
