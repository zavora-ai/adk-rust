//! Approval service: an inbox of approval requests over an [`ApprovalStore`].
//!
//! A call held for approval is recorded in the run's [`ApprovalStore`] and, when the run
//! has an [`ApprovalService`], submitted to it as an [`ApprovalRequest`] carrying an
//! [`ApprovalPolicy`]: who may approve, how many distinct approvers it needs, whether
//! the requester may approve their own call, when it escalates, and when it expires.
//! Approvers list their [`inbox`](ApprovalService::inbox) and
//! [`decide`](ApprovalService::decide); the decision that resolves the request is written
//! to the store under the call's fingerprint, wakes the waiting durable run if one is
//! attached, and is otherwise picked up by the next run, which resumes the held call.
//!
//! | Action | Effect |
//! |--------|--------|
//! | [`Approve`](ApprovalAction::Approve) | Counts toward the quorum; the call executes once the quorum is met |
//! | [`Edit`](ApprovalAction::Edit) | Approves with substituted arguments; earlier approvals of other arguments stop counting. The edited call passes the [`ToolPolicy`](crate::ToolPolicy) again before it executes |
//! | [`Reject`](ApprovalAction::Reject) | Refuses the call at once, with the reason reported to the model |
//! | [`Respond`](ApprovalAction::Respond) | Does not run the call; the response is reported to the model in its place |
//!
//! Every change to a request is a compare-and-swap on its
//! [`version`](ApprovalRequest::version), so of two approvers deciding the same version
//! one wins and the other receives `approval.version_conflict`. [`tick`](ApprovalService::tick),
//! driven by the service's [`Clock`], expires overdue requests (denying the call) and
//! fires escalation steps, each exactly once.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::principal::SubjectRef;
//! use adk_core::{
//!     ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
//!     ApprovalState, ApproverSelector, InMemoryApprovalStore, StoreApprovalService,
//!     ToolConfirmationRequest,
//! };
//! use serde_json::json;
//!
//! # futures::executor::block_on(async {
//! let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()));
//! let call = ToolConfirmationRequest {
//!     tool_name: "transfer".into(),
//!     function_call_id: Some("call-1".into()),
//!     args: json!({ "amount": 900 }),
//! };
//! let policy = ApprovalPolicy::new([ApproverSelector::group("finance")]).with_quorum(2);
//! let request = service
//!     .submit(ApprovalRequest::new("req-1", ApprovalScope::new("app", "u", "s"), &call, policy))
//!     .await?;
//!
//! let first = service
//!     .decide(&request.id, request.version,
//!         ApprovalDecision::approve(SubjectRef::human("ana")).with_groups(["finance"]))
//!     .await?;
//! assert_eq!(first.state, ApprovalState::Pending);
//! let second = service
//!     .decide(&first.id, first.version,
//!         ApprovalDecision::approve(SubjectRef::human("bo")).with_groups(["finance"]))
//!     .await?;
//! assert_eq!(second.state, ApprovalState::Approved);
//! # Ok::<(), adk_core::AdkError>(())
//! # }).unwrap();
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::principal::{ActingContext, SubjectRef};
use crate::{
    AdkError, ApprovalScope, ApprovalStore, Clock, DurableRunStore, ErrorCategory, ErrorComponent,
    ErrorDetails, Result, SystemClock, ToolApproval, ToolConfirmationRequest,
};

/// The most requests one [`ApprovalStore::list_requests`] page returns by default.
pub const DEFAULT_APPROVAL_PAGE: usize = 100;

/// How long after a request resolves [`ApprovalService::tick`] waits before applying an
/// outcome that is still unapplied, so it does not race the decision that is applying it.
pub const APPROVAL_REAPPLY_GRACE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// What an approver may do with a request.
///
/// # Example
///
/// ```rust
/// use adk_core::ApprovalAction;
///
/// assert_eq!(serde_json::to_value(ApprovalAction::Respond).unwrap(), "respond");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalAction {
    /// Approve the call as requested.
    Approve,
    /// Approve the call with substituted arguments.
    Edit,
    /// Refuse the call.
    Reject,
    /// Do not run the call; answer the model with a response instead.
    Respond,
}

impl ApprovalAction {
    /// Every action, the default set a request allows.
    pub const ALL: [Self; 4] = [Self::Approve, Self::Edit, Self::Reject, Self::Respond];
}

/// How much harm the call can do, as shown to approvers.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalRisk {
    /// Routine.
    Low,
    /// Needs a look.
    #[default]
    Medium,
    /// Needs a careful look.
    High,
    /// Irreversible or high-value.
    Critical,
}

/// Who may approve: one subject, or every member of a group.
///
/// Group membership is not looked up by the service; the caller deciding asserts the
/// groups the approver belongs to ([`ApprovalDecision::approver_groups`]), as its
/// authentication established them. `adk-server` takes them from the authenticated
/// request.
///
/// # Example
///
/// ```rust
/// use adk_core::ApproverSelector;
/// use adk_core::principal::SubjectRef;
///
/// let ana = SubjectRef::human("ana");
/// assert!(ApproverSelector::group("finance").matches(&ana, &["finance".to_string()]));
/// assert!(ApproverSelector::Subject(ana.clone()).matches(&ana, &[]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApproverSelector {
    /// Exactly this subject.
    Subject(SubjectRef),
    /// Any subject in this group.
    Group(String),
}

impl ApproverSelector {
    /// Selects the person with `id`.
    pub fn human(id: impl Into<String>) -> Self {
        Self::Subject(SubjectRef::human(id))
    }

    /// Selects every member of `group`.
    pub fn group(group: impl Into<String>) -> Self {
        Self::Group(group.into())
    }

    /// Whether `subject`, a member of `groups`, is selected.
    pub fn matches(&self, subject: &SubjectRef, groups: &[String]) -> bool {
        match self {
            Self::Subject(selected) => selected == subject,
            Self::Group(group) => groups.iter().any(|member_of| member_of == group),
        }
    }
}

/// One escalation step: once `after` has passed since the request was created, the
/// approvers in `to` may decide as well, and the notifiers hear about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationStep {
    /// Time from the request's creation to this step.
    #[serde(rename = "afterSecs", with = "duration_secs")]
    pub after: Duration,
    /// Approvers added by this step.
    pub to: Vec<ApproverSelector>,
}

/// What happens to a request nobody resolves before it expires.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ExpiryAction {
    /// Deny the call.
    #[default]
    Deny,
}

/// Who must approve a request, and what happens when nobody does.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalPolicy, ApproverSelector};
/// use std::time::Duration;
///
/// let policy = ApprovalPolicy::new([ApproverSelector::group("payments")])
///     .with_quorum(2)
///     .escalate_after(Duration::from_secs(900), [ApproverSelector::group("cfo-office")])
///     .expires_after(Duration::from_secs(3_600));
/// assert!(policy.forbid_self_approval);
/// assert!(policy.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPolicy {
    /// Who may approve from the start.
    pub approvers: Vec<ApproverSelector>,
    /// How many distinct approvers must approve. A decision made through a delegation
    /// counts for the delegator, so a delegator and their delegate count once.
    pub quorum: u32,
    /// Refuses decisions by the request's requester or acting agent, directly or
    /// through a delegation.
    pub forbid_self_approval: bool,
    /// Escalation steps, in order of `after`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalation: Vec<EscalationStep>,
    /// What expiry does.
    #[serde(default)]
    pub on_expiry: ExpiryAction,
    /// How long a submitted request stays open when it carries no expiry of its own.
    #[serde(
        default,
        rename = "expiresAfterSecs",
        with = "option_duration_secs",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_after: Option<Duration>,
}

impl ApprovalPolicy {
    /// A policy needing one approval from `approvers`, refusing self-approval, with no
    /// escalation or expiry.
    pub fn new(approvers: impl IntoIterator<Item = ApproverSelector>) -> Self {
        Self {
            approvers: approvers.into_iter().collect(),
            quorum: 1,
            forbid_self_approval: true,
            escalation: Vec::new(),
            on_expiry: ExpiryAction::Deny,
            expires_after: None,
        }
    }

    /// Requires `quorum` distinct approvals.
    #[must_use]
    pub fn with_quorum(mut self, quorum: u32) -> Self {
        self.quorum = quorum;
        self
    }

    /// Lets the requester approve their own call.
    #[must_use]
    pub fn allow_self_approval(mut self) -> Self {
        self.forbid_self_approval = false;
        self
    }

    /// Adds an escalation step.
    #[must_use]
    pub fn escalate_after(
        mut self,
        after: Duration,
        to: impl IntoIterator<Item = ApproverSelector>,
    ) -> Self {
        self.escalation.push(EscalationStep { after, to: to.into_iter().collect() });
        self
    }

