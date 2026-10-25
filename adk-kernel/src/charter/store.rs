//! The charter store and approval chains (spec 3.5 §7.6, §7.11, A-I23).

use std::collections::BTreeMap;
use std::fmt;

use adk_core::principal::{SubjectKind, SubjectRef};
use async_trait::async_trait;
use awp_types::{
    ActionClass, ApprovalChain, Charter, Governance, OwnerSignature, Role, RoleHolder,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{KernelError, Result};

/// The action class of a charter change in a charter's approval chains (spec 3.5 §7.6): a
/// commit needs every role of the chain with this action, or one owner when the charter
/// names no such chain.
pub const CHARTER_ACTION: ActionClass = ActionClass::CharterChange;

/// One approval of a decision by a person holding a role (spec 3.5 §7.6, A-I23).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::charter::RoleApproval;
/// use awp_types::Role;
///
/// let approval = RoleApproval::new(Role::Owner, SubjectRef::human("amina"), chrono::Utc::now());
/// assert_eq!(serde_json::to_value(&approval).unwrap()["principal"]["id"], "amina");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RoleApproval {
    /// The role the approver acts in.
    pub role: Role,
    /// Who approved.
    pub principal: SubjectRef,
    /// When.
    pub at: DateTime<Utc>,
}

impl RoleApproval {
    /// An approval by `principal` acting as `role` at `at`.
    pub fn new(role: Role, principal: SubjectRef, at: DateTime<Utc>) -> Self {
        Self { role, principal, at }
    }
}

/// The decision that commits a charter version (spec 3.5 §7.6, §7.11): its id, the approvals
/// of its chain, and the signed note that seals it in the audit log.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::charter::{CommitDecision, RoleApproval};
/// use awp_types::Role;
///
/// let now = chrono::Utc::now();
/// let decision = CommitDecision::new("decision-1", now)
///     .with_approval(RoleApproval::new(Role::Owner, SubjectRef::human("amina"), now))
///     .with_signed_note("note:sha256:ab12");
/// assert_eq!(decision.approvals.len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CommitDecision {
    /// The decision's id.
    pub decision_id: String,
    /// The approvals of the charter's approval chain, one per role it requires.
    pub approvals: Vec<RoleApproval>,
    /// The signed note sealing the commit in the audit log (`adk-auth`'s signed audit), by
    /// reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_note: Option<String>,
    /// When the decision was made.
    pub decided_at: DateTime<Utc>,
}

impl CommitDecision {
    /// A decision with no approvals and no signed note yet.
    pub fn new(decision_id: impl Into<String>, decided_at: DateTime<Utc>) -> Self {
        Self {
            decision_id: decision_id.into(),
            approvals: Vec::new(),
            signed_note: None,
            decided_at,
        }
    }

    /// Adds an approval.
    #[must_use]
    pub fn with_approval(mut self, approval: RoleApproval) -> Self {
        self.approvals.push(approval);
        self
    }

    /// Sets the signed note's reference.
    #[must_use]
    pub fn with_signed_note(mut self, note: impl Into<String>) -> Self {
        self.signed_note = Some(note.into());
        self
    }
}

/// One committed charter version with the decision that committed it (spec 3.5 §7.11).
///
/// # Example
///
/// ```rust,ignore
/// let version = store.current(site).await?.expect("a committed charter");
/// assert_eq!(version.version(), version.charter.version);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CharterVersion {
    /// The charter, with its version and owner signature as committed.
    pub charter: Charter,
    /// The committing decision.
    pub decision: CommitDecision,
    /// When it took effect.
    pub committed_at: DateTime<Utc>,
}

impl CharterVersion {
    /// A committed version of `charter`.
    pub fn new(charter: Charter, decision: CommitDecision, committed_at: DateTime<Utc>) -> Self {
        Self { charter, decision, committed_at }
    }

    /// The site.
    pub fn site(&self) -> Uuid {
        self.charter.site_id
    }

