use std::future::Future;
use std::sync::Arc;

use adk_auth::AuditOutcome;
use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails, Result, SpendLedger};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use crate::auth::{
    AuthenticatedPaymentRequest, PaymentAuditor, PaymentOperation as AuditPaymentOperation,
};

use crate::domain::{ProtocolDescriptor, TransactionRecord};
use crate::guardrail::{
    PaymentOperation, PaymentPolicyContext, PaymentPolicyDecision, PaymentPolicyFinding,
    PaymentPolicyGuardrail, PaymentPolicySet, SpendLimitGuardrail, usd_micro_amount,
};
use crate::kernel::commands::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand, OrderUpdateCommand,
    TransactionLookup, UpdateCheckoutCommand,
};
use crate::kernel::service::MerchantCheckoutService;

/// Error code returned when a payment policy denies a checkout.
pub const PAYMENT_POLICY_DENIED_CODE: &str = "payments.policy.denied";
/// Error code returned when a payment policy escalated a checkout and no approval arrived.
pub const PAYMENT_APPROVAL_REQUIRED_CODE: &str = "payments.policy.approval_required";
/// Error code returned when an escalated checkout was explicitly refused.
pub const PAYMENT_APPROVAL_DENIED_CODE: &str = "payments.policy.approval_denied";
/// Error code returned when an approver approved an escalated operation with substituted
/// arguments: the operation did not run as proposed, and the error's `editedArgs` detail
/// carries the arguments to re-issue it with. The payment tools re-issue the call
/// themselves (spec 3.1 §10 C3).
pub const PAYMENT_APPROVAL_EDITED_CODE: &str = "payments.policy.approval_edited";

/// Outcome of asking for approval of an escalated payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentApproval {
    /// The payment may proceed.
    Approved,
    /// The payment must not proceed.
    Denied,
    /// No decision is available yet; the payment does not proceed now.
    Pending,
}

/// What an approver decided about an escalated payment: the approval, the arguments it
/// substituted for the call's own, and what it said (spec 3.1 §10 C3).
///
/// # Example
///
/// ```
/// use adk_payments::kernel::{PaymentApproval, PaymentDecision};
/// use serde_json::json;
///
/// let decision = PaymentDecision::new(PaymentApproval::Approved)
///     .with_edited_args(json!({ "amount": 40 }))
///     .with_message("approved at 40");
/// assert_eq!(decision.approval, PaymentApproval::Approved);
/// assert_eq!(decision.message.as_deref(), Some("approved at 40"));
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PaymentDecision {
    /// Whether the payment may proceed.
    pub approval: PaymentApproval,
    /// Arguments the approver substituted for the call's own. An approved payment with
    /// edited arguments is not made as proposed: the governed service returns
    /// [`PAYMENT_APPROVAL_EDITED_CODE`] and the caller re-issues it with these arguments.
    pub edited_args: Option<Value>,
    /// What the approver said: a reason for a refusal, or a note on an approval. It is
    /// recorded with the decision and reported with a refusal.
    pub message: Option<String>,
}

impl PaymentDecision {
    /// A decision with no edited arguments and no message.
    pub fn new(approval: PaymentApproval) -> Self {
        Self { approval, edited_args: None, message: None }
    }

    /// Sets the arguments the approver substituted. See
    /// [`edited_args`](Self::edited_args).
    #[must_use]
    pub fn with_edited_args(mut self, args: Value) -> Self {
        self.edited_args = Some(args);
        self
    }

