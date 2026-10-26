//! Earned autonomy per job (spec 3.5 §7.11, A-I13).

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{Clock, SystemClock};
use async_trait::async_trait;
use awp_types::{AutonomyLevel, Charter, Role};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::store::{CharterStore, is_agent_principal};
use super::{CharterEvent, CharterEventSink, WEBSITE_AUDIT_KIND, audit, emit, record};
use crate::error::{KernelError, Result};

/// The autonomy ladder, lowest first (spec 3.5 §7.11).
///
/// "Act, reversible only" is Auto over a subset of actions — rollback, read-only, drafts —
/// so it sits between "act with approval", where every non-read call is held, and "act
/// within budget", Auto for the job's tool classes.
const LADDER: [AutonomyLevel; 5] = [
    AutonomyLevel::Observe,
    AutonomyLevel::Propose,
    AutonomyLevel::ActWithApproval,
    AutonomyLevel::ActReversibleOnly,
    AutonomyLevel::ActWithinBudget,
];

/// The rung of `level` on the autonomy ladder, 0 for Observe up to 4 for "act within budget"
/// (spec 3.5 §7.11); a promotion moves up, a demotion one rung down.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::ladder_rank;
/// use awp_types::AutonomyLevel;
///
/// assert!(ladder_rank(AutonomyLevel::Propose) < ladder_rank(AutonomyLevel::ActWithApproval));
/// assert!(ladder_rank(AutonomyLevel::ActReversibleOnly) < ladder_rank(AutonomyLevel::ActWithinBudget));
/// ```
pub fn ladder_rank(level: AutonomyLevel) -> usize {
    LADDER.iter().position(|rung| *rung == level).unwrap_or(0)
}

/// The rung below `level`, `None` at Observe.
fn below(level: AutonomyLevel) -> Option<AutonomyLevel> {
    ladder_rank(level).checked_sub(1).map(|rank| LADDER[rank])
}

/// The rung above `level`, `None` at the top.
fn above(level: AutonomyLevel) -> Option<AutonomyLevel> {
    LADDER.get(ladder_rank(level) + 1).copied()
}

/// An owner's decision, the proof a promotion takes (spec 3.5 §7.11, A-I13): its id, the
/// owner who made it, when, and the signed note that seals it.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::charter::OwnerDecision;
///
/// let decision = OwnerDecision::new("decision-7", SubjectRef::human("amina"), chrono::Utc::now())
///     .with_signed_note("note:sha256:cd34");
/// assert!(decision.check_person().is_ok());
/// let agent = OwnerDecision::new("decision-8", SubjectRef::human("urn:adk:agent:example/steward"), chrono::Utc::now());
/// assert!(agent.check_person().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct OwnerDecision {
    /// The decision's id.
    pub decision_id: String,
    /// The owner who made it.
    pub owner: SubjectRef,
    /// When.
    pub at: DateTime<Utc>,
    /// The signed note sealing it, by reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_note: Option<String>,
}

impl OwnerDecision {
    /// A decision `decision_id` by `owner` at `at`.
    pub fn new(decision_id: impl Into<String>, owner: SubjectRef, at: DateTime<Utc>) -> Self {
        Self { decision_id: decision_id.into(), owner, at, signed_note: None }
    }

    /// Sets the signed note's reference.
    #[must_use]
    pub fn with_signed_note(mut self, note: impl Into<String>) -> Self {
        self.signed_note = Some(note.into());
        self
    }

    /// Refuses a decision no person made: a blank id, a subject not of kind `Human`, or an
    /// agent identity (A-I13).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`] naming the fault.
    pub fn check_person(&self) -> Result<()> {
        if self.decision_id.trim().is_empty() {
            return Err(KernelError::Unauthorized {
                reason: "an owner decision needs its decision id".into(),
            });
        }
        if self.owner.kind != SubjectKind::Human || is_agent_principal(&self.owner.id) {
            return Err(KernelError::Unauthorized {
                reason: format!(
                    "{} is not a person: only an owner's decision raises a job's autonomy, and \
                     the site can only lower it (A-I13)",
                    self.owner.id
                ),
            });
        }
        Ok(())
    }
}