    /// Expires requests `ttl` after submission.
    #[must_use]
    pub fn expires_after(mut self, ttl: Duration) -> Self {
        self.expires_after = Some(ttl);
        self
    }

    /// Checks the policy can ever be satisfied.
    ///
    /// # Errors
    ///
    /// Returns `approval.invalid_policy` when there are no approvers, the quorum is
    /// zero, or escalation steps are out of order.
    pub fn validate(&self) -> Result<()> {
        if self.approvers.is_empty() {
            return Err(invalid("approval.invalid_policy", "an approval policy needs approvers"));
        }
        if self.quorum == 0 {
            return Err(invalid(
                "approval.invalid_policy",
                "an approval quorum must be at least 1",
            ));
        }
        if self.escalation.windows(2).any(|pair| pair[0].after > pair[1].after) {
            return Err(invalid(
                "approval.invalid_policy",
                "escalation steps must be ordered by their `after` delay",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Requests and decisions
// ---------------------------------------------------------------------------

/// Where a request is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalState {
    /// Waiting for decisions.
    Pending,
    /// The quorum approved; the call executes.
    Approved,
    /// An approver rejected the call.
    Rejected,
    /// An approver answered instead of running the call.
    Responded,
    /// Nobody resolved it in time; the call is denied.
    Expired,
    /// Withdrawn; the call is denied.
    Cancelled,
}

impl ApprovalState {
    /// Every state.
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Approved,
        Self::Rejected,
        Self::Responded,
        Self::Expired,
        Self::Cancelled,
    ];

    /// The state's stored name, as it serializes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Responded => "responded",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether no further decision changes the request.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// One approver's decision on a request.
///
/// The service sets [`decided_at`](Self::decided_at) from its clock and
/// [`via_delegation`](Self::via_delegation) from the delegation it applied.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalAction, ApprovalDecision};
/// use adk_core::principal::SubjectRef;
/// use serde_json::json;
///
/// let edit = ApprovalDecision::edit(SubjectRef::human("ana"), json!({ "amount": 50 }));
/// assert_eq!(edit.kind, ApprovalAction::Edit);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecision {
    /// Who decided.
    pub approver: SubjectRef,
    /// Groups the approver belongs to, as the caller's authentication established them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approver_groups: Vec<String>,
    /// The delegator whose authority the approver acted under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_delegation: Option<SubjectRef>,
    /// What the approver did.
    pub kind: ApprovalAction,
    /// The substituted arguments of an [`Edit`](ApprovalAction::Edit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_args: Option<Value>,
    /// A rejection reason, or the text of a [`Respond`](ApprovalAction::Respond).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When the decision was made.
    pub decided_at: DateTime<Utc>,
}

impl ApprovalDecision {
    fn new(approver: SubjectRef, kind: ApprovalAction) -> Self {
        Self {
            approver,
            approver_groups: Vec::new(),
            via_delegation: None,
            kind,
            edited_args: None,
            reason: None,
            decided_at: Utc::now(),
        }
    }

    /// An approval.
    pub fn approve(approver: SubjectRef) -> Self {
        Self::new(approver, ApprovalAction::Approve)
    }

    /// An approval with `args` in place of the call's arguments.
    pub fn edit(approver: SubjectRef, args: Value) -> Self {
        Self { edited_args: Some(args), ..Self::new(approver, ApprovalAction::Edit) }
    }

    /// A rejection.
    pub fn reject(approver: SubjectRef, reason: impl Into<String>) -> Self {
        Self { reason: Some(reason.into()), ..Self::new(approver, ApprovalAction::Reject) }
    }

    /// A response reported to the model instead of running the call.
    pub fn respond(approver: SubjectRef, response: impl Into<String>) -> Self {
        Self { reason: Some(response.into()), ..Self::new(approver, ApprovalAction::Respond) }
    }

    /// Sets the groups the approver belongs to.
    #[must_use]
    pub fn with_groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.approver_groups = groups.into_iter().map(Into::into).collect();
        self
    }

    /// Acts under the delegation from `delegator`.
    #[must_use]
    pub fn via(mut self, delegator: SubjectRef) -> Self {
        self.via_delegation = Some(delegator);
        self
    }

    /// Sets the reason.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// The subject whose authority the decision counts for.
    pub fn authority(&self) -> &SubjectRef {
        self.via_delegation.as_ref().unwrap_or(&self.approver)
    }
}

/// A call waiting for approval, with its policy and the decisions made so far.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
///     ApprovalState, ApproverSelector, ToolConfirmationRequest};
/// use serde_json::json;
///
/// let call = ToolConfirmationRequest {
///     tool_name: "refund".into(),
///     function_call_id: Some("call-1".into()),
///     args: json!({ "amount": 500, "card": "4242424242424242" }),
/// };
/// let request = ApprovalRequest::new(
///     "req-1",
///     ApprovalScope::new("shop", "u-1", "s-1"),
///     &call,
///     ApprovalPolicy::new([ApproverSelector::group("support-leads")]),
/// )
/// .with_summary("Refund 500 USD")
/// .with_risk(ApprovalRisk::High)
/// .with_redacted_args(json!({ "amount": 500, "card": "****4242" }));
/// assert_eq!(request.fingerprint, call.fingerprint());
/// assert_eq!(request.state, ApprovalState::Pending);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    /// Stable identifier.
    pub id: String,
    /// The session the call belongs to.
    pub scope: ApprovalScope,
    /// The call's fingerprint, the key its decision is recorded under.
    pub fingerprint: String,
    /// The tool called.
    pub tool_name: String,
    /// The call's arguments as shown to approvers, with secrets removed.
    pub args_redacted: Value,
    /// The agent making the call and on whose behalf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting: Option<ActingContext>,
    /// Who asked for the call; refused as an approver under
    /// [`forbid_self_approval`](ApprovalPolicy::forbid_self_approval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<SubjectRef>,
    /// One line describing the call for approvers.
    pub summary: String,
    /// How much harm the call can do.
    #[serde(default)]
    pub risk: ApprovalRisk,
    /// When the request was submitted.
    pub created_at: DateTime<Utc>,
    /// When the request expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// What approvers may do.
    pub allowed_actions: Vec<ApprovalAction>,
    /// Who must approve.
    pub policy: ApprovalPolicy,
    /// The durable run waiting for the decision, woken when the request resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Incremented by every change; the compare-and-swap token for decisions.
    pub version: u64,
    /// Where the request is in its lifecycle.
    pub state: ApprovalState,
    /// Decisions so far, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ApprovalDecision>,
    /// How many escalation steps have fired.
    #[serde(default)]
    pub escalations_fired: u32,
    /// When the request reached a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Why the request was cancelled or expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Whether the resolution has been written to the store and the run woken.
    #[serde(default)]
    pub outcome_applied: bool,
}

impl ApprovalRequest {
    /// A pending request for `call` in `scope` under `policy`, allowing every action.
    pub fn new(
        id: impl Into<String>,
        scope: ApprovalScope,
        call: &ToolConfirmationRequest,
        policy: ApprovalPolicy,
    ) -> Self {
        Self {
            id: id.into(),
            scope,
            fingerprint: call.fingerprint(),
            tool_name: call.tool_name.clone(),
            args_redacted: call.args.clone(),
            acting: None,
            requester: None,
            summary: format!("Approve a call to '{}'", call.tool_name),
            risk: ApprovalRisk::Medium,
            created_at: Utc::now(),
            expires_at: None,
            allowed_actions: ApprovalAction::ALL.to_vec(),
            policy,
            run_id: None,
            version: 1,
            state: ApprovalState::Pending,
            decisions: Vec::new(),
            escalations_fired: 0,
            resolved_at: None,
            note: None,
            outcome_applied: false,
        }
    }

    /// The deterministic ID of the request for the call with `fingerprint`, held in
    /// `scope` since `held_since`.
    ///
    /// A call held again before its decision keeps its hold time in the
    /// [`ApprovalStore`], so it maps to the same request; a call held after an earlier
    /// decision gets a new one.
    pub fn id_for_call(
        scope: &ApprovalScope,
        fingerprint: &str,
        held_since: DateTime<Utc>,
    ) -> String {
        let key = serde_json::json!([
            scope.app_name,
            scope.user_id,
            scope.session_id,
            fingerprint,
            held_since.timestamp_micros()
        ]);
        let digest = crate::action_ledger::json_digest(&key);
        format!("apr_{}", digest.trim_start_matches("fnv1a128:"))
    }

    /// Sets the summary.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Sets the risk.
    #[must_use]
    pub fn with_risk(mut self, risk: ApprovalRisk) -> Self {
        self.risk = risk;
        self
    }

    /// Shows approvers `args` in place of the call's arguments.
    #[must_use]
    pub fn with_redacted_args(mut self, args: Value) -> Self {
        self.args_redacted = args;
        self
    }