    /// Sets what the approver said. See [`message`](Self::message).
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

impl From<PaymentApproval> for PaymentDecision {
    fn from(approval: PaymentApproval) -> Self {
        Self::new(approval)
    }
}

/// Decides payments that a policy escalated.
///
/// The payment tools supply an approver that decides through
/// [`adk_core::confirm_tool_call`] — call-ID decisions, fingerprint approvals, the
/// approval store, and the confirmation handler — so an escalation reaches the same
/// approval flow as any tool that requires confirmation, and a held escalation resumes
/// once an approver decides it.
#[async_trait]
pub trait PaymentApprover: Send + Sync {
    /// Approves, refuses, or defers `record`, which the policies escalated with `findings`.
    async fn approve(
        &self,
        record: &TransactionRecord,
        findings: &[PaymentPolicyFinding],
    ) -> Result<PaymentApproval>;

    /// Decides `record` as [`approve`](Self::approve) does, and may substitute arguments
    /// or add a message (spec 3.1 §10 C3). The default wraps `approve`.
    async fn decide(
        &self,
        record: &TransactionRecord,
        findings: &[PaymentPolicyFinding],
    ) -> Result<PaymentDecision> {
        Ok(PaymentDecision::new(self.approve(record, findings).await?))
    }
}

/// Who is paying, and how escalations are decided, for one governed operation.
///
/// # Example
///
/// ```
/// use adk_payments::kernel::PaymentCaller;
///
/// let caller = PaymentCaller::new().with_org("store").with_agent("shopper");
/// # let _ = caller;
/// ```
#[derive(Clone, Default)]
pub struct PaymentCaller {
    org: Option<String>,
    agent: Option<String>,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
    approver: Option<Arc<dyn PaymentApprover>>,
    audit: Option<(AuditTrail, AuditActor)>,
}

impl PaymentCaller {
    /// Creates a caller with no attribution, no ledger, and no approver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attributes spend to `org`.
    #[must_use]
    pub fn with_org(mut self, org: impl Into<String>) -> Self {
        self.org = Some(org.into());
        self
    }

    /// Attributes spend to `agent`.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Uses `ledger` for this call when the service has no ledger of its own.
    #[must_use]
    pub fn with_spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend_ledger = Some(ledger);
        self
    }

    /// Asks `approver` about escalated payments. Without one, an escalation is refused
    /// with [`PAYMENT_APPROVAL_REQUIRED_CODE`].
    #[must_use]
    pub fn with_approver(mut self, approver: Arc<dyn PaymentApprover>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Records every governed decision for this caller in `trail`, naming `actor` as
    /// acting, ahead of and alongside any [`PaymentAuditor`] of the service.
    ///
    /// An allowed payment whose record cannot be written does not run when the trail is
    /// in [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block). The
    /// payment tools set this from the run's `RunConfig::audit`.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
    /// use adk_payments::kernel::PaymentCaller;
    ///
    /// let trail = AuditTrail::new(Arc::new(InMemoryAuditRecorder::new()));
    /// let actor = AuditActor::for_invocation("store", "shopper", "alice", "inv-1");
    /// let caller = PaymentCaller::new().with_audit(trail, actor);
    /// # let _ = caller;
    /// ```
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail, actor: AuditActor) -> Self {
        self.audit = Some((trail, actor));
        self
    }
}

const fn operation_action(operation: PaymentOperation) -> &'static str {
    match operation {
        PaymentOperation::CreateCheckout => "payment.create_checkout",
        PaymentOperation::CompleteCheckout => "payment.complete_checkout",
        PaymentOperation::ExecutePayment => "payment.execute_payment",
    }
}

