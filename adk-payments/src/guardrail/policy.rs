use std::sync::{Arc, Mutex};

use adk_core::{ReservationId, SpendLedger};
use adk_guardrail::Severity;
use async_trait::async_trait;

use crate::domain::{ProtocolDescriptor, TransactionRecord};

/// One concrete policy finding emitted by a payment guardrail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentPolicyFinding {
    /// The guardrail that produced the finding.
    pub guardrail: String,
    /// Human-readable explanation of the policy outcome.
    pub reason: String,
    /// Severity of the finding.
    pub severity: Severity,
}

impl PaymentPolicyFinding {
    /// Creates a new policy finding.
    #[must_use]
    pub fn new(
        guardrail: impl Into<String>,
        reason: impl Into<String>,
        severity: Severity,
    ) -> Self {
        Self { guardrail: guardrail.into(), reason: reason.into(), severity }
    }
}

/// Outcome of evaluating a payment policy or policy set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentPolicyDecision {
    /// The payment passed every configured guardrail.
    Allow,
    /// The payment may continue only after explicit approval.
    ///
    /// The payment tools route this outcome through the tool confirmation flow; see
    /// [`GovernedCheckoutService`](crate::kernel::GovernedCheckoutService).
    Escalate { findings: Vec<PaymentPolicyFinding> },
    /// The payment must not continue under the current policy.
    Deny { findings: Vec<PaymentPolicyFinding> },
}

impl PaymentPolicyDecision {
    /// Returns an allow decision.
    #[must_use]
    pub const fn allow() -> Self {
        Self::Allow
    }

    /// Returns an escalation decision with one or more findings.
    #[must_use]
    pub fn escalate(findings: Vec<PaymentPolicyFinding>) -> Self {
        Self::Escalate { findings }
    }

    /// Returns a denial decision with one or more findings.
    #[must_use]
    pub fn deny(findings: Vec<PaymentPolicyFinding>) -> Self {
        Self::Deny { findings }
    }

    /// Returns `true` when the payment may proceed without intervention.
    #[must_use]
    pub const fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Returns `true` when the payment requires explicit human confirmation.
    #[must_use]
    pub const fn is_escalate(&self) -> bool {
        matches!(self, Self::Escalate { .. })
    }

    /// Returns `true` when the payment is denied.
    #[must_use]
    pub const fn is_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// Returns the policy findings attached to the decision.
    #[must_use]
    pub fn findings(&self) -> &[PaymentPolicyFinding] {
        match self {
            Self::Allow => &[],
            Self::Escalate { findings } | Self::Deny { findings } => findings.as_slice(),
        }
    }

    /// Returns the highest-severity finding attached to the decision.
    #[must_use]
    pub fn highest_severity(&self) -> Option<Severity> {
        self.findings()
            .iter()
            .map(|finding| finding.severity)
            .max_by_key(|severity| severity_rank(*severity))
    }
}

/// The checkout operation a payment policy is evaluated for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaymentOperation {
    /// A checkout session is being created.
    CreateCheckout,
    /// A checkout is being completed, which moves money.
    CompleteCheckout,
}

/// Budget a policy reserved while evaluating one operation.
///
/// The enforcer commits every hold when the operation succeeds and releases them when it
/// is denied, left awaiting approval, or fails.
#[derive(Debug, Clone)]
pub struct SpendHold {
    /// Ledger the reservation was taken in.
    pub ledger: Arc<dyn SpendLedger>,
    /// The reservation to commit or release.
    pub reservation: ReservationId,
    /// Amount reserved, committed when the completed transaction's total cannot be read.
    pub amount_micro_usd: u64,
}

/// What a payment policy sees beside the transaction: the operation, who is paying, and
/// the spend ledger.
///
/// # Example
///
/// ```rust
/// use adk_core::InMemorySpendLedger;
/// use adk_payments::guardrail::{PaymentOperation, PaymentPolicyContext};
/// use std::sync::Arc;
///
/// let context = PaymentPolicyContext::new(PaymentOperation::CompleteCheckout)
///     .with_org("store")
///     .with_agent("shopper")
///     .with_spend_ledger(Arc::new(InMemorySpendLedger::default()));
/// assert_eq!(context.org(), Some("store"));
/// ```
#[derive(Debug)]
pub struct PaymentPolicyContext {
    operation: PaymentOperation,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
    org: Option<String>,
    agent: Option<String>,
    holds: Mutex<Vec<SpendHold>>,
}