    /// Sets the acting context, and the requester to its `on_behalf_of` subject when no
    /// requester is set.
    #[must_use]
    pub fn with_acting(mut self, acting: ActingContext) -> Self {
        if self.requester.is_none() {
            self.requester = acting.on_behalf_of.clone();
        }
        self.acting = Some(acting);
        self
    }

    /// Sets the requester.
    #[must_use]
    pub fn with_requester(mut self, requester: SubjectRef) -> Self {
        self.requester = Some(requester);
        self
    }

    /// Attaches the durable run waiting for the decision.
    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Sets the expiry.
    #[must_use]
    pub fn with_expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Restricts what approvers may do.
    #[must_use]
    pub fn with_allowed_actions(
        mut self,
        actions: impl IntoIterator<Item = ApprovalAction>,
    ) -> Self {
        self.allowed_actions = actions.into_iter().collect();
        self
    }

    /// Whether the request still needs work: pending, or resolved but not yet applied.
    pub fn is_open(&self) -> bool {
        self.state == ApprovalState::Pending || !self.outcome_applied
    }

    /// The approvers who may decide now: the policy's, plus those of every fired
    /// escalation step.
    pub fn eligible_approvers(&self) -> impl Iterator<Item = &ApproverSelector> {
        let fired = usize::try_from(self.escalations_fired).unwrap_or(usize::MAX);
        self.policy
            .approvers
            .iter()
            .chain(self.policy.escalation.iter().take(fired).flat_map(|step| step.to.iter()))
    }

    /// Whether `subject`, a member of `groups`, is among the
    /// [eligible approvers](Self::eligible_approvers).
    pub fn is_eligible(&self, subject: &SubjectRef, groups: &[String]) -> bool {
        self.eligible_approvers().any(|selector| selector.matches(subject, groups))
    }

    /// Whether `subject` is the requester or the acting agent or its principal.
    pub fn is_self(&self, subject: &SubjectRef) -> bool {
        self.requester.as_ref() == Some(subject)
            || self.acting.as_ref().is_some_and(|acting| {
                acting.on_behalf_of.as_ref() == Some(subject)
                    || SubjectRef::agent(&acting.agent.id) == *subject
            })
    }

    /// The authorities whose approvals count toward the quorum, and the arguments they
    /// approved when an [`Edit`](ApprovalAction::Edit) substituted them.
    ///
    /// An edit restarts the count: approvals before it approved other arguments.
    pub fn quorum_progress(&self) -> (Vec<SubjectRef>, Option<Value>) {
        let mut approvals: Vec<SubjectRef> = Vec::new();
        let mut args = None;
        for decision in &self.decisions {
            match decision.kind {
                ApprovalAction::Approve => {
                    if !approvals.contains(decision.authority()) {
                        approvals.push(decision.authority().clone());
                    }
                }
                ApprovalAction::Edit => {
                    approvals.clear();
                    approvals.push(decision.authority().clone());
                    args = decision.edited_args.clone();
                }
                ApprovalAction::Reject | ApprovalAction::Respond => {}
            }
        }
        (approvals, args)
    }

    /// The decision the governed path takes for the call once the request resolved, or
    /// `None` while it is pending.
    pub fn outcome(&self) -> Option<ToolApproval> {
        let last_reason = || self.decisions.last().and_then(|decision| decision.reason.clone());
        match self.state {
            ApprovalState::Pending => None,
            ApprovalState::Approved => {
                let approval = ToolApproval::approve();
                Some(match self.quorum_progress().1 {
                    Some(args) => approval.with_edited_args(args),
                    None => approval,
                })
            }
            ApprovalState::Rejected => {
                Some(ToolApproval::deny().with_message(match last_reason() {
                    Some(reason) => format!("the approver rejected it: {reason}"),
                    None => "the approver rejected it".to_string(),
                }))
            }
            ApprovalState::Responded => Some(ToolApproval::deny().with_message(format!(
                "the approver responded instead: {}",
                last_reason().unwrap_or_default()
            ))),
            ApprovalState::Expired => Some(
                ToolApproval::deny()
                    .with_message("the approval request expired before enough approvers decided"),
            ),
            ApprovalState::Cancelled => Some(ToolApproval::deny().with_message(match &self.note {
                Some(note) => format!("the approval request was cancelled: {note}"),
                None => "the approval request was cancelled".to_string(),
            })),
        }
    }

    /// The position of this request in listing order.
    pub fn cursor(&self) -> ApprovalCursor {
        ApprovalCursor { created_at: self.created_at, id: self.id.clone() }
    }
}

// ---------------------------------------------------------------------------
// Delegation
// ---------------------------------------------------------------------------

/// When a delegation applies: from `not_before` up to, not including, `not_after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationWindow {
    /// The first instant the delegation applies.
    pub not_before: DateTime<Utc>,
    /// The instant it stops applying.
    pub not_after: DateTime<Utc>,
}

impl DelegationWindow {
    /// A window from `not_before` to `not_after`.
    pub fn new(not_before: DateTime<Utc>, not_after: DateTime<Utc>) -> Self {
        Self { not_before, not_after }
    }

    /// Whether the window contains `at`.
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.not_before <= at && at < self.not_after
    }
}

/// Which requests a delegation covers. Empty fields cover everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationScope {
    /// Only requests from this application.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    /// Only calls to these tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_names: Vec<String>,
}

impl DelegationScope {
    /// Whether the scope covers `request`.
    pub fn covers(&self, request: &ApprovalRequest) -> bool {
        self.app_name.as_ref().is_none_or(|app| *app == request.scope.app_name)
            && (self.tool_names.is_empty() || self.tool_names.contains(&request.tool_name))
    }
}

/// Lets `to` decide in `from`'s place within a window and scope.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalDelegation, DelegationWindow};
/// use adk_core::principal::SubjectRef;
/// use chrono::{Duration, Utc};
///
/// let now = Utc::now();
/// let cover = ApprovalDelegation::new(
///     SubjectRef::human("ana"),
///     SubjectRef::human("bo"),
///     DelegationWindow::new(now, now + Duration::days(7)),
/// )
/// .with_from_groups(["finance"]);
/// assert!(cover.window.contains(now));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDelegation {
    /// Stable identifier.
    pub id: String,
    /// The delegator.
    pub from: SubjectRef,
    /// Groups the delegator belongs to, as their authentication established them when
    /// delegating; the delegate may act under them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from_groups: Vec<String>,
    /// The delegate.
    pub to: SubjectRef,
    /// When it applies.
    pub window: DelegationWindow,
    /// Which requests it covers.
    #[serde(default)]
    pub scope: DelegationScope,
    /// When it was granted.
    pub created_at: DateTime<Utc>,
}

impl ApprovalDelegation {
    /// A delegation from `from` to `to` over every request, with a fresh ID.
    pub fn new(from: SubjectRef, to: SubjectRef, window: DelegationWindow) -> Self {
        Self {
            id: format!("dlg_{}", uuid::Uuid::new_v4().simple()),
            from,
            from_groups: Vec::new(),
            to,
            window,
            scope: DelegationScope::default(),
            created_at: Utc::now(),
        }
    }

    /// Sets the delegator's groups.
    #[must_use]
    pub fn with_from_groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.from_groups = groups.into_iter().map(Into::into).collect();
        self
    }

    /// Restricts the requests covered.
    #[must_use]
    pub fn with_scope(mut self, scope: DelegationScope) -> Self {
        self.scope = scope;
        self
    }

    /// Whether the delegation lets its delegate decide `request` at `at`.
    pub fn applies_to(&self, request: &ApprovalRequest, at: DateTime<Utc>) -> bool {
        self.window.contains(at)
            && self.scope.covers(request)
            && request.is_eligible(&self.from, &self.from_groups)
    }
}

// ---------------------------------------------------------------------------
// Store queries
// ---------------------------------------------------------------------------

/// A position in the listing order of requests: creation time, then ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCursor {
    /// Creation time of the last request seen.
    pub created_at: DateTime<Utc>,
    /// ID of the last request seen.
    pub id: String,
}

impl ApprovalCursor {
    /// Whether `request` lists after this position.
    pub fn precedes(&self, request: &ApprovalRequest) -> bool {
        (request.created_at, request.id.as_str()) > (self.created_at, self.id.as_str())
    }

    /// Encodes the cursor as an opaque page token.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.created_at.timestamp_micros(), self.id)
    }

    /// Decodes a page token made by [`encode`](Self::encode).
    ///
    /// # Errors
    ///
    /// Returns `approval.invalid_cursor` for a malformed token.
    pub fn decode(token: &str) -> Result<Self> {
        let malformed = || invalid("approval.invalid_cursor", "malformed approval page cursor");
        let (micros, id) = token.split_once(':').ok_or_else(malformed)?;
        let created_at = micros
            .parse::<i64>()
            .ok()
            .and_then(DateTime::from_timestamp_micros)
            .ok_or_else(malformed)?;
        Ok(Self { created_at, id: id.to_string() })
    }
}

