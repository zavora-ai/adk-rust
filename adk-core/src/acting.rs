//! How a run's agents act: the identity ADK-Rust derives when none is configured, the
//! run-level [`ActingConfig`], delegation, and the [`SubjectDelegationPolicy`] that lets a
//! run act for a subject other than its session's user.
//!
//! # Identity model
//!
//! | Type | Answers | Source |
//! |------|---------|--------|
//! | [`AdkIdentity`](crate::AdkIdentity) | Whose data: app, user, session | The session the run continues |
//! | [`ExecutionIdentity`](crate::ExecutionIdentity) | Which execution: invocation, branch, agent name | The runner, per invocation |
//! | [`ActingContext`] | Who acts, for whom, through whom | The run's [`ActingConfig`], resolved per agent |
//!
//! The invariants the runtime keeps:
//!
//! 1. **The session user is the subject.** An agent acts on behalf of the run's
//!    `user_id`, as the run's [`SubjectKind`]. A run configured to act for anyone else is
//!    refused at its start with [`SUBJECT_MISMATCH_CODE`] unless a
//!    [`SubjectDelegationPolicy`] allows it; the session's subject then heads the
//!    delegation chain.
//! 2. **Each agent acts as itself.** The principal is resolved for the agent that is
//!    executing — root, transfer target, or workflow sub-agent — from
//!    [`ActingConfig::principals`], falling back to [`AgentPrincipal::derived`].
//! 3. **One invocation id.** [`ActingContext`] holds none; the invocation is the
//!    execution identity's.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;

use crate::principal::{
    ActingContext, AgentId, AgentPrincipal, MAX_AGENT_ID_LEN, PrincipalError, SubjectKind,
    SubjectRef,
};
use crate::{AdkError, ErrorCategory, ErrorComponent, Result};

/// The error code of a run refused because it asks to act for a subject other than its
/// session's user, with no [`SubjectDelegationPolicy`] allowing it.
pub const SUBJECT_MISMATCH_CODE: &str = "identity.subject_mismatch";

/// The error code of a durable run refused because its stored principal names an agent
/// the worker may not act as.
pub const PRINCIPAL_NOT_ALLOWED_CODE: &str = "identity.principal_not_allowed";

impl From<PrincipalError> for AdkError {
    fn from(error: PrincipalError) -> Self {
        AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::InvalidInput,
            "identity.agent_id.invalid",
            error.to_string(),
        )
    }
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Human => "human",
            Self::Service => "service",
            Self::Agent => "agent",
        })
    }
}

impl fmt::Display for SubjectRef {
    /// Formats as `kind:id`, for example `human:alice`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

impl AgentPrincipal {
    /// The principal ADK-Rust derives when none is configured:
    /// `urn:adk:agent:{app_name}/{agent_name}`.
    ///
    /// Characters outside `[A-Za-z0-9-._~]` are percent-encoded, so every app and agent
    /// name yields a valid [`AgentId`], and the same names always yield the same id.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::AgentPrincipal;
    ///
    /// let principal = AgentPrincipal::derived("billing app", "invoicer");
    /// assert_eq!(principal.id.as_str(), "urn:adk:agent:billing%20app/invoicer");
    /// ```
    pub fn derived(app_name: &str, agent_name: &str) -> Self {
        let encode = |value: &str| -> String {
            if value.is_empty() {
                return "_".to_string();
            }
            let mut out = String::with_capacity(value.len());
            for byte in value.bytes() {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    out.push(byte as char);
                } else {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            out
        };
        let mut id = format!("urn:adk:agent:{}/{}", encode(app_name), encode(agent_name));
        // Percent-encoding at most triples a name, so only absurd names reach the cap;
        // the encoded id is ASCII, so truncation stays on a character boundary.
        id.truncate(MAX_AGENT_ID_LEN);
        Self::new(AgentId::new(id).expect("a percent-encoded URN is a valid agent id"))
    }
}

impl ActingContext {
    /// The acting context ADK-Rust derives when none is configured.
    ///
    /// The principal is [`AgentPrincipal::derived`] from `app_name` and `agent_name`,
    /// and the agent acts for `user_id` as a subject of `kind` when `user_id` is
    /// non-empty.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ActingContext, SubjectKind, SubjectRef};
    ///
    /// let acting = ActingContext::derived("shop", "cashier", "alice", SubjectKind::Human);
    /// assert_eq!(acting.agent.id.as_str(), "urn:adk:agent:shop/cashier");
    /// assert_eq!(acting.on_behalf_of, Some(SubjectRef::human("alice")));
    /// ```
    pub fn derived(app_name: &str, agent_name: &str, user_id: &str, kind: SubjectKind) -> Self {
        let acting = Self::new(AgentPrincipal::derived(app_name, agent_name));
        if user_id.is_empty() {
            acting
        } else {
            acting.on_behalf_of(SubjectRef::new(kind, user_id))
        }
    }