/// The caller's audit record for `operation` on `record`: the cart's digest, the
/// merchant, the total, and the guardrails that spoke. Cart lines stay out of the record.
fn payment_draft(
    trail: &AuditTrail,
    actor: &AuditActor,
    operation: PaymentOperation,
    record: &TransactionRecord,
    protocol: &ProtocolDescriptor,
    findings: &[PaymentPolicyFinding],
) -> AuditRecordDraft {
    let cart = serde_json::to_value(&record.cart).unwrap_or(Value::Null);
    let guardrails: Vec<Value> =
        findings.iter().map(|finding| Value::String(finding.guardrail.clone())).collect();
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Payment, operation_action(operation))
        .actor(actor.clone())
        .args_digest(trail.digest(&cart))
        .policy_ref("payment_policy")
        .ext("transactionId", Value::String(record.transaction_id.as_str().to_string()))
        .ext("merchantId", Value::String(record.merchant_of_record.merchant_id.clone()))
        .ext("currency", Value::String(record.cart.total.currency.clone()))
        .ext("amountMinor", Value::from(record.cart.total.amount_minor))
        .ext("scale", Value::from(record.cart.total.scale))
        .ext("protocol", Value::String(protocol.name.clone()));
    if !guardrails.is_empty() {
        draft = draft.ext("guardrails", Value::Array(guardrails));
    }
    if let Some(identity) = &record.session_identity {
        draft = draft.ext("sessionId", Value::String(identity.session_id.to_string()));
    }
    draft
}

/// A policy refusal naming the guardrails that spoke, with `extra` metadata beside them.
fn policy_error(
    code: &'static str,
    summary: &str,
    findings: &[PaymentPolicyFinding],
    extra: &[(&str, Value)],
) -> AdkError {
    let reasons: Vec<String> = findings
        .iter()
        .map(|finding| format!("{}: {}", finding.guardrail, finding.reason))
        .collect();
    let mut details = ErrorDetails::default();
    details.metadata.insert("findings".to_string(), reasons.clone().into());
    for (key, value) in extra {
        details.metadata.insert((*key).to_string(), value.clone());
    }
    AdkError::new(
        ErrorComponent::Guardrail,
        ErrorCategory::Forbidden,
        code,
        format!("{summary}: {}", reasons.join("; ")),
    )
    .with_details(details)
}

/// Adds [`SpendLimitGuardrail::when_configured`] unless `policies` already limit spend,
/// so a ledger on the caller or the service caps payments without further setup.
pub(crate) fn with_spend_limit(policies: PaymentPolicySet) -> PaymentPolicySet {
    if policies.contains(SpendLimitGuardrail::new().name()) {
        policies
    } else {
        policies.with(SpendLimitGuardrail::when_configured())
    }
}

/// How a governed operation's spend holds settle once it returns.
pub(crate) enum Settlement {
    /// Commit every hold, at this amount when known.
    Commit(Option<u64>),
    /// Release every hold: no money moved.
    #[cfg(feature = "ap2")]
    Release,
}

/// Payment policies, spend ledger, and audit trail shared by every governed payment path:
/// [`GovernedCheckoutService`] for checkouts and the AP2 adapter for payment mandates.
#[derive(Clone)]
pub(crate) struct PaymentGovernor {
    policies: PaymentPolicySet,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
    auditor: Option<PaymentAuditor>,
}

impl PaymentGovernor {
    pub(crate) fn new(policies: PaymentPolicySet) -> Self {
        Self { policies, spend_ledger: None, auditor: None }
    }

    pub(crate) fn policies(&self) -> &PaymentPolicySet {
        &self.policies
    }

    #[cfg(feature = "ap2")]
    pub(crate) fn set_policies(&mut self, policies: PaymentPolicySet) {
        self.policies = policies;
    }

    pub(crate) fn set_spend_ledger(&mut self, ledger: Arc<dyn SpendLedger>) {
        self.spend_ledger = Some(ledger);
    }

    pub(crate) fn set_auditor(&mut self, auditor: PaymentAuditor) {
        self.auditor = Some(auditor);
    }