/// How the owner decided one proposal of a job (spec 3.5 §7.11): what promotion and the
/// two-rejections rule count.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::DecisionOutcome;
///
/// assert_eq!(serde_json::to_value(DecisionOutcome::ApprovedUnchanged).unwrap(), "approvedUnchanged");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DecisionOutcome {
    /// Approved as proposed.
    ApprovedUnchanged,
    /// Approved after the owner edited it.
    ApprovedWithEdits,
    /// Rejected.
    Rejected,
}

/// One owner decision on one of a job's proposals (spec 3.5 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::charter::{DecisionOutcome, JobDecision};
///
/// let decision = JobDecision::new(uuid::Uuid::nil(), "review-replies", "approval-1", DecisionOutcome::Rejected, SubjectRef::human("amina"), chrono::Utc::now());
/// assert_eq!(decision.job, "review-replies");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct JobDecision {
    /// The site.
    pub site: Uuid,
    /// The job.
    pub job: String,
    /// The decision's id, such as the approval request's; recorded once.
    pub id: String,
    /// How the owner decided.
    pub outcome: DecisionOutcome,
    /// Who decided.
    pub by: SubjectRef,
    /// When.
    pub at: DateTime<Utc>,
}

impl JobDecision {
    /// A decision `id` on a proposal of `job`.
    pub fn new(
        site: Uuid,
        job: impl Into<String>,
        id: impl Into<String>,
        outcome: DecisionOutcome,
        by: SubjectRef,
        at: DateTime<Utc>,
    ) -> Self {
        Self { site, job: job.into(), id: id.into(), outcome, by, at }
    }
}

/// Why a job's autonomy changed (spec 3.5 §7.11): the weekly review says it in these words.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::AutonomyReason;
///
/// let reason = AutonomyReason::GuardrailBreach { guardrail: "unsubscribes".into() };
/// assert_eq!(reason.describe(), "the unsubscribes guardrail was breached");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum AutonomyReason {
    /// The job's level from the charter's authority when it was first registered.
    Initial {
        /// The charter version that set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        charter_version: Option<u32>,
    },
    /// An owner raised it (A-I13).
    Promoted {
        /// The owner's decision.
        decision: OwnerDecision,
    },
    /// A guardrail breach lowered it one level.
    GuardrailBreach {
        /// The guardrail.
        guardrail: String,
    },
    /// Two owner rejections in a row lowered it one level.
    OwnerRejections {
        /// The two decisions.
        decisions: Vec<String>,
    },
    /// An incident in the job's area lowered it one level.
    Incident {
        /// The area.
        area: String,
        /// The incident.
        incident: String,
    },
    /// The site, or a charter that sets a lower level, lowered it.
    Lowered {
        /// Why.
        reason: String,
    },
}

impl AutonomyReason {
    /// The reason in words.
    pub fn describe(&self) -> String {
        match self {
            Self::Initial { charter_version: Some(version) } => {
                format!("set by charter version {version}")
            }
            Self::Initial { charter_version: None } => "registered".to_string(),
            Self::Promoted { decision } => {
                format!("promoted by {}'s decision {}", decision.owner.id, decision.decision_id)
            }
            Self::GuardrailBreach { guardrail } => {
                format!("the {guardrail} guardrail was breached")
            }
            Self::OwnerRejections { decisions } => {
                format!("the owner rejected two proposals in a row ({})", decisions.join(", "))
            }
            Self::Incident { area, incident } => format!("incident {incident} in {area}"),
            Self::Lowered { reason } => reason.clone(),
        }
    }
}