    /// The context `delegate` acts in when this agent hands it a task.
    ///
    /// The delegate acts for the same subject, and the chain gains this agent.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ActingContext, AgentPrincipal, SubjectKind, SubjectRef};
    ///
    /// let planner = ActingContext::derived("ops", "planner", "alice", SubjectKind::Human);
    /// let researcher = planner.delegate(AgentPrincipal::derived("ops", "researcher"));
    /// assert_eq!(researcher.delegation_chain, vec![SubjectRef::agent(&planner.agent.id)]);
    /// assert_eq!(researcher.on_behalf_of, planner.on_behalf_of);
    /// ```
    #[must_use]
    pub fn delegate(&self, delegate: AgentPrincipal) -> Self {
        let mut delegated = Self::new(delegate);
        delegated.on_behalf_of = self.on_behalf_of.clone();
        delegated.delegation_chain = self.delegation_chain.clone();
        delegated.delegated_by(SubjectRef::agent(&self.agent.id))
    }
}

/// Who a run's agents act as, and for whom.
///
/// Carried on [`RunConfig::acting`](crate::RunConfig::acting). The runner fills
/// [`principals`](Self::principals) from its [`IdentitySource`](crate::IdentitySource)
/// and checks [`on_behalf_of`](Self::on_behalf_of) against the session's user at the
/// start of each run ([`admit_subject`](Self::admit_subject)).
///
/// # Example
///
/// ```rust
/// use adk_core::{ActingConfig, AgentId, AgentPrincipal, SubjectKind, SubjectRef};
///
/// let acting = ActingConfig::default()
///     .with_principal("billing", AgentPrincipal::new(AgentId::new("spiffe://acme/billing")?))
///     .with_subject_kind(SubjectKind::Service);
///
/// let billing = acting.acting_context_for("shop", "billing", "nightly-close");
/// assert_eq!(billing.agent.id.as_str(), "spiffe://acme/billing");
/// assert_eq!(billing.on_behalf_of, Some(SubjectRef::service("nightly-close")));
///
/// // An agent with no configured principal acts as its derived id.
/// let auditor = acting.acting_context_for("shop", "auditor", "nightly-close");
/// assert_eq!(auditor.agent.id.as_str(), "urn:adk:agent:shop/auditor");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActingConfig {
    /// The principal each named agent acts as. An agent with no entry acts as
    /// [`AgentPrincipal::derived`] from the app and its name.
    pub principals: BTreeMap<String, AgentPrincipal>,
    /// The subject the run's agents act for. `None` acts for the session's user as
    /// [`subject_kind`](Self::subject_kind); any other subject needs a
    /// [`SubjectDelegationPolicy`] on the runner.
    pub on_behalf_of: Option<SubjectRef>,
    /// What kind of subject the session's user is. [`SubjectKind::Human`] by default;
    /// runs created by a scheduler, a webhook, or a queue are
    /// [`SubjectKind::Service`].
    pub subject_kind: SubjectKind,
    /// The subjects that delegated the run's work, outermost first.
    pub delegation_chain: Vec<SubjectRef>,
}

impl ActingConfig {
    /// Sets the principal the agent named `agent_name` acts as.
    #[must_use]
    pub fn with_principal(
        mut self,
        agent_name: impl Into<String>,
        principal: AgentPrincipal,
    ) -> Self {
        self.principals.insert(agent_name.into(), principal);
        self
    }