/// Selects stored requests for [`ApprovalStore::list_requests`].
///
/// Results are ordered by creation time, then ID, and start after
/// [`after`](Self::after). A backend that stores creation times at microsecond
/// precision compares cursors at that precision too.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalRequestQuery, ApprovalState};
///
/// let query = ApprovalRequestQuery::default().with_states([ApprovalState::Pending]).with_limit(20);
/// assert_eq!(query.limit, 20);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ApprovalRequestQuery {
    /// Only requests in one of these states; empty means any state.
    pub states: Vec<ApprovalState>,
    /// Only [open](ApprovalRequest::is_open) requests.
    pub open_only: bool,
    /// Only requests from this application.
    pub app_name: Option<String>,
    /// Only requests listing after this position.
    pub after: Option<ApprovalCursor>,
    /// At most this many requests.
    pub limit: usize,
}

impl Default for ApprovalRequestQuery {
    fn default() -> Self {
        Self {
            states: Vec::new(),
            open_only: false,
            app_name: None,
            after: None,
            limit: DEFAULT_APPROVAL_PAGE,
        }
    }
}

impl ApprovalRequestQuery {
    /// Only requests in `states`.
    #[must_use]
    pub fn with_states(mut self, states: impl IntoIterator<Item = ApprovalState>) -> Self {
        self.states = states.into_iter().collect();
        self
    }

    /// Only open requests.
    #[must_use]
    pub fn open_only(mut self) -> Self {
        self.open_only = true;
        self
    }

    /// Only requests from `app_name`.
    #[must_use]
    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// Starts after `cursor`.
    #[must_use]
    pub fn with_after(mut self, cursor: Option<ApprovalCursor>) -> Self {
        self.after = cursor;
        self
    }

    /// Returns at most `limit` requests.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Whether `request` matches every filter except the limit.
    pub fn matches(&self, request: &ApprovalRequest) -> bool {
        (self.states.is_empty() || self.states.contains(&request.state))
            && (!self.open_only || request.is_open())
            && self.app_name.as_ref().is_none_or(|app| *app == request.scope.app_name)
            && self.after.as_ref().is_none_or(|cursor| cursor.precedes(request))
    }
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

/// What [`ApprovalService::inbox`] lists.
///
/// With an approver or groups set, the inbox holds the requests they may decide now,
/// directly or through a live delegation, excluding those self-approval rules refuse.
/// With neither set it holds every request in the selected states.
///
/// # Example
///
/// ```rust
/// use adk_core::InboxQuery;
/// use adk_core::principal::SubjectRef;
///
/// let query = InboxQuery::for_approver(SubjectRef::human("ana"), ["finance"]).with_limit(10);
/// assert_eq!(query.limit, 10);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InboxQuery {
    /// The approver whose inbox to list.
    pub approver: Option<SubjectRef>,
    /// Groups the approver belongs to, or groups whose queue to list.
    pub groups: Vec<String>,
    /// States to list; empty means pending only.
    pub states: Vec<ApprovalState>,
    /// Only requests from this application.
    pub app_name: Option<String>,
    /// The page token from the previous page.
    pub cursor: Option<String>,
    /// Page size.
    pub limit: usize,
}

impl Default for InboxQuery {
    fn default() -> Self {
        Self {
            approver: None,
            groups: Vec::new(),
            states: Vec::new(),
            app_name: None,
            cursor: None,
            limit: 50,
        }
    }
}

impl InboxQuery {
    /// The pending requests `approver`, a member of `groups`, may decide.
    pub fn for_approver(
        approver: SubjectRef,
        groups: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            approver: Some(approver),
            groups: groups.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// The pending requests members of `group` may decide.
    pub fn for_group(group: impl Into<String>) -> Self {
        Self { groups: vec![group.into()], ..Self::default() }
    }

    /// Lists requests in `states` instead of pending ones.
    #[must_use]
    pub fn with_states(mut self, states: impl IntoIterator<Item = ApprovalState>) -> Self {
        self.states = states.into_iter().collect();
        self
    }

    /// Only requests from `app_name`.
    #[must_use]
    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// Continues from a page token.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Option<String>) -> Self {
        self.cursor = cursor;
        self
    }

    /// Sets the page size.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// One page of an inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxPage {
    /// The requests, oldest first.
    pub items: Vec<ApprovalRequest>,
    /// The token for the next page, when there may be one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Notification
// ---------------------------------------------------------------------------

/// Why an [`ApprovalNotification`] was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ApprovalEventKind {
    /// A request was submitted.
    Submitted,
    /// An escalation step fired.
    Escalated,
    /// An approver decided.
    Decided,
    /// The request resolved: approved, rejected, or answered.
    Resolved,
    /// The request expired.
    Expired,
    /// The request was cancelled.
    Cancelled,
}

/// What an [`ApprovalNotifier`] receives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalNotification {
    /// Unique per change of the request: its ID and version. Repeats of one change carry
    /// the same ID, so receivers can drop duplicates.
    pub id: String,
    /// Why it was sent.
    pub kind: ApprovalEventKind,
    /// The request after the change.
    pub request: ApprovalRequest,
    /// The decision, for [`Decided`](ApprovalEventKind::Decided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
    /// The step's position, from zero, for [`Escalated`](ApprovalEventKind::Escalated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_step: Option<u32>,
    /// When it happened.
    pub at: DateTime<Utc>,
}

/// Tells people about approval requests: on submission, escalation, decision,
/// resolution, expiry, and cancellation.
///
/// Notification is best effort: the service logs a notifier's failure and carries on,
/// because the request's state in the store is the source of truth.
/// `adk_server::approvals::WebhookApprovalNotifier` delivers signed webhooks; chat,
/// e-mail, and paging integrations implement this trait.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalNotification, ApprovalNotifier, Result, async_trait};
///
/// #[derive(Debug)]
/// struct LogNotifier;
///
/// #[async_trait]
/// impl ApprovalNotifier for LogNotifier {
///     async fn notify(&self, notification: &ApprovalNotification) -> Result<()> {
///         tracing::info!(approval.id = %notification.request.id, "approval event");
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait ApprovalNotifier: fmt::Debug + Send + Sync {
    /// Delivers one notification.
    async fn notify(&self, notification: &ApprovalNotification) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Routing calls held by the governed path
// ---------------------------------------------------------------------------

/// A call the governed path held for approval, offered to
/// [`ApprovalService::submit_call`].
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalCall {
    /// The session the call belongs to.
    pub scope: ApprovalScope,
    /// The held call, with its final arguments.
    pub request: ToolConfirmationRequest,
    /// Why the call needs approval, as reported to the model.
    pub reason: String,
    /// The agent that made the call.
    pub agent_name: String,
    /// The invocation the call was made in.
    pub invocation_id: String,
    /// The durable run executing the invocation, if any.
    pub run_id: Option<String>,
    /// The agent's acting context, if any.
    pub acting: Option<ActingContext>,
}

/// How an [`ApprovalRouter`] wants a held call presented to approvers.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRoute {
    /// Who must approve.
    pub policy: ApprovalPolicy,
    /// The summary; defaults to the governed path's reason.
    pub summary: Option<String>,
    /// The risk.
    pub risk: ApprovalRisk,
    /// The arguments shown to approvers; defaults to the call's.
    pub redacted_args: Option<Value>,
    /// What approvers may do; defaults to every action.
    pub allowed_actions: Option<Vec<ApprovalAction>>,
}

impl ApprovalRoute {
    /// A route under `policy`.
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self {
            policy,
            summary: None,
            risk: ApprovalRisk::default(),
            redacted_args: None,
            allowed_actions: None,
        }
    }

    /// Sets the summary.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// Sets the risk.
    #[must_use]
    pub fn with_risk(mut self, risk: ApprovalRisk) -> Self {
        self.risk = risk;
        self
    }

    /// Shows approvers `args` in place of the call's.
    #[must_use]
    pub fn with_redacted_args(mut self, args: Value) -> Self {
        self.redacted_args = Some(args);
        self
    }

    /// Restricts what approvers may do.
    #[must_use]
    pub fn with_allowed_actions(
        mut self,
        actions: impl IntoIterator<Item = ApprovalAction>,
    ) -> Self {
        self.allowed_actions = Some(actions.into_iter().collect());
        self
    }
}