    /// The version number.
    pub fn version(&self) -> u32 {
        self.charter.version
    }
}

/// Why an approval chain is not satisfied (A-I23).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::ChainRefusal;
///
/// let refusal = ChainRefusal::new("charter", "owner needs an approval");
/// assert!(refusal.to_string().contains("charter"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChainRefusal {
    /// The action class.
    pub action: String,
    /// What is missing or wrong.
    pub reason: String,
}

impl ChainRefusal {
    /// A refusal of `action`'s chain for `reason`.
    pub fn new(action: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { action: action.into(), reason: reason.into() }
    }
}

impl fmt::Display for ChainRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the `{}` approval chain is not satisfied: {}", self.action, self.reason)
    }
}

/// Whether `id` names an agent rather than a person: an `urn:adk:agent:` URN or a SPIFFE
/// id, the forms of `adk_core::principal::AgentId` (spec 3.5 §7.6: no agent holds a role).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::is_agent_principal;
///
/// assert!(is_agent_principal("urn:adk:agent:example/steward"));
/// assert!(!is_agent_principal("amina@example.com"));
/// ```
pub fn is_agent_principal(id: &str) -> bool {
    let id = id.trim().to_ascii_lowercase();
    id.starts_with("urn:adk:agent:") || id.starts_with("spiffe://")
}

/// Checks that `approvals` satisfy the approval chain `governance` names for `action`
/// (spec 3.5 §7.6, A-I23): every role the chain lists has an approval by a person holding
/// that role, each role's approval by a different person, none by `proposer`. A role the
/// chain lists twice needs two people. A charter that names no chain for `action` needs one
/// owner.
///
/// An approval by an agent (a subject of kind `Agent` or `Service`, or an agent identity
/// such as `urn:adk:agent:…`) is refused outright: no agent holds a role.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::charter::{RoleApproval, check_chain};
/// use awp_types::{ActionClass, ApprovalChain, Governance, Role, RoleHolder};
///
/// let governance = Governance {
///     roles: vec![
///         RoleHolder { principal: "eva".into(), roles: vec![Role::Editor, Role::Publisher] },
///         RoleHolder { principal: "paul".into(), roles: vec![Role::Publisher] },
///         RoleHolder { principal: "amina".into(), roles: vec![Role::Owner] },
///     ],
///     approval_chains: vec![ApprovalChain { action: ActionClass::Publish, roles: vec![Role::Editor, Role::Publisher] }],
/// };
/// let now = chrono::Utc::now();
/// let by = |role, who: &str| RoleApproval::new(role, SubjectRef::human(who), now);
/// let publish = ActionClass::Publish;
/// // One person cannot fill both roles.
/// assert!(check_chain(&governance, &publish, &[by(Role::Editor, "eva"), by(Role::Publisher, "eva")], None).is_err());
/// assert!(check_chain(&governance, &publish, &[by(Role::Editor, "eva"), by(Role::Publisher, "paul")], None).is_ok());
/// // The proposer cannot approve.
/// assert!(check_chain(&governance, &publish, &[by(Role::Editor, "eva"), by(Role::Publisher, "paul")], Some("paul")).is_err());
/// ```
///
/// # Errors
///
/// Returns the [`ChainRefusal`] naming the first fault found.
pub fn check_chain(
    governance: &Governance,
    action: &ActionClass,
    approvals: &[RoleApproval],
    proposer: Option<&str>,
) -> std::result::Result<(), ChainRefusal> {
    let refuse = |reason: String| Err(ChainRefusal::new(action.as_str(), reason));
    let chain = governance
        .approval_chains
        .iter()
        .find(|chain| &chain.action == action)
        .cloned()
        .unwrap_or_else(|| ApprovalChain { action: action.clone(), roles: vec![Role::Owner] });
    if chain.roles.is_empty() {
        return refuse("the chain lists no role, so nobody can satisfy it".into());
    }
    // Who approved in which of the roles they hold.
    let mut acted: BTreeMap<&str, Vec<Role>> = BTreeMap::new();
    for approval in approvals {
        let who = &approval.principal;
        if who.kind != SubjectKind::Human || is_agent_principal(&who.id) {
            return refuse(format!(
                "{} is not a person, and no agent holds a role (spec 3.5 §7.6)",
                who.id
            ));
        }
        if !governance.holds(&who.id, approval.role) {
            return refuse(format!("{} does not hold the {:?} role", who.id, approval.role));
        }
        if proposer.is_some_and(|proposer| proposer == who.id) {
            return refuse(format!("{} proposed it, so cannot also approve it", who.id));
        }
        acted.entry(who.id.as_str()).or_default().push(approval.role);
    }
    // The chain's roles go to distinct people, each in a role they approved in: the AWP
    // chain's own matching over the roles acted in, so the order of approvals is irrelevant.
    let acting = Governance {
        roles: acted
            .iter()
            .map(|(who, roles)| RoleHolder { principal: (*who).to_string(), roles: roles.clone() })
            .collect(),
        approval_chains: Vec::new(),
    };
    let people: Vec<&str> = acted.keys().copied().collect();
    if !chain.is_satisfied_by(&acting, &people) {
        return refuse(format!(
            "it needs {} approvals, one per role of {:?}, each by a different person acting in \
             a role they hold",
            chain.roles.len(),
            chain.roles
        ));
    }
    Ok(())
}