    /// Sets the subject the run's agents act for.
    #[must_use]
    pub fn with_on_behalf_of(mut self, subject: SubjectRef) -> Self {
        self.on_behalf_of = Some(subject);
        self
    }

    /// Sets what kind of subject the session's user is.
    #[must_use]
    pub fn with_subject_kind(mut self, kind: SubjectKind) -> Self {
        self.subject_kind = kind;
        self
    }

    /// The principal the agent named `agent_name` of `app_name` acts as.
    pub fn principal_for(&self, app_name: &str, agent_name: &str) -> AgentPrincipal {
        self.principals
            .get(agent_name)
            .cloned()
            .unwrap_or_else(|| AgentPrincipal::derived(app_name, agent_name))
    }

    /// The subject a session of `user_id` names: `user_id` as
    /// [`subject_kind`](Self::subject_kind), or `None` for an empty user.
    pub fn session_subject(&self, user_id: &str) -> Option<SubjectRef> {
        (!user_id.is_empty()).then(|| SubjectRef::new(self.subject_kind, user_id))
    }

    /// The acting context of the agent named `agent_name` running for `user_id`.
    pub fn acting_context_for(
        &self,
        app_name: &str,
        agent_name: &str,
        user_id: &str,
    ) -> ActingContext {
        ActingContext {
            agent: self.principal_for(app_name, agent_name),
            on_behalf_of: self.on_behalf_of.clone().or_else(|| self.session_subject(user_id)),
            delegation_chain: self.delegation_chain.clone(),
        }
    }

    /// Checks the subject the run acts for against the session's user and pins it.
    ///
    /// | `on_behalf_of` | Result |
    /// |----------------|--------|
    /// | `None` | Set to the session's subject |
    /// | The session's subject | Unchanged |
    /// | Another subject, allowed by `policy` | Unchanged; the session's subject heads the delegation chain |
    /// | Another subject, otherwise | Refused |
    ///
    /// `agent` is the principal of the run's root agent, for the policy to decide on.
    ///
    /// # Errors
    ///
    /// Returns [`SUBJECT_MISMATCH_CODE`] (`Forbidden`) when the run asks to act for a
    /// subject other than the session's user and no policy allows it, or the policy's
    /// own error.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ActingConfig, AgentPrincipal, SUBJECT_MISMATCH_CODE, SubjectRef};
    ///
    /// # futures::executor::block_on(async {
    /// let agent = AgentPrincipal::derived("bank", "teller");
    /// let mut acting = ActingConfig::default().with_on_behalf_of(SubjectRef::human("bob"));
    /// let error = acting.admit_subject("bank", "alice", &agent, None).await.unwrap_err();
    /// assert_eq!(error.code, SUBJECT_MISMATCH_CODE);
    /// # });
    /// ```
    pub async fn admit_subject(
        &mut self,
        app_name: &str,
        user_id: &str,
        agent: &AgentPrincipal,
        policy: Option<&dyn SubjectDelegationPolicy>,
    ) -> Result<()> {
        let Some(session) = self.session_subject(user_id) else {
            return Ok(());
        };
        let Some(requested) = &self.on_behalf_of else {
            self.on_behalf_of = Some(session);
            return Ok(());
        };
        if *requested == session {
            return Ok(());
        }
        let allowed = match policy {
            Some(policy) => {
                policy
                    .may_act_for(&SubjectDelegationRequest {
                        app_name: app_name.to_string(),
                        agent: agent.clone(),
                        session_subject: session.clone(),
                        requested: requested.clone(),
                    })
                    .await?
            }
            None => false,
        };
        if !allowed {
            return Err(subject_mismatch_error(&session, requested));
        }
        if self.delegation_chain.first() != Some(&session) {
            self.delegation_chain.insert(0, session);
        }
        Ok(())
    }
}

/// The [`SUBJECT_MISMATCH_CODE`] error for a run of `session` asking to act for `requested`.
pub fn subject_mismatch_error(session: &SubjectRef, requested: &SubjectRef) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Forbidden,
        SUBJECT_MISMATCH_CODE,
        format!(
            "the run acts for {requested} but its session belongs to {session}; a run acts for \
             its session's user unless a SubjectDelegationPolicy on the runner allows the \
             delegation"
        ),
    )
}