/// Decides how a held call becomes an [`ApprovalRequest`], or that it does not.
///
/// An [`ApprovalPolicy`] is a router that applies itself to every call; a closure
/// `Fn(&ApprovalCall) -> Option<ApprovalRoute>` is one too.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApproverSelector};
///
/// let router = |call: &ApprovalCall| {
///     let amount = call.request.args["amount"].as_f64().unwrap_or(0.0);
///     let approvers = ApprovalPolicy::new([ApproverSelector::group("finance")]);
///     Some(if amount > 1_000.0 {
///         ApprovalRoute::new(approvers.with_quorum(2)).with_risk(ApprovalRisk::High)
///     } else {
///         ApprovalRoute::new(approvers)
///     })
/// };
/// # let _ = router;
/// ```
pub trait ApprovalRouter: Send + Sync {
    /// The route for `call`, or `None` to leave it held in the store only.
    fn route(&self, call: &ApprovalCall) -> Option<ApprovalRoute>;
}

impl ApprovalRouter for ApprovalPolicy {
    fn route(&self, _call: &ApprovalCall) -> Option<ApprovalRoute> {
        Some(ApprovalRoute::new(self.clone()))
    }
}

impl<F> ApprovalRouter for F
where
    F: Fn(&ApprovalCall) -> Option<ApprovalRoute> + Send + Sync,
{
    fn route(&self, call: &ApprovalCall) -> Option<ApprovalRoute> {
        self(call)
    }
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// What one [`ApprovalService::tick`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalTick {
    /// Requests that expired.
    pub expired: Vec<String>,
    /// Escalation steps that fired, as request ID and step position.
    pub escalated: Vec<(String, u32)>,
    /// Resolved requests whose outcome was applied after an earlier attempt failed.
    pub reapplied: Vec<String>,
}

/// An inbox of approval requests with quorum, delegation, escalation, and expiry.
///
/// See the [module documentation](self) for the lifecycle. [`StoreApprovalService`] is
/// the implementation over any [`ApprovalStore`].
#[async_trait]
pub trait ApprovalService: fmt::Debug + Send + Sync {
    /// Submits a request. Submitting an ID that exists returns the stored request
    /// unchanged, so retries are safe.
    ///
    /// # Errors
    ///
    /// Returns `approval.invalid_policy` or `approval.invalid_request` for a request
    /// that can never be resolved, or a store error.
    async fn submit(&self, request: ApprovalRequest) -> Result<ApprovalRequest>;

    /// Submits the request for a call the governed path held, as the service's router
    /// presents it. Returns `None` when the service routes no request for the call.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    async fn submit_call(&self, call: ApprovalCall) -> Result<Option<ApprovalRequest>>;

    /// Reads a request.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>>;

    /// Lists one page of an inbox. See [`InboxQuery`].
    ///
    /// # Errors
    ///
    /// Returns `approval.invalid_cursor` or a store error.
    async fn inbox(&self, query: InboxQuery) -> Result<InboxPage>;

    /// Records `decision` on the request at `expected_version`, resolving it when the
    /// decision completes the quorum, rejects, or responds.
    ///
    /// # Errors
    ///
    /// | Code | Category | When |
    /// |------|----------|------|
    /// | `approval.not_found` | NotFound | No request has `id` |
    /// | `approval.version_conflict` | Conflict | The request changed since `expected_version`; re-read it and decide again |
    /// | `approval.already_resolved` | Conflict | The request is no longer pending |
    /// | `approval.expired` | Conflict | The request is past its expiry |
    /// | `approval.already_decided` | Conflict | The approver's authority already counts toward the quorum |
    /// | `approval.not_an_approver` | Forbidden | The approver is not eligible, directly or by delegation |
    /// | `approval.delegation_inactive` | Forbidden | The only delegation that would make them eligible is outside its window |
    /// | `approval.self_approval` | Forbidden | The policy forbids self-approval and the approver, or their delegator, is the requester or acting agent |
    /// | `approval.action_not_allowed` | InvalidInput | The request does not allow the action, or an edit or response lacks its content |
    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalRequest>;

    /// Grants a delegation.
    ///
    /// # Errors
    ///
    /// Returns `approval.invalid_delegation` for an empty window or a self-delegation,
    /// or a store error.
    async fn delegate(&self, delegation: ApprovalDelegation) -> Result<ApprovalDelegation>;

    /// Revokes a delegation. Returns whether one was revoked.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    async fn revoke_delegation(&self, id: &str) -> Result<bool>;

    /// Lists the delegations granted to `to`.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>>;

    /// Cancels a pending request, denying the call.
    ///
    /// # Errors
    ///
    /// Returns `approval.not_found`, `approval.already_resolved`, or
    /// `approval.version_conflict` when it changed concurrently.
    async fn cancel(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest>;

    /// Expires overdue requests and fires due escalation steps, as of `now`, and
    /// applies resolutions an earlier failure left unapplied.
    ///
    /// Safe to call from several processes at once: each change is a compare-and-swap,
    /// so each expiry and each escalation step happens once.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    async fn tick(&self, now: DateTime<Utc>) -> Result<ApprovalTick>;

    /// The store decisions are written to, for the governed path to take them from.
    fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
        None
    }
}

/// The [`ApprovalService`] over an [`ApprovalStore`].
///
/// Durability and cross-process atomicity come from the store: on a durable backend
/// requests survive restarts and every process deciding through the same database sees
/// one winner per version.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{
///     ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, RunConfig, StoreApprovalService,
/// };
///
/// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
///     .with_router(ApprovalPolicy::new([ApproverSelector::group("ops")]));
/// let config = RunConfig::builder().approval_service(Arc::new(service)).build();
/// assert!(config.approval_store.is_some());
/// ```
pub struct StoreApprovalService {
    store: Arc<dyn ApprovalStore>,
    clock: Arc<dyn Clock>,
    notifiers: Vec<Arc<dyn ApprovalNotifier>>,
    run_store: Option<Arc<dyn DurableRunStore>>,
    router: Option<Arc<dyn ApprovalRouter>>,
    audit: Option<crate::audit_log::AuditTrail>,
}

impl fmt::Debug for StoreApprovalService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreApprovalService")
            .field("store", &self.store)
            .field("clock", &self.clock)
            .field("notifiers", &self.notifiers)
            .field("run_store", &self.run_store.is_some())
            .field("router", &self.router.is_some())
            .field("audit", &self.audit)
            .finish()
    }
}

impl StoreApprovalService {
    /// A service over `store`, reading the system clock, with no notifiers, run store,
    /// or router.
    pub fn new(store: Arc<dyn ApprovalStore>) -> Self {
        Self {
            store,
            clock: Arc::new(SystemClock),
            notifiers: Vec::new(),
            run_store: None,
            router: None,
            audit: None,
        }
    }