/// A job's autonomy now (spec 3.5 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::JobAutonomy;
/// use awp_types::AutonomyLevel;
///
/// let job = JobAutonomy::new(uuid::Uuid::nil(), "seo", AutonomyLevel::Propose, chrono::Utc::now()).with_area("be-found");
/// assert_eq!(job.revision, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct JobAutonomy {
    /// The site.
    pub site: Uuid,
    /// The job.
    pub job: String,
    /// Its level.
    pub level: AutonomyLevel,
    /// The area of its work (§7.20: `run-and-protect`, `be-found`, `serve`, `grow`, `money`,
    /// `account`), which an incident's demotion matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// When it entered `level`.
    pub since: DateTime<Utc>,
    /// The number of changes so far, 1 after registration.
    pub revision: u64,
}

impl JobAutonomy {
    /// A newly registered job at `level`, revision 1.
    pub fn new(
        site: Uuid,
        job: impl Into<String>,
        level: AutonomyLevel,
        at: DateTime<Utc>,
    ) -> Self {
        Self { site, job: job.into(), level, area: None, since: at, revision: 1 }
    }

    /// Sets the job's area.
    #[must_use]
    pub fn with_area(mut self, area: impl Into<String>) -> Self {
        self.area = Some(area.into());
        self
    }
}

/// One change of a job's autonomy, kept as its history (spec 3.5 §7.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AutonomyChange {
    /// The site.
    pub site: Uuid,
    /// The job.
    pub job: String,
    /// The job's revision this change makes.
    pub revision: u64,
    /// The level it left; `None` at registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<AutonomyLevel>,
    /// The level it entered.
    pub to: AutonomyLevel,
    /// Why.
    pub reason: AutonomyReason,
    /// Who changed it: the owner for a promotion, the site for a demotion.
    pub by: SubjectRef,
    /// When.
    pub at: DateTime<Utc>,
}

/// Whether a job has earned a promotion (spec 3.5 §7.11): `streak` unchanged approvals in a
/// row within the policy's window since its last change, of `needed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PromotionEligibility {
    /// The site.
    pub site: Uuid,
    /// The job.
    pub job: String,
    /// Its level.
    pub level: AutonomyLevel,
    /// The level a promotion would give it; `None` at the top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<AutonomyLevel>,
    /// The unchanged approvals in a row within the window.
    pub streak: usize,
    /// How many it needs.
    pub needed: usize,
    /// Whether the weekly review offers the promotion.
    pub eligible: bool,
}

/// When a job is offered a promotion (spec 3.5 §7.11): `approvals` unchanged approvals in a
/// row (20) within `within` (30 days), with no breach since its last change.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::PromotionPolicy;
///
/// let policy = PromotionPolicy::default();
/// assert_eq!((policy.approvals, policy.within.num_days()), (20, 30));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PromotionPolicy {
    /// Unchanged approvals in a row a promotion needs.
    pub approvals: usize,
    /// The window they fall within.
    #[serde(rename = "withinMs", with = "chrono_millis")]
    pub within: Duration,
}

impl PromotionPolicy {
    /// A policy of `approvals` within `within`.
    pub fn new(approvals: usize, within: Duration) -> Self {
        Self { approvals, within }
    }
}

impl Default for PromotionPolicy {
    fn default() -> Self {
        Self { approvals: 20, within: Duration::days(30) }
    }
}

mod chrono_millis {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &chrono::Duration,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_i64(value.num_milliseconds())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<chrono::Duration, D::Error> {
        Ok(chrono::Duration::milliseconds(i64::deserialize(d)?))
    }
}

/// Every job's autonomy, its history and the owner's decisions on its proposals (spec 3.5
/// §7.11), on in-memory, SQLite, PostgreSQL and Redis.
///
/// [`apply`](Self::apply) is a compare-and-set on the job's revision, so two replicas
/// changing one job move it once.
#[async_trait]
pub trait AutonomyStore: Send + Sync {
    /// The job's autonomy, or `None` for a job never registered.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn get(&self, site: Uuid, job: &str) -> Result<Option<JobAutonomy>>;