    /// Runs `run` only after the policies allow `operation` on `record` for `caller` and,
    /// when an auditor is configured, the decision is recorded.
    ///
    /// Spend holds the policies took are settled by `settle` when `run` succeeds and
    /// released when it fails or never starts.
    pub(crate) async fn govern<T, F, Fut, S>(
        &self,
        operation: PaymentOperation,
        record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
        caller: &PaymentCaller,
        run: F,
        settle: S,
    ) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
        S: FnOnce(&T) -> Settlement,
    {
        let mut context = PaymentPolicyContext::new(operation);
        if let Some(ledger) = self.spend_ledger.clone().or_else(|| caller.spend_ledger.clone()) {
            context = context.with_spend_ledger(ledger);
        }
        if let Some(org) = &caller.org {
            context = context.with_org(org);
        }
        if let Some(agent) = &caller.agent {
            context = context.with_agent(agent);
        }

        let caller_record = |decision: AuditDecision, findings: &[PaymentPolicyFinding]| {
            caller.audit.as_ref().map(|(trail, actor)| {
                let draft = payment_draft(trail, actor, operation, record, protocol, findings)
                    .decision(decision);
                let draft = if matches!(decision, AuditDecision::Allowed | AuditDecision::Approved)
                {
                    draft
                } else {
                    draft.outcome(AuditRecordOutcome::NotRun)
                };
                (trail.clone(), draft)
            })
        };
        let escalated = match self.policies.evaluate(record, protocol, &context).await {
            PaymentPolicyDecision::Allow => false,
            PaymentPolicyDecision::Deny { findings } => {
                // The payment is refused either way, so a failed write is logged by the trail.
                if let Some((trail, draft)) = caller_record(AuditDecision::Denied, &findings) {
                    let _ = trail.record(draft).await;
                }
                self.audit_refusal(operation, record, protocol, caller).await;
                return Err(policy_error(
                    PAYMENT_POLICY_DENIED_CODE,
                    "payment refused by policy",
                    &findings,
                    &[],
                ));
            }
            PaymentPolicyDecision::Escalate { findings } => {
                let decided = match &caller.approver {
                    Some(approver) => approver.decide(record, &findings).await,
                    None => Ok(PaymentDecision::new(PaymentApproval::Pending)),
                };
                let (approval, edited, message) = match decided {
                    Ok(decision) => (Ok(decision.approval), decision.edited_args, decision.message),
                    Err(error) => (Err(error), None, None),
                };
                // What the approver said and substituted travels with the decision's record.
                let approver_exts = |draft: AuditRecordDraft, trail: &AuditTrail| {
                    let draft = match &message {
                        Some(message) => {
                            draft.ext("approverMessage", Value::String(message.clone()))
                        }
                        None => draft,
                    };
                    match &edited {
                        Some(edited) => {
                            draft.ext("editedArgsDigest", Value::String(trail.digest(edited)))
                        }
                        None => draft,
                    }
                };
                let message_ext = || {
                    message
                        .as_ref()
                        .map(|message| ("approverMessage", Value::String(message.clone())))
                        .into_iter()
                        .collect::<Vec<_>>()
                };
                let refused_as = match (&approval, &edited) {
                    (Ok(PaymentApproval::Denied), _) => AuditDecision::Rejected,
                    (Ok(PaymentApproval::Pending), _) => AuditDecision::ApprovalRequired,
                    // Approved, but not as proposed: the caller re-issues the operation.
                    (Ok(PaymentApproval::Approved), Some(_)) => AuditDecision::Approved,
                    (Ok(PaymentApproval::Approved), None) | (Err(_), _) => AuditDecision::Denied,
                };
                let refusal = match (approval, &edited) {
                    (Ok(PaymentApproval::Approved), None) => None,
                    (Ok(PaymentApproval::Approved), Some(edited)) => {
                        let mut extra = message_ext();
                        extra.push(("editedArgs", edited.clone()));
                        Some(policy_error(
                            PAYMENT_APPROVAL_EDITED_CODE,
                            "payment approved with edited arguments; re-issue it with them",
                            &findings,
                            &extra,
                        ))
                    }
                    (Ok(PaymentApproval::Denied), _) => {
                        let summary = match &message {
                            Some(message) => format!("payment approval was denied ({message})"),
                            None => "payment approval was denied".to_string(),
                        };
                        Some(policy_error(
                            PAYMENT_APPROVAL_DENIED_CODE,
                            &summary,
                            &findings,
                            &message_ext(),
                        ))
                    }
                    (Ok(PaymentApproval::Pending), _) => Some(policy_error(
                        PAYMENT_APPROVAL_REQUIRED_CODE,
                        "payment requires approval",
                        &findings,
                        &[],
                    )),
                    (Err(error), _) => Some(error),
                };
                if let Some(error) = refusal {
                    context.release_holds().await;
                    if let Some((trail, draft)) = caller_record(refused_as, &findings) {
                        // An approval with edited arguments is recorded as approved, and as
                        // not run, since the operation as proposed never runs.
                        let draft =
                            approver_exts(draft, &trail).outcome(AuditRecordOutcome::NotRun);
                        let _ = trail.record(draft).await;
                    }
                    // An approval with edited arguments refuses nothing: the operation runs
                    // when the caller re-issues it, and that run is audited.
                    if error.code != PAYMENT_APPROVAL_EDITED_CODE {
                        self.audit_refusal(operation, record, protocol, caller).await;
                    }
                    return Err(error);
                }
                if let Some((trail, draft)) = caller_record(AuditDecision::Approved, &findings)
                    && let Err(error) = trail.record(approver_exts(draft, &trail)).await
                {
                    context.release_holds().await;
                    return Err(error);
                }
                true
            }
        };
        if !escalated
            && let Some((trail, draft)) = caller_record(AuditDecision::Allowed, &[])
            && let Err(error) = trail.record(draft).await
        {
            context.release_holds().await;
            return Err(error);
        }

        // An unrecorded decision must not move money: the audit write precedes the call.
        if let Some(auditor) = &self.auditor {
            let request = audit_request(record, caller);
            if let Err(error) = auditor
                .record_operation(
                    &request,
                    record,
                    protocol,
                    audit_operation(operation),
                    AuditOutcome::Allowed,
                    escalated,
                )
                .await
            {
                context.release_holds().await;
                return Err(error.into());
            }
        }

        let result = run().await;
        // The operation has already happened, so the outcome record cannot refuse it.
        if let Some((trail, actor)) = &caller.audit {
            let outcome = if result.is_ok() {
                AuditRecordOutcome::Succeeded
            } else {
                AuditRecordOutcome::Failed
            };
            let draft =
                payment_draft(trail, actor, operation, record, protocol, &[]).outcome(outcome);
            if let Err(error) = trail.recorder().record(draft).await {
                tracing::error!(error = %error, "payment outcome could not be audited");
            }
        }
        match result {
            Ok(result) => {
                match settle(&result) {
                    Settlement::Commit(actual) => context.commit_holds(actual).await,
                    #[cfg(feature = "ap2")]
                    Settlement::Release => context.release_holds().await,
                }
                Ok(result)
            }
            Err(error) => {
                context.release_holds().await;
                Err(error)
            }
        }
    }