    /// Reads time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Adds a notifier.
    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn ApprovalNotifier>) -> Self {
        self.notifiers.push(notifier);
        self
    }

    /// Wakes runs in `run_store` when their request resolves.
    #[must_use]
    pub fn with_run_store(mut self, run_store: Arc<dyn DurableRunStore>) -> Self {
        self.run_store = Some(run_store);
        self
    }

    /// Routes calls held by the governed path into requests.
    #[must_use]
    pub fn with_router(mut self, router: impl ApprovalRouter + 'static) -> Self {
        self.router = Some(Arc::new(router));
        self
    }

    /// Records every approver's decision in `audit` before it takes effect.
    ///
    /// The record's actor is the approver, with the delegator they acted for in the
    /// delegation chain; it carries the digest of the arguments shown to approvers and
    /// of any edited arguments, never the arguments. Under
    /// [`AuditFailureMode::Block`](crate::audit_log::AuditFailureMode::Block) a decision
    /// whose record cannot be written is refused with `audit.record_failed`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
    /// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
    ///
    /// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
    ///     .with_audit(AuditTrail::new(Arc::new(InMemoryAuditRecorder::new())));
    /// # let _ = service;
    /// ```
    #[must_use]
    pub fn with_audit(mut self, audit: crate::audit_log::AuditTrail) -> Self {
        self.audit = Some(audit);
        self
    }

    async fn notify(
        &self,
        kind: ApprovalEventKind,
        request: &ApprovalRequest,
        decision: Option<&ApprovalDecision>,
        escalation_step: Option<u32>,
    ) {
        if self.notifiers.is_empty() {
            return;
        }
        let notification = ApprovalNotification {
            id: format!("{}:{}", request.id, request.version),
            kind,
            request: request.clone(),
            decision: decision.cloned(),
            escalation_step,
            at: self.clock.now(),
        };
        for notifier in &self.notifiers {
            if let Err(error) = notifier.notify(&notification).await {
                tracing::warn!(
                    approval.id = %request.id,
                    approval.event = ?kind,
                    error = %error,
                    "approval notifier failed"
                );
            }
        }
    }

    /// Writes a resolved request's outcome to the store, wakes its run, and marks it
    /// applied. Returns the request as it now stands.
    ///
    /// A `recovering` apply writes the outcome only while the call is still pending in
    /// the store: `decide` clears the pending entry in the same atomic step that records
    /// the decision, so a missing entry means the outcome was written — and possibly
    /// already consumed — and must not be written twice.
    async fn apply(&self, request: ApprovalRequest, recovering: bool) -> Result<ApprovalRequest> {
        let Some(outcome) = request.outcome() else {
            return Ok(request);
        };
        if request.outcome_applied {
            return Ok(request);
        }
        let write = !recovering
            || self
                .store
                .pending(&request.scope)
                .await?
                .iter()
                .any(|entry| entry.fingerprint == request.fingerprint);
        if write {
            self.store.decide(&request.scope, &request.fingerprint, outcome).await?;
        }
        if let (Some(run_id), Some(runs)) = (&request.run_id, &self.run_store)
            && let Err(error) = runs.wake(run_id).await
        {
            // The decision is in the store, so the run's next attempt still takes it.
            tracing::warn!(approval.id = %request.id, run.id = %run_id, error = %error, "could not wake the run waiting for an approval");
        }
        let mut applied = request.clone();
        applied.outcome_applied = true;
        applied.version = request.version + 1;
        if self.store.replace_request(&applied, request.version).await? {
            Ok(applied)
        } else {
            // Another process applied it first; the outcome it wrote is the same.
            Ok(self.store.get_request(&request.id).await?.unwrap_or(applied))
        }
    }

    /// Writes `next` over the request at `expected` or reports the conflict.
    async fn swap(&self, next: &ApprovalRequest, expected: u64) -> Result<()> {
        if self.store.replace_request(next, expected).await? {
            return Ok(());
        }
        let current = self.store.get_request(&next.id).await?;
        Err(match current {
            Some(current) => version_conflict(&next.id, expected, current.version),
            None => not_found(&next.id),
        })
    }

    async fn live_delegations(
        &self,
        to: &SubjectRef,
        at: DateTime<Utc>,
    ) -> Result<Vec<ApprovalDelegation>> {
        Ok(self
            .store
            .delegations_to(to)
            .await?
            .into_iter()
            .filter(|delegation| delegation.window.contains(at))
            .collect())
    }

    /// The authority `approver` may decide `request` under at `at`, or why not.
    async fn authority(
        &self,
        request: &ApprovalRequest,
        decision: &ApprovalDecision,
        at: DateTime<Utc>,
    ) -> Result<Option<SubjectRef>> {
        let approver = &decision.approver;
        let forbid_self = request.policy.forbid_self_approval;
        if forbid_self && request.is_self(approver) {
            return Err(forbidden(
                "approval.self_approval",
                format!("'{}' may not approve their own call; ask another approver", approver.id),
            ));
        }
        let direct = request.is_eligible(approver, &decision.approver_groups);
        if direct && decision.via_delegation.is_none() {
            return Ok(None);
        }
        let delegations = self.store.delegations_to(approver).await?;
        let candidates: Vec<&ApprovalDelegation> = delegations
            .iter()
            .filter(|delegation| {
                decision.via_delegation.as_ref().is_none_or(|from| *from == delegation.from)
                    && delegation.scope.covers(request)
                    && request.is_eligible(&delegation.from, &delegation.from_groups)
            })
            .collect();
        if let Some(live) = candidates.iter().find(|delegation| delegation.window.contains(at)) {
            if forbid_self && request.is_self(&live.from) {
                return Err(forbidden(
                    "approval.self_approval",
                    format!(
                        "'{}' may not approve on behalf of '{}', the requester",
                        approver.id, live.from.id
                    ),
                ));
            }
            return Ok(Some(live.from.clone()));
        }
        if !candidates.is_empty() {
            return Err(forbidden(
                "approval.delegation_inactive",
                format!(
                    "the delegation that lets '{}' approve this request is outside its window",
                    approver.id
                ),
            ));
        }
        Err(forbidden(
            "approval.not_an_approver",
            format!("'{}' is not an approver of request '{}'", approver.id, request.id),
        ))
    }

    /// Whether the inbox of `approver` in `groups`, with `delegations`, holds `request`.
    fn in_inbox(
        request: &ApprovalRequest,
        approver: Option<&SubjectRef>,
        groups: &[String],
        delegations: &[ApprovalDelegation],
        at: DateTime<Utc>,
    ) -> bool {
        let forbid_self = request.policy.forbid_self_approval;
        let Some(approver) = approver else {
            return groups.is_empty()
                || request.eligible_approvers().any(|selector| {
                    matches!(selector, ApproverSelector::Group(group) if groups.contains(group))
                });
        };
        if forbid_self && request.is_self(approver) {
            return false;
        }
        request.is_eligible(approver, groups)
            || delegations.iter().any(|delegation| {
                delegation.applies_to(request, at)
                    && !(forbid_self && request.is_self(&delegation.from))
            })
    }
}