    /// Every job of the site, by job id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn list(&self, site: Uuid) -> Result<Vec<JobAutonomy>>;

    /// Writes `record` and appends `change` when `record.revision` is one past the stored
    /// revision (1 for a job with none) and `change` makes that revision; `false`, writing
    /// nothing, otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when `change` does not match `record`, or
    /// [`KernelError::Store`] when the backend fails.
    async fn apply(&self, record: JobAutonomy, change: AutonomyChange) -> Result<bool>;

    /// The job's changes, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn history(&self, site: Uuid, job: &str) -> Result<Vec<AutonomyChange>>;

    /// Records `decision`; `false` when the job already has a decision with its id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn record_decision(&self, decision: JobDecision) -> Result<bool>;

    /// The job's decisions made at or after `since`, oldest first (by time, then id).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn decisions(
        &self,
        site: Uuid,
        job: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<JobDecision>>;
}

/// Refuses an `apply` whose change does not describe its record.
pub(crate) fn check_apply(record: &JobAutonomy, change: &AutonomyChange) -> Result<()> {
    if record.site != change.site
        || record.job != change.job
        || record.revision != change.revision
        || record.level != change.to
        || record.revision == 0
    {
        return Err(KernelError::Config {
            reason: format!(
                "an autonomy change of job {} must make the record's revision {} and level",
                record.job, record.revision
            ),
        });
    }
    Ok(())
}

/// The autonomy ladder of a site's jobs (spec 3.5 §7.11, A-I13): promotion only by an
/// owner's decision, demotion one level by the site on a guardrail breach, two owner
/// rejections in a row, or an incident in the job's area, each with its reason.
///
/// | Call | Who | Effect |
/// |---|---|---|
/// | [`register`](Self::register), [`sync`](Self::sync) | the site | A job enters at its charter level; a charter that sets a lower level lowers it; a higher one changes nothing |
/// | [`promote`](Self::promote), [`promote_to`](Self::promote_to) | an owner, with an [`OwnerDecision`] | One level up, or to a named higher level; refused for anyone else |
/// | [`lower`](Self::lower) | the site | One level down |
/// | [`guardrail_breach`](Self::guardrail_breach), [`incident`](Self::incident) | the site | One level down |
/// | [`record_decision`](Self::record_decision) | the owner's inbox | Counts towards promotion; the second rejection in a row lowers one level |
/// | [`eligibility`](Self::eligibility), [`offers`](Self::offers) | the weekly review | The promotions the record earned |
///
/// A promotion's change carries the owner's decision, from which the harness creates the
/// standing rule (spec 3.5 §6 D-7). Every change is audited as `website.job.promoted` or
/// `website.job.demoted` and emitted as [`CharterEvent::AutonomyChanged`].
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::charter::{AutonomyLadder, InMemoryAutonomyStore, OwnerDecision};
/// use awp_types::AutonomyLevel;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new()));
/// let site = uuid::Uuid::nil();
/// ladder.register(site, "review-replies", AutonomyLevel::Propose, Some("serve")).await?;
/// let agent = OwnerDecision::new("d-1", SubjectRef::service("urn:adk:agent:example/steward"), chrono::Utc::now());
/// assert!(ladder.promote(site, "review-replies", &agent).await.is_err(), "the site never raises itself");
/// let owner = OwnerDecision::new("d-2", SubjectRef::human("amina"), chrono::Utc::now());
/// assert_eq!(ladder.promote(site, "review-replies", &owner).await?.level, AutonomyLevel::ActWithApproval);
/// assert_eq!(ladder.guardrail_breach(site, "review-replies", "complaints").await?.unwrap().level, AutonomyLevel::Propose);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct AutonomyLadder {
    store: Arc<dyn AutonomyStore>,
    charters: Option<Arc<dyn CharterStore>>,
    policy: PromotionPolicy,
    clock: Arc<dyn Clock>,
    audit: Option<AuditTrail>,
    events: Option<Arc<dyn CharterEventSink>>,
}