/// A request to act for a subject other than the session's user.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SubjectDelegationRequest {
    /// The application the run belongs to.
    pub app_name: String,
    /// The principal of the run's root agent.
    pub agent: AgentPrincipal,
    /// The session's user, as the run's subject kind.
    pub session_subject: SubjectRef,
    /// The subject the run asks to act for.
    pub requested: SubjectRef,
}

/// Decides whether a run may act for a subject other than its session's user.
///
/// Installed with `Runner::builder().subject_delegation(...)`. Without one, every such run
/// is refused with [`SUBJECT_MISMATCH_CODE`]. Implement it over a token-exchange or
/// directory lookup to decide per request; [`StaticSubjectDelegation`] covers fixed
/// rules.
#[async_trait]
pub trait SubjectDelegationPolicy: fmt::Debug + Send + Sync {
    /// Whether the run described by `request` may act for `request.requested`.
    ///
    /// # Errors
    ///
    /// An error refuses the run with that error.
    async fn may_act_for(&self, request: &SubjectDelegationRequest) -> Result<bool>;
}

/// A [`SubjectDelegationPolicy`] with fixed rules: sessions of a subject may act for any
/// subject, or for listed subjects.
///
/// # Example
///
/// ```rust
/// use adk_core::{
///     AgentPrincipal, StaticSubjectDelegation, SubjectDelegationPolicy,
///     SubjectDelegationRequest, SubjectRef,
/// };
///
/// let policy = StaticSubjectDelegation::new()
///     .allow_any(SubjectRef::service("crm-sync"))
///     .allow(SubjectRef::human("assistant-of-bob"), SubjectRef::human("bob"));
/// # let _ = policy;
/// ```
#[derive(Debug, Clone, Default)]
pub struct StaticSubjectDelegation {
    rules: Vec<(SubjectRef, Option<SubjectRef>)>,
}

impl StaticSubjectDelegation {
    /// A policy that allows nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sessions of `session_subject` may act for any subject.
    #[must_use]
    pub fn allow_any(mut self, session_subject: SubjectRef) -> Self {
        self.rules.push((session_subject, None));
        self
    }

    /// Sessions of `session_subject` may act for `requested`.
    #[must_use]
    pub fn allow(mut self, session_subject: SubjectRef, requested: SubjectRef) -> Self {
        self.rules.push((session_subject, Some(requested)));
        self
    }
}