#[async_trait]
impl ApprovalService for StoreApprovalService {
    async fn submit(&self, mut request: ApprovalRequest) -> Result<ApprovalRequest> {
        request.policy.validate()?;
        if request.id.is_empty() {
            return Err(invalid("approval.invalid_request", "an approval request needs an id"));
        }
        if request.allowed_actions.is_empty() {
            return Err(invalid(
                "approval.invalid_request",
                "an approval request must allow at least one action",
            ));
        }
        let now = self.clock.now();
        request.created_at = now;
        if request.expires_at.is_none()
            && let Some(ttl) = request.policy.expires_after
        {
            let ttl = chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::MAX);
            request.expires_at =
                Some(now.checked_add_signed(ttl).unwrap_or(DateTime::<Utc>::MAX_UTC));
        }
        request.version = 1;
        request.state = ApprovalState::Pending;
        request.decisions.clear();
        request.escalations_fired = 0;
        request.resolved_at = None;
        request.note = None;
        request.outcome_applied = false;
        let stored = self.store.create_request(&request).await?;
        if stored == request {
            tracing::info!(approval.id = %request.id, tool.name = %request.tool_name, "approval request submitted");
            self.notify(ApprovalEventKind::Submitted, &stored, None, None).await;
        }
        Ok(stored)
    }

    async fn submit_call(&self, call: ApprovalCall) -> Result<Option<ApprovalRequest>> {
        let Some(route) = self.router.as_ref().and_then(|router| router.route(&call)) else {
            return Ok(None);
        };
        self.store.record_pending(&call.scope, &call.request).await?;
        let fingerprint = call.request.fingerprint();
        let Some(held) = self
            .store
            .pending(&call.scope)
            .await?
            .into_iter()
            .find(|entry| entry.fingerprint == fingerprint)
        else {
            // Decided between the hold and now; the next run takes the decision.
            return Ok(None);
        };
        let id = ApprovalRequest::id_for_call(&call.scope, &fingerprint, held.requested_at);
        let mut request = ApprovalRequest::new(id, call.scope.clone(), &call.request, route.policy)
            .with_summary(route.summary.unwrap_or(call.reason))
            .with_risk(route.risk);
        if let Some(args) = route.redacted_args {
            request = request.with_redacted_args(args);
        }
        if let Some(actions) = route.allowed_actions {
            request = request.with_allowed_actions(actions);
        }
        if let Some(acting) = call.acting {
            request = request.with_acting(acting);
        }
        if request.requester.is_none() {
            request = request.with_requester(SubjectRef::human(call.scope.user_id.clone()));
        }
        if let Some(run_id) = call.run_id {
            request = request.with_run_id(run_id);
        }
        self.submit(request).await.map(Some)
    }

    async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.store.get_request(id).await
    }

    async fn inbox(&self, query: InboxQuery) -> Result<InboxPage> {
        let now = self.clock.now();
        let delegations = match &query.approver {
            Some(approver) => self.live_delegations(approver, now).await?,
            None => Vec::new(),
        };
        let states = if query.states.is_empty() {
            vec![ApprovalState::Pending]
        } else {
            query.states.clone()
        };
        let limit = query.limit.max(1);
        let mut after = query.cursor.as_deref().map(ApprovalCursor::decode).transpose()?;
        let mut items = Vec::new();
        loop {
            let mut store_query = ApprovalRequestQuery::default()
                .with_states(states.iter().copied())
                .with_after(after.clone())
                .with_limit(DEFAULT_APPROVAL_PAGE);
            store_query.app_name = query.app_name.clone();
            let batch = self.store.list_requests(&store_query).await?;
            let exhausted = batch.len() < DEFAULT_APPROVAL_PAGE;
            for request in batch {
                after = Some(request.cursor());
                if Self::in_inbox(
                    &request,
                    query.approver.as_ref(),
                    &query.groups,
                    &delegations,
                    now,
                ) {
                    items.push(request);
                    if items.len() == limit {
                        return Ok(InboxPage {
                            items,
                            next_cursor: after.map(|cursor| cursor.encode()),
                        });
                    }
                }
            }
            if exhausted {
                return Ok(InboxPage { items, next_cursor: None });
            }
        }
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        mut decision: ApprovalDecision,
    ) -> Result<ApprovalRequest> {
        let now = self.clock.now();
        let request = self.store.get_request(id).await?.ok_or_else(|| not_found(id))?;
        if request.version != expected_version {
            return Err(version_conflict(id, expected_version, request.version));
        }
        if request.state.is_terminal() {
            return Err(conflict(
                "approval.already_resolved",
                format!("approval request '{id}' is already {}", request.state.as_str()),
            ));
        }
        if request.expires_at.is_some_and(|at| at <= now) {
            return Err(conflict(
                "approval.expired",
                format!("approval request '{id}' expired; the call is denied"),
            ));
        }
        if !request.allowed_actions.contains(&decision.kind) {
            return Err(invalid(
                "approval.action_not_allowed",
                format!("approval request '{id}' does not allow {:?}", decision.kind),
            ));
        }
        match decision.kind {
            ApprovalAction::Edit if decision.edited_args.is_none() => {
                return Err(invalid(
                    "approval.action_not_allowed",
                    "an edit must carry the edited arguments",
                ));
            }
            ApprovalAction::Respond
                if decision.reason.as_deref().is_none_or(|text| text.trim().is_empty()) =>
            {
                return Err(invalid(
                    "approval.action_not_allowed",
                    "a response must carry the text to report to the model",
                ));
            }
            ApprovalAction::Approve | ApprovalAction::Reject => decision.edited_args = None,
            ApprovalAction::Edit | ApprovalAction::Respond => {}
        }
        decision.via_delegation = self.authority(&request, &decision, now).await?;
        decision.decided_at = now;
        if matches!(decision.kind, ApprovalAction::Approve)
            && request.quorum_progress().0.contains(decision.authority())
        {
            return Err(conflict(
                "approval.already_decided",
                format!(
                    "'{}' already approved request '{id}'; another approver must decide",
                    decision.authority().id
                ),
            ));
        }

        if let Some(trail) = &self.audit {
            use crate::audit_log::{AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind};
            let audited = match decision.kind {
                ApprovalAction::Approve | ApprovalAction::Edit => AuditDecision::Approved,
                ApprovalAction::Reject | ApprovalAction::Respond => AuditDecision::Rejected,
            };
            let mut actor = AuditActor::subject(decision.approver.clone());
            actor.delegation_chain.extend(decision.via_delegation.clone());
            let action = serde_json::to_value(decision.kind).unwrap_or(Value::Null);
            let mut draft = AuditRecordDraft::new(AuditRecordKind::Approval, &request.tool_name)
                .actor(actor)
                .args_digest(trail.digest(&request.args_redacted))
                .decision(audited)
                .ext("source", Value::String("approval_service".to_string()))
                .ext("approvalId", Value::String(id.to_string()))
                .ext("approvalAction", action)
                .ext("version", Value::from(expected_version + 1))
                .ext("sessionId", Value::String(request.scope.session_id.clone()));
            if let Some(acting) = &request.acting {
                draft = draft.ext("actingAgent", Value::String(acting.agent.id.to_string()));
            }
            if let Some(edited) = &decision.edited_args {
                draft = draft.ext("editedArgsDigest", Value::String(trail.digest(edited)));
            }
            trail.record(draft).await?;
        }

        let mut next = request.clone();
        next.decisions.push(decision.clone());
        next.version = expected_version + 1;
        next.state = match decision.kind {
            ApprovalAction::Reject => ApprovalState::Rejected,
            ApprovalAction::Respond => ApprovalState::Responded,
            ApprovalAction::Approve | ApprovalAction::Edit => {
                let approvals = next.quorum_progress().0.len();
                if approvals >= usize::try_from(next.policy.quorum).unwrap_or(usize::MAX) {
                    ApprovalState::Approved
                } else {
                    ApprovalState::Pending
                }
            }
        };
        if next.state.is_terminal() {
            next.resolved_at = Some(now);
        }
        self.swap(&next, expected_version).await?;
        tracing::info!(
            approval.id = %id,
            approval.action = ?decision.kind,
            approval.state = next.state.as_str(),
            "approval decision recorded"
        );
        self.notify(ApprovalEventKind::Decided, &next, Some(&decision), None).await;
        if !next.state.is_terminal() {
            return Ok(next);
        }
        let applied = self.apply(next, false).await?;
        self.notify(ApprovalEventKind::Resolved, &applied, None, None).await;
        Ok(applied)
    }

    async fn delegate(&self, mut delegation: ApprovalDelegation) -> Result<ApprovalDelegation> {
        if delegation.window.not_after <= delegation.window.not_before {
            return Err(invalid(
                "approval.invalid_delegation",
                "a delegation window must end after it starts",
            ));
        }
        if delegation.from == delegation.to {
            return Err(invalid(
                "approval.invalid_delegation",
                "a subject cannot delegate to itself",
            ));
        }
        if delegation.id.is_empty() {
            return Err(invalid("approval.invalid_delegation", "a delegation needs an id"));
        }
        delegation.created_at = self.clock.now();
        self.store.put_delegation(&delegation).await?;
        tracing::info!(
            delegation.id = %delegation.id,
            delegation.from = %delegation.from.id,
            delegation.to = %delegation.to.id,
            "approval delegation granted"
        );
        Ok(delegation)
    }

    async fn revoke_delegation(&self, id: &str) -> Result<bool> {
        self.store.delete_delegation(id).await
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        self.store.delegations_to(to).await
    }

    async fn cancel(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest> {
        let request = self.store.get_request(id).await?.ok_or_else(|| not_found(id))?;
        if request.state.is_terminal() {
            return Err(conflict(
                "approval.already_resolved",
                format!("approval request '{id}' is already {}", request.state.as_str()),
            ));
        }
        let mut next = request.clone();
        next.state = ApprovalState::Cancelled;
        next.version = request.version + 1;
        next.resolved_at = Some(self.clock.now());
        next.note = reason;
        self.swap(&next, request.version).await?;
        let applied = self.apply(next, false).await?;
        self.notify(ApprovalEventKind::Cancelled, &applied, None, None).await;
        Ok(applied)
    }

    async fn tick(&self, now: DateTime<Utc>) -> Result<ApprovalTick> {
        let mut report = ApprovalTick::default();
        let mut after = None;
        loop {
            let query = ApprovalRequestQuery::default().open_only().with_after(after.clone());
            let batch = self.store.list_requests(&query).await?;
            let exhausted = batch.len() < query.limit;
            for request in batch {
                after = Some(request.cursor());
                if request.state.is_terminal() {
                    let grace = chrono::Duration::from_std(APPROVAL_REAPPLY_GRACE)
                        .unwrap_or(chrono::Duration::MAX);
                    let settled = request
                        .resolved_at
                        .and_then(|at| at.checked_add_signed(grace))
                        .is_none_or(|at| at <= now);
                    if settled {
                        let applied = self.apply(request, true).await?;
                        report.reapplied.push(applied.id);
                    }
                    continue;
                }
                if request.expires_at.is_some_and(|at| at <= now) {
                    let mut next = request.clone();
                    next.state = ApprovalState::Expired;
                    next.version = request.version + 1;
                    next.resolved_at = Some(now);
                    if !self.store.replace_request(&next, request.version).await? {
                        continue;
                    }
                    tracing::info!(approval.id = %next.id, "approval request expired; denying the call");
                    let applied = self.apply(next, false).await?;
                    self.notify(ApprovalEventKind::Expired, &applied, None, None).await;
                    report.expired.push(applied.id);
                    continue;
                }
                let due = request
                    .policy
                    .escalation
                    .iter()
                    .take_while(|step| {
                        chrono::Duration::from_std(step.after)
                            .ok()
                            .and_then(|after| request.created_at.checked_add_signed(after))
                            .is_some_and(|at| at <= now)
                    })
                    .count();
                let due = u32::try_from(due).unwrap_or(u32::MAX);
                if due <= request.escalations_fired {
                    continue;
                }
                let mut next = request.clone();
                next.escalations_fired = due;
                next.version = request.version + 1;
                if !self.store.replace_request(&next, request.version).await? {
                    continue;
                }
                for step in request.escalations_fired..due {
                    tracing::info!(approval.id = %next.id, approval.escalation_step = step, "approval request escalated");
                    self.notify(ApprovalEventKind::Escalated, &next, None, Some(step)).await;
                    report.escalated.push((next.id.clone(), step));
                }
            }
            if exhausted {
                return Ok(report);
            }
        }
    }

    fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
        Some(self.store.clone())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn error(category: ErrorCategory, code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Agent, category, code, message.into())
}

fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    error(ErrorCategory::InvalidInput, code, message)
}

fn forbidden(code: &'static str, message: impl Into<String>) -> AdkError {
    error(ErrorCategory::Forbidden, code, message)
}

fn conflict(code: &'static str, message: impl Into<String>) -> AdkError {
    error(ErrorCategory::Conflict, code, message)
}

fn not_found(id: &str) -> AdkError {
    error(ErrorCategory::NotFound, "approval.not_found", format!("no approval request '{id}'"))
}

fn version_conflict(id: &str, expected: u64, current: u64) -> AdkError {
    let mut details = ErrorDetails::default();
    details.metadata.insert("current_version".to_string(), current.into());
    conflict(
        "approval.version_conflict",
        format!(
            "approval request '{id}' is at version {current}, not {expected}; another decision \
             landed first, so re-read the request and decide again"
        ),
    )
    .with_details(details)
}

/// The error a store method reports when the backend does not implement it.
pub(crate) fn store_unsupported(operation: &str) -> AdkError {
    error(
        ErrorCategory::Unsupported,
        "approval.store_unsupported",
        format!(
            "this approval store does not implement `{operation}`, which the approval service \
             needs; use a store from adk-core or adk-session, or implement the method"
        ),
    )
}

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_secs)
    }
}

mod option_duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    #[allow(clippy::ref_option)] // serde's `with` passes the field by reference
    pub fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => serializer.serialize_some(&duration.as_secs()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<u64>::deserialize(deserializer)?.map(Duration::from_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policies_serialize_in_camel_case_with_whole_seconds() {
        let policy = ApprovalPolicy::new([ApproverSelector::group("finance")])
            .with_quorum(2)
            .escalate_after(Duration::from_secs(900), [ApproverSelector::human("cfo")])
            .expires_after(Duration::from_secs(3_600));
        let value = serde_json::to_value(&policy).unwrap();
        assert_eq!(
            value,
            json!({
                "approvers": [{ "group": "finance" }],
                "quorum": 2,
                "forbidSelfApproval": true,
                "escalation": [{
                    "afterSecs": 900,
                    "to": [{ "subject": { "kind": "human", "id": "cfo" } }]
                }],
                "onExpiry": "deny",
                "expiresAfterSecs": 3600
            })
        );
        assert_eq!(serde_json::from_value::<ApprovalPolicy>(value).unwrap(), policy);
    }

    #[test]
    fn cursors_round_trip_ids_containing_colons() {
        let cursor = ApprovalCursor { created_at: Utc::now(), id: "a:b/ü".into() };
        let decoded = ApprovalCursor::decode(&cursor.encode()).unwrap();
        assert_eq!(decoded.id, cursor.id);
        assert_eq!(decoded.created_at.timestamp_micros(), cursor.created_at.timestamp_micros());
        assert_eq!(ApprovalCursor::decode("nope").unwrap_err().code, "approval.invalid_cursor");
    }

    #[test]
    fn call_ids_change_with_the_hold_time() {
        let scope = ApprovalScope::new("app", "u", "s");
        let at = Utc::now();
        let id = ApprovalRequest::id_for_call(&scope, "fp", at);
        assert_eq!(id, ApprovalRequest::id_for_call(&scope, "fp", at));
        assert_ne!(
            id,
            ApprovalRequest::id_for_call(&scope, "fp", at + chrono::Duration::seconds(1))
        );
        assert!(id.starts_with("apr_"));
    }

    #[test]
    fn invalid_policies_are_refused() {
        assert!(ApprovalPolicy::new([]).validate().is_err());
        let zero = ApprovalPolicy::new([ApproverSelector::group("g")]).with_quorum(0);
        assert_eq!(zero.validate().unwrap_err().code, "approval.invalid_policy");
        let unordered = ApprovalPolicy::new([ApproverSelector::group("g")])
            .escalate_after(Duration::from_secs(60), [])
            .escalate_after(Duration::from_secs(30), []);
        assert!(unordered.validate().is_err());
    }

    #[tokio::test]
    async fn resolving_a_request_wakes_its_waiting_run() {
        use crate::{
            ApprovalStore, DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, NewRun,
            RunState,
        };

        let runs = Arc::new(InMemoryRunStore::new());
        let enqueued =
            runs.enqueue(NewRun::new("q", "app", "u", "agent", json!({}))).await.unwrap();
        let run_id = enqueued.record().unwrap().run_id.clone();
        let claimed = runs.claim("q", "worker", Duration::from_secs(60)).await.unwrap().unwrap();
        // The worker parks the run until the approval arrives.
        runs.wait(&run_id, claimed.epoch, None).await.unwrap();
        assert_eq!(runs.get(&run_id).await.unwrap().unwrap().state, RunState::Waiting);

        let store = Arc::new(InMemoryApprovalStore::new());
        let service = StoreApprovalService::new(store.clone()).with_run_store(runs.clone());
        let call = ToolConfirmationRequest {
            tool_name: "pay".into(),
            function_call_id: Some("c1".into()),
            args: json!({ "amount": 5 }),
        };
        let scope = ApprovalScope::new("app", "u", "s");
        let request = service
            .submit(
                ApprovalRequest::new(
                    "req",
                    scope.clone(),
                    &call,
                    ApprovalPolicy::new([ApproverSelector::human("ana")]),
                )
                .with_run_id(run_id.clone()),
            )
            .await
            .unwrap();
        service
            .decide(
                &request.id,
                request.version,
                ApprovalDecision::approve(SubjectRef::human("ana")),
            )
            .await
            .unwrap();

        assert!(store.has_decision(&scope, &call.fingerprint()).await.unwrap());
        let woken = runs.claim("q", "worker", Duration::from_secs(60)).await.unwrap();
        assert_eq!(
            woken.map(|claim| claim.record.run_id),
            Some(run_id),
            "the decision woke the run"
        );
    }

    #[tokio::test]
    async fn an_approver_decision_is_audited_before_it_takes_effect() {
        use crate::audit_log::{
            AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecorder,
            AuditTrail, InMemoryAuditRecorder,
        };
        use crate::{ApprovalStore, InMemoryApprovalStore};

        #[derive(Debug)]
        struct Broken;
        #[async_trait::async_trait]
        impl AuditRecorder for Broken {
            fn digest(&self, value: &Value) -> String {
                crate::json_digest(value)
            }
            async fn record(&self, _draft: AuditRecordDraft) -> Result<()> {
                Err(crate::AdkError::session("audit store down"))
            }
        }

        let call = ToolConfirmationRequest {
            tool_name: "pay".into(),
            function_call_id: Some("c1".into()),
            args: json!({ "to": "acct-secret", "amount": 5 }),
        };
        let scope = ApprovalScope::new("app", "u", "s");
        let request = |id: &str| {
            ApprovalRequest::new(
                id,
                scope.clone(),
                &call,
                ApprovalPolicy::new([ApproverSelector::human("ana")]),
            )
        };

        let recorder = InMemoryAuditRecorder::new();
        let store = Arc::new(InMemoryApprovalStore::new());
        let service = StoreApprovalService::new(store.clone())
            .with_audit(AuditTrail::new(Arc::new(recorder.clone())));
        let submitted = service.submit(request("req")).await.unwrap();
        service
            .decide(
                &submitted.id,
                submitted.version,
                ApprovalDecision::approve(SubjectRef::human("ana")),
            )
            .await
            .unwrap();
        assert_eq!(
            recorder.records(),
            vec![
                AuditRecordDraft::new(AuditRecordKind::Approval, "pay")
                    .actor(AuditActor::subject(SubjectRef::human("ana")))
                    .args_digest(recorder.digest(&submitted.args_redacted))
                    .decision(AuditDecision::Approved)
                    .ext("source", json!("approval_service"))
                    .ext("approvalId", json!(submitted.id))
                    .ext("approvalAction", json!("approve"))
                    .ext("version", json!(submitted.version + 1))
                    .ext("sessionId", json!("s"))
            ]
        );

        let blocked_store = Arc::new(InMemoryApprovalStore::new());
        let blocked = StoreApprovalService::new(blocked_store.clone())
            .with_audit(AuditTrail::new(Arc::new(Broken)));
        let submitted = blocked.submit(request("req-2")).await.unwrap();
        let error = blocked
            .decide(
                &submitted.id,
                submitted.version,
                ApprovalDecision::approve(SubjectRef::human("ana")),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "audit.record_failed");
        assert!(!blocked_store.has_decision(&scope, &call.fingerprint()).await.unwrap());
        assert_eq!(
            blocked.get(&submitted.id).await.unwrap().unwrap().state,
            ApprovalState::Pending
        );
    }
}