    /// Records a refused payment. The payment is already refused, so a failed write is
    /// logged rather than returned.
    async fn audit_refusal(
        &self,
        operation: PaymentOperation,
        record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
        caller: &PaymentCaller,
    ) {
        let Some(auditor) = &self.auditor else {
            return;
        };
        let request = audit_request(record, caller);
        if let Err(error) = auditor
            .record_operation(
                &request,
                record,
                protocol,
                audit_operation(operation),
                AuditOutcome::Denied,
                false,
            )
            .await
        {
            tracing::warn!(error = %error, "failed to record a refused payment");
        }
    }
}

const fn audit_operation(operation: PaymentOperation) -> AuditPaymentOperation {
    match operation {
        PaymentOperation::CreateCheckout => AuditPaymentOperation::CreateCheckout,
        PaymentOperation::CompleteCheckout => AuditPaymentOperation::CompleteCheckout,
        PaymentOperation::ExecutePayment => AuditPaymentOperation::ExecutePayment,
    }
}

/// The audited identity: the session's user when known, else the paying agent.
fn audit_request(
    record: &TransactionRecord,
    caller: &PaymentCaller,
) -> AuthenticatedPaymentRequest {
    let user = record
        .session_identity
        .as_ref()
        .map(|identity| identity.user_id.to_string())
        .or_else(|| caller.agent.clone())
        .unwrap_or_else(|| record.initiated_by.actor_id.clone());
    let mut request = AuthenticatedPaymentRequest::new(user);
    if let Some(identity) = &record.session_identity {
        request = request.with_session_id(identity.session_id.to_string());
    }
    if let Some(org) = &caller.org {
        request = request.with_metadata("org", Value::String(org.clone()));
    }
    if let Some(agent) = &caller.agent {
        request = request.with_metadata("agent", Value::String(agent.clone()));
    }
    request
}