/// Every committed version of every site's charter (spec 3.5 §7.11), append-only, on
/// in-memory, SQLite, PostgreSQL and Redis.
///
/// [`commit`](Self::commit) is the only way a version takes effect; it checks the draft and
/// the decision and calls [`append`](Self::append), which backends implement as a
/// compare-and-set on the site's latest version.
///
/// # Example
///
/// ```rust,ignore
/// let committed = store.commit(draft, decision).await?;
/// assert_eq!(store.current(committed.site()).await?.unwrap().version(), committed.version());
/// ```
#[async_trait]
pub trait CharterStore: Send + Sync {
    /// Appends `version` when its number is one past the site's latest committed version
    /// (1 for a site with none); `false`, writing nothing, otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn append(&self, version: CharterVersion) -> Result<bool>;

    /// The site's version in force: its latest, or `None` before the first commit.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn current(&self, site: Uuid) -> Result<Option<CharterVersion>>;

    /// Every committed version of the site, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn history(&self, site: Uuid) -> Result<Vec<CharterVersion>>;

    /// Version `version` of the site, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn get(&self, site: Uuid, version: u32) -> Result<Option<CharterVersion>>;

    /// Commits `draft` by `decision` as the site's next version (spec 3.5 §7.6, A-I23).
    ///
    /// The draft must pass `Charter::validate()` (A-I14: no target without a baseline), and
    /// no role in its governance may be held by an agent. The decision must satisfy the
    /// approval chain for [`CHARTER_ACTION`] of the charter in force — or, for a site's first
    /// charter, of the draft — by distinct people ([`check_chain`]), so a draft cannot
    /// loosen the chain that approves it. The committed version is the next number, signed
    /// with the decision: its id, the first owner's approval, the signed note and the time.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for a draft that fails validation or names another
    /// site, [`KernelError::Unauthorized`] for a decision that does not satisfy the chain,
    /// an [`AdkError`](adk_core::AdkError) conflict when another commit took the version
    /// first, or a store error.
    async fn commit(&self, draft: Charter, decision: CommitDecision) -> Result<CharterVersion> {
        let site = draft.site_id;
        draft.validate().map_err(|error| KernelError::Config {
            reason: format!("the charter draft for site {site} is invalid: {error}"),
        })?;
        if let Some(agent) = draft
            .governance
            .roles
            .iter()
            .find(|held| !held.roles.is_empty() && is_agent_principal(&held.principal))
        {
            return Err(KernelError::Config {
                reason: format!(
                    "the charter draft gives {} the {:?} roles, but no agent holds a role (spec \
                     3.5 §7.6); name a person",
                    agent.principal, agent.roles
                ),
            });
        }
        let current = self.current(site).await?;
        let governing = current.as_ref().map_or(&draft.governance, |c| &c.charter.governance);
        check_chain(governing, &CHARTER_ACTION, &decision.approvals, None)
            .map_err(|refusal| KernelError::Unauthorized { reason: refusal.to_string() })?;
        let owner = decision
            .approvals
            .iter()
            .find(|approval| approval.role == Role::Owner)
            .or_else(|| decision.approvals.first())
            .map(|approval| approval.principal.id.clone())
            .unwrap_or_default();
        let mut charter = draft;
        charter.version = current.as_ref().map_or(1, |c| c.version() + 1);
        charter.signed = Some(OwnerSignature {
            decision: decision.decision_id.clone(),
            principal: owner,
            note: decision.signed_note.clone().unwrap_or_default(),
            signed_at: decision.decided_at,
        });
        let version = CharterVersion::new(charter, decision, Utc::now());
        if !self.append(version.clone()).await? {
            return Err(KernelError::Core(adk_core::AdkError::new(
                adk_core::ErrorComponent::Server,
                adk_core::ErrorCategory::Conflict,
                CHARTER_CONFLICT_CODE,
                format!(
                    "another commit took version {} of site {site}'s charter first; read the \
                     current charter and commit again",
                    version.version()
                ),
            )));
        }
        Ok(version)
    }
}