#[async_trait]
impl SubjectDelegationPolicy for StaticSubjectDelegation {
    async fn may_act_for(&self, request: &SubjectDelegationRequest) -> Result<bool> {
        Ok(self.rules.iter().any(|(session, requested)| {
            *session == request.session_subject
                && requested.as_ref().is_none_or(|requested| *requested == request.requested)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_ids_are_stable_and_always_valid() {
        assert_eq!(AgentPrincipal::derived("a", "b"), AgentPrincipal::derived("a", "b"));
        assert_eq!(AgentPrincipal::derived("", "").id.as_str(), "urn:adk:agent:_/_");
        assert_eq!(AgentPrincipal::derived("a/b", "c:d").id.as_str(), "urn:adk:agent:a%2Fb/c%3Ad");
        let long = AgentPrincipal::derived(&"\n".repeat(400), "agent");
        assert!(AgentId::new(long.id.as_str()).is_ok());
    }

    #[test]
    fn delegation_extends_the_chain_and_keeps_the_subject() {
        let acting = ActingContext::derived("app", "agent", "alice", SubjectKind::Human)
            .delegate(AgentPrincipal::derived("app", "helper").with_tenant("acme"));
        let json = serde_json::to_value(&acting).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "agent": { "id": "urn:adk:agent:app/helper", "tenant": "acme" },
                "onBehalfOf": { "kind": "human", "id": "alice" },
                "delegationChain": [{ "kind": "agent", "id": "urn:adk:agent:app/agent" }],
            })
        );
    }

    #[test]
    fn principal_errors_convert_with_a_structured_code() {
        let error: AdkError = AgentId::new("").unwrap_err().into();
        assert_eq!(error.code, "identity.agent_id.invalid");
    }

    #[test]
    fn the_subject_kind_comes_from_the_config() {
        let service = ActingConfig::default().with_subject_kind(SubjectKind::Service);
        assert_eq!(
            service.acting_context_for("ops", "reporter", "nightly"),
            ActingContext::derived("ops", "reporter", "nightly", SubjectKind::Service)
        );
        assert_eq!(
            ActingConfig::default().acting_context_for("ops", "reporter", "alice").on_behalf_of,
            Some(SubjectRef::human("alice"))
        );
        assert_eq!(ActingConfig::default().acting_context_for("ops", "r", "").on_behalf_of, None);
    }

    #[test]
    fn principals_resolve_per_agent() {
        let billing = AgentPrincipal::new(AgentId::new("spiffe://acme/billing").unwrap());
        let config = ActingConfig::default().with_principal("billing", billing.clone());
        assert_eq!(config.principal_for("shop", "billing"), billing);
        assert_eq!(config.principal_for("shop", "audit"), AgentPrincipal::derived("shop", "audit"));
    }

    #[tokio::test]
    async fn a_mismatched_subject_is_refused_without_a_policy() {
        let agent = AgentPrincipal::derived("bank", "teller");
        let mut acting = ActingConfig::default().with_on_behalf_of(SubjectRef::human("bob"));
        let error = acting.admit_subject("bank", "alice", &agent, None).await.unwrap_err();
        assert_eq!(error.code, SUBJECT_MISMATCH_CODE);
        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert!(error.message.contains("human:bob"), "{}", error.message);
        assert!(error.message.contains("human:alice"), "{}", error.message);

        // The same id as another kind is a different subject.
        let mut acting = ActingConfig::default().with_on_behalf_of(SubjectRef::service("alice"));
        let error = acting.admit_subject("bank", "alice", &agent, None).await.unwrap_err();
        assert_eq!(error.code, SUBJECT_MISMATCH_CODE);
    }

    #[tokio::test]
    async fn the_session_subject_is_pinned_or_matched() {
        let agent = AgentPrincipal::derived("bank", "teller");
        let mut acting = ActingConfig::default();
        acting.admit_subject("bank", "alice", &agent, None).await.unwrap();
        assert_eq!(acting.on_behalf_of, Some(SubjectRef::human("alice")));

        let mut acting = ActingConfig::default()
            .with_subject_kind(SubjectKind::Service)
            .with_on_behalf_of(SubjectRef::service("cron"));
        acting.admit_subject("bank", "cron", &agent, None).await.unwrap();
        assert_eq!(acting.on_behalf_of, Some(SubjectRef::service("cron")));
        assert!(acting.delegation_chain.is_empty());
    }

    #[tokio::test]
    async fn a_delegation_rule_allows_another_subject() {
        let agent = AgentPrincipal::derived("crm", "sync");
        let policy = StaticSubjectDelegation::new().allow_any(SubjectRef::service("crm-sync"));
        let mut acting = ActingConfig::default()
            .with_subject_kind(SubjectKind::Service)
            .with_on_behalf_of(SubjectRef::human("bob"));
        acting.admit_subject("crm", "crm-sync", &agent, Some(&policy)).await.unwrap();
        assert_eq!(acting.on_behalf_of, Some(SubjectRef::human("bob")));
        assert_eq!(acting.delegation_chain, vec![SubjectRef::service("crm-sync")]);

        // The rule names the session subject, so another service is still refused.
        let mut other = ActingConfig::default()
            .with_subject_kind(SubjectKind::Service)
            .with_on_behalf_of(SubjectRef::human("bob"));
        let error = other.admit_subject("crm", "billing", &agent, Some(&policy)).await.unwrap_err();
        assert_eq!(error.code, SUBJECT_MISMATCH_CODE);

        let pair = StaticSubjectDelegation::new()
            .allow(SubjectRef::human("assistant"), SubjectRef::human("bob"));
        let mut for_bob = ActingConfig::default().with_on_behalf_of(SubjectRef::human("bob"));
        for_bob.admit_subject("crm", "assistant", &agent, Some(&pair)).await.unwrap();
        let mut for_carol = ActingConfig::default().with_on_behalf_of(SubjectRef::human("carol"));
        assert!(for_carol.admit_subject("crm", "assistant", &agent, Some(&pair)).await.is_err());
    }
}