/// A [`MerchantCheckoutService`] that evaluates a [`PaymentPolicySet`] before every
/// checkout creation and completion.
///
/// | Policy outcome | Effect |
/// |----------------|--------|
/// | Allow | The operation runs; spend holds are committed on success and released on failure |
/// | Escalate | The caller's [`PaymentApprover`] decides; without one the call fails with [`PAYMENT_APPROVAL_REQUIRED_CODE`] |
/// | Deny | The call fails with [`PAYMENT_POLICY_DENIED_CODE`]; the backend is not reached |
///
/// Completion is evaluated against the stored checkout, so a cart changed after creation
/// is judged as it will be paid. A checkout the backend cannot return is refused.
/// Updates, lookups, cancellations, and order updates pass through. With an auditor
/// ([`with_auditor`](Self::with_auditor)), every allowed operation is recorded before the
/// backend is reached, and an operation whose record cannot be written is refused.
///
/// Wrap a backend with this service before handing it to the ACP adapter so protocol
/// traffic is governed too; the payment tools wrap theirs automatically, and the AP2
/// adapter governs its own checkouts and payment mandates.
///
/// # Example
///
/// ```rust,ignore
/// use adk_payments::guardrail::{AmountThresholdGuardrail, MerchantAllowlistGuardrail, PaymentPolicySet};
/// use adk_payments::kernel::GovernedCheckoutService;
/// use std::sync::Arc;
///
/// let governed = GovernedCheckoutService::new(
///     backend,
///     PaymentPolicySet::new()
///         .with(AmountThresholdGuardrail::new(Some(5_000), Some(10_000)).with_currency("USD", 2))
///         .with(MerchantAllowlistGuardrail::new(["merchant-1"])),
/// );
/// let service: Arc<dyn adk_payments::kernel::MerchantCheckoutService> = Arc::new(governed);
/// ```
pub struct GovernedCheckoutService {
    inner: Arc<dyn MerchantCheckoutService>,
    governor: PaymentGovernor,
}

impl GovernedCheckoutService {
    /// Governs `inner` with `policies`.
    #[must_use]
    pub fn new(inner: Arc<dyn MerchantCheckoutService>, policies: PaymentPolicySet) -> Self {
        Self { inner, governor: PaymentGovernor::new(policies) }
    }

