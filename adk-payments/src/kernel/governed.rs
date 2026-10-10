use std::future::Future;
use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails, Result, SpendLedger};
use async_trait::async_trait;
use chrono::Utc;

use crate::domain::{ProtocolDescriptor, TransactionRecord};
use crate::guardrail::{
    PaymentOperation, PaymentPolicyContext, PaymentPolicyDecision, PaymentPolicyFinding,
    PaymentPolicySet, usd_micro_amount,
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

/// Decides payments that a policy escalated.
///
/// The payment tools supply an approver that consults the run's tool confirmation
/// decisions and handler, so an escalation reaches the same approval flow as any tool
/// that requires confirmation.
#[async_trait]
pub trait PaymentApprover: Send + Sync {
    /// Approves, refuses, or defers `record`, which the policies escalated with `findings`.
    async fn approve(
        &self,
        record: &TransactionRecord,
        findings: &[PaymentPolicyFinding],
    ) -> Result<PaymentApproval>;
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
}

fn policy_error(code: &'static str, summary: &str, findings: &[PaymentPolicyFinding]) -> AdkError {
    let reasons: Vec<String> = findings
        .iter()
        .map(|finding| format!("{}: {}", finding.guardrail, finding.reason))
        .collect();
    let mut details = ErrorDetails::default();
    details.metadata.insert("findings".to_string(), reasons.clone().into());
    AdkError::new(
        ErrorComponent::Guardrail,
        ErrorCategory::Forbidden,
        code,
        format!("{summary}: {}", reasons.join("; ")),
    )
    .with_details(details)
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
/// Updates, lookups, cancellations, and order updates pass through.
///
/// Wrap a backend with this service before handing it to the ACP or AP2 adapters so
/// protocol traffic is governed too; the payment tools wrap theirs automatically.
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
    policies: PaymentPolicySet,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
}

impl GovernedCheckoutService {
    /// Governs `inner` with `policies`.
    #[must_use]
    pub fn new(inner: Arc<dyn MerchantCheckoutService>, policies: PaymentPolicySet) -> Self {
        Self { inner, policies, spend_ledger: None }
    }

    /// Gives policies `ledger`, ahead of any ledger the caller supplies.
    #[must_use]
    pub fn with_spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend_ledger = Some(ledger);
        self
    }

    /// Returns the policies this service enforces.
    #[must_use]
    pub fn policies(&self) -> &PaymentPolicySet {
        &self.policies
    }

    /// Creates a checkout after the policies allow it for `caller`.
    ///
    /// # Errors
    ///
    /// Returns [`PAYMENT_POLICY_DENIED_CODE`], [`PAYMENT_APPROVAL_REQUIRED_CODE`], or
    /// [`PAYMENT_APPROVAL_DENIED_CODE`] when the policies stop the checkout, or the
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
        self.govern(PaymentOperation::CreateCheckout, &record, &protocol, caller, || {
            self.inner.create_checkout(command)
        })
        .await
    }

    /// Completes a checkout after the policies allow the stored checkout for `caller`.
    ///
    /// # Errors
    ///
    /// Returns `payments.policy.checkout_not_found` when the backend has no such
    /// checkout, a policy error code when the policies stop the payment, or the
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
        self.govern(PaymentOperation::CompleteCheckout, &record, &protocol, caller, || {
            self.inner.complete_checkout(command)
        })
        .await
    }

    async fn govern<F, Fut>(
        &self,
        operation: PaymentOperation,
        record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
        caller: &PaymentCaller,
        run: F,
    ) -> Result<TransactionRecord>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<TransactionRecord>>,
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

        match self.policies.evaluate(record, protocol, &context).await {
            PaymentPolicyDecision::Allow => {}
            PaymentPolicyDecision::Deny { findings } => {
                return Err(policy_error(
                    PAYMENT_POLICY_DENIED_CODE,
                    "payment refused by policy",
                    &findings,
                ));
            }
            PaymentPolicyDecision::Escalate { findings } => {
                let approval = match &caller.approver {
                    Some(approver) => approver.approve(record, &findings).await,
                    None => Ok(PaymentApproval::Pending),
                };
                let refusal = match approval {
                    Ok(PaymentApproval::Approved) => None,
                    Ok(PaymentApproval::Denied) => Some(policy_error(
                        PAYMENT_APPROVAL_DENIED_CODE,
                        "payment approval was denied",
                        &findings,
                    )),
                    Ok(PaymentApproval::Pending) => Some(policy_error(
                        PAYMENT_APPROVAL_REQUIRED_CODE,
                        "payment requires approval",
                        &findings,
                    )),
                    Err(error) => Some(error),
                };
                if let Some(error) = refusal {
                    context.release_holds().await;
                    return Err(error);
                }
            }
        }

        match run().await {
            Ok(result) => {
                context.commit_holds(usd_micro_amount(&result.cart.total).ok()).await;
                Ok(result)
            }
            Err(error) => {
                context.release_holds().await;
                Err(error)
            }
        }
    }
}

/// Builds the caller a protocol adapter supplies: the session's app and the acting actor.
fn caller_from(context: &crate::kernel::commands::CommerceContext) -> PaymentCaller {
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