impl PaymentPolicyContext {
    /// Creates a context for `operation` with no ledger and no attribution.
    #[must_use]
    pub fn new(operation: PaymentOperation) -> Self {
        Self {
            operation,
            spend_ledger: None,
            org: None,
            agent: None,
            holds: Mutex::new(Vec::new()),
        }
    }

    /// Gives policies access to `ledger`.
    #[must_use]
    pub fn with_spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend_ledger = Some(ledger);
        self
    }

    /// Attributes the payment to `org`, such as the calling application.
    #[must_use]
    pub fn with_org(mut self, org: impl Into<String>) -> Self {
        self.org = Some(org.into());
        self
    }

    /// Attributes the payment to `agent`.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Returns the operation being evaluated.
    #[must_use]
    pub fn operation(&self) -> PaymentOperation {
        self.operation
    }

    /// Returns the spend ledger, when one is configured.
    #[must_use]
    pub fn spend_ledger(&self) -> Option<&Arc<dyn SpendLedger>> {
        self.spend_ledger.as_ref()
    }

    /// Returns the organization the payment is attributed to.
    #[must_use]
    pub fn org(&self) -> Option<&str> {
        self.org.as_deref()
    }

    /// Returns the agent the payment is attributed to.
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Records budget a policy reserved, for the enforcer to commit or release.
    pub fn hold(&self, hold: SpendHold) {
        self.holds.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(hold);
    }

    fn take_holds(&self) -> Vec<SpendHold> {
        std::mem::take(&mut *self.holds.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Commits every hold, at `actual_micro_usd` when known and the reserved amount
    /// otherwise. A failed commit is logged: the payment already happened.
    pub async fn commit_holds(&self, actual_micro_usd: Option<u64>) {
        for hold in self.take_holds() {
            let amount = actual_micro_usd.unwrap_or(hold.amount_micro_usd);
            if let Err(error) = hold.ledger.commit(hold.reservation, amount).await {
                tracing::error!(error = %error, spend.amount_micro_usd = amount, "failed to commit payment spend");
            }
        }
    }

    /// Releases every hold. A failed release is logged; the hold expires by its TTL.
    pub async fn release_holds(&self) {
        for hold in self.take_holds() {
            if let Err(error) = hold.ledger.release(hold.reservation).await {
                tracing::warn!(error = %error, "failed to release payment spend reservation");
            }
        }
    }
}

/// Trait implemented by payment-specific policy guardrails.
///
/// Evaluation is asynchronous so a policy can consult the
/// [`spend_ledger`](PaymentPolicyContext::spend_ledger) or another service.
#[async_trait]
pub trait PaymentPolicyGuardrail: Send + Sync {
    /// Stable name of the guardrail.
    fn name(&self) -> &str;

    /// Evaluates one canonical transaction under a specific protocol surface.
    async fn evaluate(
        &self,
        record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
        context: &PaymentPolicyContext,
    ) -> PaymentPolicyDecision;
}

/// Ordered collection of payment policy guardrails.
pub struct PaymentPolicySet {
    guardrails: Vec<Arc<dyn PaymentPolicyGuardrail>>,
}

impl PaymentPolicySet {
    /// Creates an empty payment policy set.
    #[must_use]
    pub fn new() -> Self {
        Self { guardrails: Vec::new() }
    }

    /// Adds one concrete guardrail to the set.
    #[must_use]
    pub fn with(mut self, guardrail: impl PaymentPolicyGuardrail + 'static) -> Self {
        self.guardrails.push(Arc::new(guardrail));
        self
    }

    /// Adds one shared guardrail instance to the set.
    #[must_use]
    pub fn with_arc(mut self, guardrail: Arc<dyn PaymentPolicyGuardrail>) -> Self {
        self.guardrails.push(guardrail);
        self
    }

    /// Returns all configured payment policy guardrails.
    #[must_use]
    pub fn guardrails(&self) -> &[Arc<dyn PaymentPolicyGuardrail>] {
        &self.guardrails
    }

    /// Returns `true` when a guardrail named `name` is configured.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.guardrails.iter().any(|guardrail| guardrail.name() == name)
    }

    /// Evaluates all configured guardrails and returns the strongest outcome.
    ///
    /// A denial releases any budget the guardrails reserved in `context`. After an allow
    /// or an escalation, the caller commits or releases the holds once the operation ends.
    pub async fn evaluate(
        &self,
        record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
        context: &PaymentPolicyContext,
    ) -> PaymentPolicyDecision {
        let mut denied = Vec::new();
        let mut escalated = Vec::new();

        for guardrail in &self.guardrails {
            match guardrail.evaluate(record, protocol, context).await {
                PaymentPolicyDecision::Allow => {}
                PaymentPolicyDecision::Escalate { findings } => escalated.extend(findings),
                PaymentPolicyDecision::Deny { findings } => denied.extend(findings),
            }
        }

        sort_findings(&mut denied);
        sort_findings(&mut escalated);

        if !denied.is_empty() {
            context.release_holds().await;
            PaymentPolicyDecision::deny(denied)
        } else if !escalated.is_empty() {
            PaymentPolicyDecision::escalate(escalated)
        } else {
            PaymentPolicyDecision::allow()
        }
    }
}