    /// Gives policies `ledger`, ahead of any ledger the caller supplies.
    #[must_use]
    pub fn with_spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.governor.set_spend_ledger(ledger);
        self
    }

    /// Records every governed decision with `auditor`, refusing an operation whose
    /// allowed decision cannot be recorded.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_auth::FileAuditSink;
    /// use adk_payments::auth::PaymentAuditor;
    ///
    /// let governed = governed.with_auditor(PaymentAuditor::new(Arc::new(
    ///     FileAuditSink::new("payments-audit.jsonl")?,
    /// )));
    /// ```
    #[must_use]
    pub fn with_auditor(mut self, auditor: PaymentAuditor) -> Self {
        self.governor.set_auditor(auditor);
        self
    }

    /// Returns the policies this service enforces.
    #[must_use]
    pub fn policies(&self) -> &PaymentPolicySet {
        self.governor.policies()
    }

    /// Creates a checkout after the policies allow it for `caller`.
    ///
    /// # Errors
    ///
    /// Returns [`PAYMENT_POLICY_DENIED_CODE`], [`PAYMENT_APPROVAL_REQUIRED_CODE`], or
    /// [`PAYMENT_APPROVAL_DENIED_CODE`] when the policies stop the checkout,
    /// `payments.auth.audit_failed` when the decision cannot be recorded, or the
    /// backend's error.
    pub async fn create_checkout_as(
        &self,
        command: CreateCheckoutCommand,
        caller: &PaymentCaller,
    ) -> Result<TransactionRecord> {
        let context = &command.context;
        let mut record = TransactionRecord::new(
            context.transaction_id.clone(),
            context.actor.clone(),
            context.merchant_of_record.clone(),
            context.mode,
            command.cart.clone(),
            Utc::now(),
        );
        record.session_identity = context.session_identity.clone();
        record.payment_processor = context.payment_processor.clone();
        let protocol = context.protocol.clone();
        self.governor
            .govern(
                PaymentOperation::CreateCheckout,
                &record,
                &protocol,
                caller,
                || self.inner.create_checkout(command),
                checkout_settlement,
            )
            .await
    }

    /// Completes a checkout after the policies allow the stored checkout for `caller`.
    ///
    /// # Errors
    ///
    /// Returns `payments.policy.checkout_not_found` when the backend has no such
    /// checkout, a policy error code when the policies stop the payment,
    /// `payments.auth.audit_failed` when the decision cannot be recorded, or the
    /// backend's error.
    pub async fn complete_checkout_as(
        &self,
        command: CompleteCheckoutCommand,
        caller: &PaymentCaller,
    ) -> Result<TransactionRecord> {
        let lookup = TransactionLookup {
            transaction_id: command.context.transaction_id.clone(),
            session_identity: command.context.session_identity.clone(),
        };
        let Some(record) = self.inner.get_checkout(lookup).await? else {
            return Err(AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::NotFound,
                "payments.policy.checkout_not_found",
                format!(
                    "checkout `{}` was not found, so its payment policies cannot be evaluated",
                    command.context.transaction_id.as_str()
                ),
            ));
        };
        let protocol = command.context.protocol.clone();
        self.governor
            .govern(
                PaymentOperation::CompleteCheckout,
                &record,
                &protocol,
                caller,
                || self.inner.complete_checkout(command),
                checkout_settlement,
            )
            .await
    }
}

/// A checkout that returned commits its holds at the returned total.
pub(crate) fn checkout_settlement(record: &TransactionRecord) -> Settlement {
    Settlement::Commit(usd_micro_amount(&record.cart.total).ok())
}

/// Builds the caller a protocol adapter supplies: the session's app and the acting actor.
pub(crate) fn caller_from(context: &crate::kernel::commands::CommerceContext) -> PaymentCaller {
    let mut caller = PaymentCaller::new().with_agent(&context.actor.actor_id);
    if let Some(identity) = &context.session_identity {
        caller = caller.with_org(identity.app_name.as_str());
    }
    caller
}

#[async_trait]
impl MerchantCheckoutService for GovernedCheckoutService {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        let caller = caller_from(&command.context);
        self.create_checkout_as(command, &caller).await
    }

    async fn update_checkout(&self, command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        self.inner.update_checkout(command).await
    }

    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.inner.get_checkout(lookup).await
    }

    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        let caller = caller_from(&command.context);
        self.complete_checkout_as(command, &caller).await
    }

    async fn cancel_checkout(&self, command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        self.inner.cancel_checkout(command).await
    }

    async fn apply_order_update(&self, command: OrderUpdateCommand) -> Result<TransactionRecord> {
        self.inner.apply_order_update(command).await
    }
}