impl std::fmt::Debug for AutonomyLadder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutonomyLadder")
            .field("policy", &self.policy)
            .field("charters", &self.charters.is_some())
            .finish_non_exhaustive()
    }
}

/// The subject the site's own demotions are recorded under.
const SITE_SUBJECT: &str = "kernel:autonomy";

/// How many times a change is retried against concurrent writers.
const RETRIES: usize = 8;

impl AutonomyLadder {
    /// A ladder over `store` with the default promotion policy, no charter store (an owner
    /// decision is then checked for a person only), the system clock, no audit and no events.
    pub fn new(store: Arc<dyn AutonomyStore>) -> Self {
        Self {
            store,
            charters: None,
            policy: PromotionPolicy::default(),
            clock: Arc::new(SystemClock),
            audit: None,
            events: None,
        }
    }

    /// Checks owner decisions against the governance of the charter in force: the decider
    /// must hold the owner role.
    #[must_use]
    pub fn with_charters(mut self, charters: Arc<dyn CharterStore>) -> Self {
        self.charters = Some(charters);
        self
    }

    /// Sets the promotion policy.
    #[must_use]
    pub fn with_policy(mut self, policy: PromotionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Reads time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Audits every change on `trail`.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Emits every change on `sink`.
    #[must_use]
    pub fn with_events(mut self, sink: Arc<dyn CharterEventSink>) -> Self {
        self.events = Some(sink);
        self
    }

    /// The store.
    pub fn store(&self) -> &Arc<dyn AutonomyStore> {
        &self.store
    }

    /// The promotion policy.
    pub fn policy(&self) -> &PromotionPolicy {
        &self.policy
    }

    /// Registers `job` at `level` in `area` unless it is registered; returns its record.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn register(
        &self,
        site: Uuid,
        job: &str,
        level: AutonomyLevel,
        area: Option<&str>,
    ) -> Result<JobAutonomy> {
        self.enter(site, job, level, area, None).await
    }

    async fn enter(
        &self,
        site: Uuid,
        job: &str,
        level: AutonomyLevel,
        area: Option<&str>,
        charter_version: Option<u32>,
    ) -> Result<JobAutonomy> {
        if let Some(existing) = self.store.get(site, job).await? {
            return Ok(existing);
        }
        let now = self.clock.now();
        let mut record = JobAutonomy::new(site, job, level, now);
        record.area = area.map(str::to_string);
        let change = AutonomyChange {
            site,
            job: job.to_string(),
            revision: 1,
            from: None,
            to: level,
            reason: AutonomyReason::Initial { charter_version },
            by: SubjectRef::service(SITE_SUBJECT),
            at: now,
        };
        if self.store.apply(record.clone(), change).await? {
            return Ok(record);
        }
        self.store.get(site, job).await?.ok_or_else(|| missing(job))
    }