/// The error code of a commit another commit beat to its version.
pub(crate) const CHARTER_CONFLICT_CODE: &str = "website.charter.conflict";

/// Commits `draft` by `decision` on `store` as [`CharterStore::commit`] does, and records the
/// outcome on `trail` (spec 3.5 §9.3): `website.approval_chain.satisfied` and
/// `website.charter.committed` for a commit, `website.approval_chain.refused` for a decision
/// whose chain is not satisfied. The records name the approvers and the decision, never the
/// charter's text.
///
/// # Example
///
/// ```rust,ignore
/// let version = commit_audited(store.as_ref(), draft, decision, Some(&trail)).await?;
/// ```
///
/// # Errors
///
/// As [`CharterStore::commit`].
pub async fn commit_audited(
    store: &dyn CharterStore,
    draft: Charter,
    decision: CommitDecision,
    trail: Option<&adk_core::audit_log::AuditTrail>,
) -> Result<CharterVersion> {
    use adk_core::audit_log::AuditActor;

    let site = draft.site_id;
    let decision_id = decision.decision_id.clone();
    let approvers: Vec<String> =
        decision.approvals.iter().map(|approval| approval.principal.id.clone()).collect();
    let actor = || match decision.approvals.first() {
        Some(approval) => AuditActor::subject(approval.principal.clone()),
        None => AuditActor::default(),
    };
    let actor = actor();
    let kind = super::WEBSITE_AUDIT_KIND;
    match store.commit(draft, decision).await {
        Ok(version) => {
            let fields = serde_json::json!({
                "site": site,
                "action": CHARTER_ACTION.as_str(),
                "decision": decision_id,
                "approvers": approvers,
                "charterVersion": version.version(),
                "signedNote": version.decision.signed_note,
            });
            super::record(
                trail,
                kind,
                super::audit::APPROVAL_CHAIN_SATISFIED,
                actor.clone(),
                fields.clone(),
            )
            .await;
            super::record(trail, kind, super::audit::CHARTER_COMMITTED, actor, fields).await;
            Ok(version)
        }
        Err(KernelError::Unauthorized { reason }) => {
            let fields = serde_json::json!({
                "site": site,
                "action": CHARTER_ACTION.as_str(),
                "decision": decision_id,
                "approvers": approvers,
                "reason": reason,
            });
            super::record(trail, kind, super::audit::APPROVAL_CHAIN_REFUSED, actor, fields).await;
            Err(KernelError::Unauthorized { reason })
        }
        Err(error) => Err(error),
    }
}