impl Default for PaymentPolicySet {
    fn default() -> Self {
        Self::new()
    }
}

fn sort_findings(findings: &mut [PaymentPolicyFinding]) {
    findings.sort_by(|left, right| {
        left.guardrail
            .cmp(&right.guardrail)
            .then(severity_rank(right.severity).cmp(&severity_rank(left.severity)))
            .then(left.reason.cmp(&right.reason))
    });
}

const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Low => 0,
        Severity::Medium => 1,
        Severity::High => 2,
        Severity::Critical => 3,
    }
}

#[cfg(test)]
mod tests {
    use adk_guardrail::Severity;
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
        ProtocolExtensions, TransactionId,
    };

    struct StaticDecisionGuardrail {
        name: &'static str,
        decision: PaymentPolicyDecision,
    }

    #[async_trait]
    impl PaymentPolicyGuardrail for StaticDecisionGuardrail {
        fn name(&self) -> &str {
            self.name
        }

        async fn evaluate(
            &self,
            _record: &TransactionRecord,
            _protocol: &ProtocolDescriptor,
            _context: &PaymentPolicyContext,
        ) -> PaymentPolicyDecision {
            self.decision.clone()
        }
    }

    fn sample_record() -> TransactionRecord {
        TransactionRecord::new(
            TransactionId::from("tx-policy"),
            CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            MerchantRef {
                merchant_id: "merchant-1".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: None,
                country_code: Some("US".to_string()),
                website: Some("https://merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            CommerceMode::HumanPresent,
            Cart {
                cart_id: Some("cart-1".to_string()),
                lines: vec![CartLine {
                    line_id: "line-1".to_string(),
                    merchant_sku: Some("sku-1".to_string()),
                    title: "Widget".to_string(),
                    quantity: 1,
                    unit_price: Money::new("USD", 1_500, 2),
                    total_price: Money::new("USD", 1_500, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("USD", 1_500, 2)),
                adjustments: Vec::new(),
                total: Money::new("USD", 1_500, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 0, 0).unwrap(),
        )
    }

    #[tokio::test]
    async fn policy_set_prefers_denials_over_escalations() {
        let set = PaymentPolicySet::new()
            .with(StaticDecisionGuardrail {
                name: "amount_threshold",
                decision: PaymentPolicyDecision::escalate(vec![PaymentPolicyFinding::new(
                    "amount_threshold",
                    "needs approval",
                    Severity::Medium,
                )]),
            })
            .with(StaticDecisionGuardrail {
                name: "merchant_allowlist",
                decision: PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                    "merchant_allowlist",
                    "merchant is blocked",
                    Severity::High,
                )]),
            });

        let decision = set
            .evaluate(
                &sample_record(),
                &ProtocolDescriptor::acp("2026-01-30"),
                &PaymentPolicyContext::new(PaymentOperation::CreateCheckout),
            )
            .await;

        assert!(decision.is_deny());
        assert_eq!(decision.findings().len(), 1);
        assert_eq!(decision.findings()[0].guardrail, "merchant_allowlist");
        assert_eq!(decision.highest_severity(), Some(Severity::High));
    }
}