    /// Brings the jobs of `charter`'s authority onto the ladder (spec 3.5 §7.11): a job not
    /// registered enters at the charter's level; a registered job above the charter's level
    /// is lowered to it, unless an owner promoted it there after the charter took effect; a
    /// job below it stays where it is, since only an owner's [`promote`](Self::promote)
    /// raises a level.
    ///
    /// The charter took effect when its owner signed it — the committing decision's time —
    /// or, for an unsigned charter, when the charter store committed it; a charter with
    /// neither lowers every job above it.
    /// Syncing the charter in force again — as every start of a site does — so keeps the
    /// owner's promotions (A-I13), and a later charter that sets a lower level still lowers.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn sync(&self, charter: &Charter) -> Result<Vec<JobAutonomy>> {
        let effective = match (&charter.signed, &self.charters) {
            (Some(signature), _) => Some(signature.signed_at),
            (None, Some(charters)) => charters
                .get(charter.site_id, charter.version)
                .await?
                .map(|version| version.committed_at),
            (None, None) => None,
        };
        let mut jobs = Vec::new();
        for authority in &charter.authority {
            let record = self
                .enter(
                    charter.site_id,
                    &authority.job,
                    authority.level,
                    None,
                    Some(charter.version),
                )
                .await?;
            let promoted = match effective {
                Some(since) => {
                    self.promoted_since(charter.site_id, &authority.job, record.level, since)
                        .await?
                }
                None => false,
            };
            let record = if ladder_rank(record.level) > ladder_rank(authority.level) && !promoted {
                let reason = AutonomyReason::Lowered {
                    reason: format!("charter version {} sets it lower", charter.version),
                };
                self.change(
                    charter.site_id,
                    &authority.job,
                    |_| Some(authority.level),
                    reason,
                    None,
                )
                .await?
                .unwrap_or(record)
            } else {
                record
            };
            jobs.push(record);
        }
        Ok(jobs)
    }

    /// Whether an owner promoted `job` to `level` or above at or after `since`.
    async fn promoted_since(
        &self,
        site: Uuid,
        job: &str,
        level: AutonomyLevel,
        since: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(self.store.history(site, job).await?.iter().any(|change| {
            matches!(change.reason, AutonomyReason::Promoted { .. })
                && change.at >= since
                && ladder_rank(change.to) >= ladder_rank(level)
        }))
    }

    /// The job's level, or `None` for a job never registered.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn level(&self, site: Uuid, job: &str) -> Result<Option<AutonomyLevel>> {
        Ok(self.store.get(site, job).await?.map(|record| record.level))
    }

    /// Raises `job` one level by `decision` (A-I13). See [`promote_to`](Self::promote_to).
    ///
    /// # Errors
    ///
    /// As [`promote_to`](Self::promote_to); also [`KernelError::Config`] at the top rung.
    pub async fn promote(
        &self,
        site: Uuid,
        job: &str,
        decision: &OwnerDecision,
    ) -> Result<JobAutonomy> {
        let record = self.store.get(site, job).await?.ok_or_else(|| missing(job))?;
        let to = above(record.level).ok_or_else(|| KernelError::Config {
            reason: format!("job {job} is at {:?}, the top of the ladder", record.level),
        })?;
        self.promote_to(site, job, to, decision).await
    }

    /// Raises `job` to `to` by `decision` (spec 3.5 §7.11, A-I13). The decision must be a
    /// person's ([`OwnerDecision::check_person`]); with a charter store, that person must hold
    /// the owner role in the charter in force. The change carries the decision, from which
    /// the harness creates the standing rule.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`] for a decision no owner made,
    /// [`KernelError::Config`] when `to` is not above the job's level, or a store error.
    pub async fn promote_to(
        &self,
        site: Uuid,
        job: &str,
        to: AutonomyLevel,
        decision: &OwnerDecision,
    ) -> Result<JobAutonomy> {
        decision.check_person()?;
        if let Some(charters) = &self.charters {
            let current =
                charters.current(site).await?.ok_or_else(|| KernelError::Unauthorized {
                    reason: format!(
                        "site {site} has no charter, so it has no owner to promote {job}"
                    ),
                })?;
            if !current.charter.governance.holds(&decision.owner.id, Role::Owner) {
                return Err(KernelError::Unauthorized {
                    reason: format!(
                        "{} is not an owner in charter version {} of site {site}, so cannot \
                         promote {job} (A-I13)",
                        decision.owner.id,
                        current.version()
                    ),
                });
            }
        }
        let reason = AutonomyReason::Promoted { decision: decision.clone() };
        let changed = self
            .change(
                site,
                job,
                |level| (ladder_rank(to) > ladder_rank(level)).then_some(to),
                reason,
                Some(decision.owner.clone()),
            )
            .await?;
        changed.ok_or_else(|| KernelError::Config {
            reason: format!("job {job} is not below {to:?}, so there is nothing to promote"),
        })
    }

    /// Lowers `job` one level for `reason` (spec 3.5 §7.11): the site may always lower its
    /// own autonomy. `None` when the job is at Observe already.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an unregistered job, or a store error.
    pub async fn lower(&self, site: Uuid, job: &str, reason: &str) -> Result<Option<JobAutonomy>> {
        self.demote(site, job, AutonomyReason::Lowered { reason: reason.to_string() }).await
    }

    /// Lowers `job` one level because `guardrail` was breached (spec 3.5 §7.11, A-I15).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an unregistered job, or a store error.
    pub async fn guardrail_breach(
        &self,
        site: Uuid,
        job: &str,
        guardrail: &str,
    ) -> Result<Option<JobAutonomy>> {
        self.demote(site, job, AutonomyReason::GuardrailBreach { guardrail: guardrail.into() })
            .await
    }

    /// Lowers every job of `area` one level for `incident` (spec 3.5 §7.11); returns the
    /// jobs it lowered.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn incident(
        &self,
        site: Uuid,
        area: &str,
        incident: &str,
    ) -> Result<Vec<JobAutonomy>> {
        let mut lowered = Vec::new();
        for job in self.store.list(site).await? {
            if job.area.as_deref() != Some(area) {
                continue;
            }
            let reason =
                AutonomyReason::Incident { area: area.to_string(), incident: incident.to_string() };
            if let Some(record) = self.demote(site, &job.job, reason).await? {
                lowered.push(record);
            }
        }
        Ok(lowered)
    }

    /// Records the owner's `decision` on a proposal of its job (spec 3.5 §7.11). The second
    /// rejection in a row since the job's last change lowers it one level, which is
    /// returned. A repeated decision id changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`] for a decision no person made — an agent's
    /// approvals never build a promotion — [`KernelError::Config`] for an unregistered job,
    /// or a store error.
    pub async fn record_decision(&self, decision: JobDecision) -> Result<Option<JobAutonomy>> {
        if decision.by.kind != SubjectKind::Human || is_agent_principal(&decision.by.id) {
            return Err(KernelError::Unauthorized {
                reason: format!(
                    "{} is not a person; only the owner's decisions count towards a job's \
                     autonomy (A-I13)",
                    decision.by.id
                ),
            });
        }
        let (site, job) = (decision.site, decision.job.clone());
        let record = self.store.get(site, &job).await?.ok_or_else(|| missing(&job))?;
        if !self.store.record_decision(decision.clone()).await?
            || decision.outcome != DecisionOutcome::Rejected
        {
            return Ok(None);
        }
        let recent = self.store.decisions(site, &job, record.since).await?;
        let last_two: Vec<&JobDecision> = recent.iter().rev().take(2).collect();
        if last_two.len() == 2 && last_two.iter().all(|d| d.outcome == DecisionOutcome::Rejected) {
            let decisions = last_two.iter().rev().map(|d| d.id.clone()).collect();
            return self.demote(site, &job, AutonomyReason::OwnerRejections { decisions }).await;
        }
        Ok(None)
    }

    /// Whether `job` has earned a promotion (spec 3.5 §7.11): the unchanged approvals in a
    /// row, latest first, made within the policy's window since the job's last change.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an unregistered job, or a store error.
    pub async fn eligibility(&self, site: Uuid, job: &str) -> Result<PromotionEligibility> {
        let record = self.store.get(site, job).await?.ok_or_else(|| missing(job))?;
        let window_start = self.clock.now() - self.policy.within;
        let since = record.since.max(window_start);
        let decisions = self.store.decisions(site, job, since).await?;
        let streak = decisions
            .iter()
            .rev()
            .take_while(|decision| decision.outcome == DecisionOutcome::ApprovedUnchanged)
            .count();
        let next = above(record.level);
        Ok(PromotionEligibility {
            site,
            job: job.to_string(),
            level: record.level,
            next,
            streak,
            needed: self.policy.approvals,
            eligible: next.is_some() && streak >= self.policy.approvals,
        })
    }

    /// The promotions the site's jobs earned, for the weekly review; each is emitted as
    /// [`CharterEvent::PromotionOffered`].
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn offers(&self, site: Uuid) -> Result<Vec<PromotionEligibility>> {
        let mut offers = Vec::new();
        for job in self.store.list(site).await? {
            let eligibility = self.eligibility(site, &job.job).await?;
            if let (true, Some(to)) = (eligibility.eligible, eligibility.next) {
                emit(
                    self.events.as_ref(),
                    CharterEvent::PromotionOffered {
                        site,
                        job: job.job.clone(),
                        to,
                        streak: eligibility.streak,
                        at: self.clock.now(),
                    },
                )
                .await;
                offers.push(eligibility);
            }
        }
        Ok(offers)
    }

    /// Lowers `job` one rung for `reason`; `None` at Observe.
    async fn demote(
        &self,
        site: Uuid,
        job: &str,
        reason: AutonomyReason,
    ) -> Result<Option<JobAutonomy>> {
        if self.store.get(site, job).await?.is_none() {
            return Err(missing(job));
        }
        self.change(site, job, below, reason, None).await
    }

    /// Moves `job` to `target(level)` for `reason`, retrying against concurrent changes;
    /// `None` when `target` leaves it where it is. `by` is the owner for a promotion, the
    /// site otherwise.
    async fn change(
        &self,
        site: Uuid,
        job: &str,
        target: impl Fn(AutonomyLevel) -> Option<AutonomyLevel>,
        reason: AutonomyReason,
        by: Option<SubjectRef>,
    ) -> Result<Option<JobAutonomy>> {
        for _ in 0..RETRIES {
            let current = self.store.get(site, job).await?.ok_or_else(|| missing(job))?;
            let Some(to) = target(current.level).filter(|to| *to != current.level) else {
                return Ok(None);
            };
            let now = self.clock.now();
            let next = JobAutonomy {
                level: to,
                since: now,
                revision: current.revision + 1,
                ..current.clone()
            };
            let by = by.clone().unwrap_or_else(|| SubjectRef::service(SITE_SUBJECT));
            let change = AutonomyChange {
                site,
                job: job.to_string(),
                revision: next.revision,
                from: Some(current.level),
                to,
                reason: reason.clone(),
                by: by.clone(),
                at: now,
            };
            if self.store.apply(next.clone(), change).await? {
                let raised = ladder_rank(to) > ladder_rank(current.level);
                tracing::info!(site = %site, job, from = ?current.level, to = ?to, reason = %reason.describe(), "job autonomy changed");
                let decision = match &reason {
                    AutonomyReason::Promoted { decision } => Some(decision.decision_id.clone()),
                    _ => None,
                };
                record(
                    self.audit.as_ref(),
                    WEBSITE_AUDIT_KIND,
                    if raised { audit::JOB_PROMOTED } else { audit::JOB_DEMOTED },
                    AuditActor::subject(by),
                    json!({
                        "site": site,
                        "job": job,
                        "from": current.level,
                        "to": to,
                        "reason": reason.describe(),
                        "decision": decision,
                    }),
                )
                .await;
                emit(
                    self.events.as_ref(),
                    CharterEvent::AutonomyChanged {
                        site,
                        job: job.to_string(),
                        from: current.level,
                        to,
                        reason: reason.describe(),
                        at: now,
                    },
                )
                .await;
                return Ok(Some(next));
            }
        }
        Err(KernelError::Store {
            backend: "autonomy",
            reason: format!("job {job} kept changing under {RETRIES} attempts; try again"),
        })
    }
}

fn missing(job: &str) -> KernelError {
    KernelError::Config {
        reason: format!(
            "job {job} is not on the autonomy ladder; register it from the charter's authority \
             first"
        ),
    }
}
