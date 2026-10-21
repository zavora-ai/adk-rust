//! The payments pack (spec 3.1 §7.16, feature `payments`).
//!
//! `adk-payments` already routes an escalated payment through the governed path and meters
//! it in the shared `SpendLedger`. The pack is wiring and classes: it registers the payment
//! tools as class [`ToolKind::Payment`](crate::ToolKind) — `Deny` in ReadOnly and Plan,
//! `RequireApproval` and non-waivable in Ask and Auto, never covered by a standing rule or
//! a `tool_approvals` entry (spec 3.1 §9.2 I38) — keeps every call under the run's
//! `RunBudget` and the payments guardrails (the mandate's limit as a hard stop), links the
//! journal receipt from the `harness.payment` audit record (§9.3), and names the mandate
//! and the remaining budget in every model request.
//!
//! The harness holds each payment for one approval before the tool runs, so the pack's
//! own guardrails never escalate: a review threshold inside the tool would ask the person a
//! second time. The mandate limit refuses, and the spend ledger caps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_auth::ScopeGuard;
use adk_core::{
    BudgetTracker, Content, InvocationContext, LlmRequest, ReadonlyContext, Result, SpendKey,
    SpendLedger, SpendPeriod, Tool, ToolContext, ToolEffect,
};
use adk_payments::domain::{Money, TransactionId};
use adk_payments::guardrail::{
    AmountThresholdGuardrail, MerchantAllowlistGuardrail, PaymentPolicySet, usd_micro_amount,
};
use adk_payments::journal::transaction_state_storage_key;
use adk_payments::kernel::{
    InterventionService, MerchantCheckoutService, PAYMENT_APPROVAL_DENIED_CODE,
    PAYMENT_APPROVAL_REQUIRED_CODE, PAYMENT_POLICY_DENIED_CODE, TransactionLookup,
    TransactionStore,
};
use adk_payments::tools::PaymentToolsetBuilder;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::HarnessAudit;
use crate::toolset::RequestObserver;

/// Names of the tools `adk-payments` publishes; the pack registers every one as class
/// `Payment` (spec 3.1 §7.16), whether or not the configured toolset declares it.
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::payments::PAYMENT_TOOL_NAMES.contains(&"payments_checkout_complete"));
/// ```
pub const PAYMENT_TOOL_NAMES: &[&str] = &[
    "payments_checkout_create",
    "payments_checkout_update",
    "payments_checkout_complete",
    "payments_checkout_cancel",
    "payments_status_lookup",
    "payments_intervention_continue",
    "payments_ap2_payment_submit",
];

/// Action of the audit record one payment attempt writes (spec 3.1 §9.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::payments::PAYMENT_AUDIT_ACTION, "harness.payment");
/// ```
pub const PAYMENT_AUDIT_ACTION: &str = "harness.payment";

/// Whether `name` is one of the payment tools.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::payments::is_payment_tool;
///
/// assert!(is_payment_tool("payments_ap2_payment_submit"));
/// assert!(!is_payment_tool("bash"));
/// ```
pub fn is_payment_tool(name: &str) -> bool {
    PAYMENT_TOOL_NAMES.contains(&name)
}

/// The AP2 mandate a harness buys under (spec 3.1 §7.16, §5 H8).
///
/// The prompt names it, every `harness.payment` record cites its `id`, and its `limit` is
/// the hard stop of the payments guardrails: a transaction whose total exceeds it, or is in
/// another currency, is refused before any money moves. `merchants`, when set, is the
/// merchant allowlist.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::payments::PaymentMandate;
/// use adk_payments::domain::Money;
///
/// let mandate = PaymentMandate::new("mandate-42", "domain registration", Money::new("USD", 2_000, 2))
///     .with_merchants(["domainmart"]);
/// assert_eq!(mandate.limit_text(), "20.00 USD");
/// assert_eq!(mandate.limit_micro_usd(), Some(20_000_000));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentMandate {
    /// The mandate's identifier, as the audit record cites it.
    pub id: String,
    /// What the mandate authorizes, in the user's words.
    pub description: String,
    /// The most one transaction may total.
    pub limit: Money,
    /// Merchant ids the mandate allows; empty allows any merchant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merchants: Vec<String>,
}

impl PaymentMandate {
    /// A mandate `id` for `description`, capped at `limit` per transaction.
    pub fn new(id: impl Into<String>, description: impl Into<String>, limit: Money) -> Self {
        Self { id: id.into(), description: description.into(), limit, merchants: Vec::new() }
    }

    /// Restricts the mandate to these merchant ids.
    #[must_use]
    pub fn with_merchants(
        mut self,
        merchants: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.merchants = merchants.into_iter().map(Into::into).collect();
        self
    }

    /// The limit as `"20.00 USD"`.
    pub fn limit_text(&self) -> String {
        format!("{} {}", self.limit.to_decimal_string(), self.limit.currency)
    }

    /// The limit in micro-USD, when the mandate is in USD.
    pub fn limit_micro_usd(&self) -> Option<u64> {
        usd_micro_amount(&self.limit).ok()
    }

    /// The payments guardrails the mandate stands for: its limit as the hard stop and,
    /// when set, its merchant allowlist. No review threshold: the harness already holds
    /// every payment for approval.
    pub fn policies(&self) -> PaymentPolicySet {
        let mut policies = PaymentPolicySet::new().with(
            AmountThresholdGuardrail::new(None, Some(self.limit.amount_minor))
                .with_currency(self.limit.currency.clone(), self.limit.scale),
        );
        if !self.merchants.is_empty() {
            policies = policies.with(MerchantAllowlistGuardrail::new(self.merchants.clone()));
        }
        policies
    }

    /// One line naming the mandate for the model.
    fn describe(&self) -> String {
        let merchants = if self.merchants.is_empty() {
            String::new()
        } else {
            format!(" at merchants {}", self.merchants.join(", "))
        };
        format!(
            "mandate {} ({}) allows at most {} per transaction{merchants}",
            self.id,
            self.description,
            self.limit_text()
        )
    }
}

/// The payments pack (spec 3.1 §7.16): the services a harness buys through, under one
/// [`PaymentMandate`].
///
/// `HarnessAgentBuilder::pack_payments` installs it: the tools of
/// `adk_payments::tools::PaymentToolsetBuilder` over `checkout` and `transactions`, scope
/// guarded, governed by the mandate's guardrails plus any
/// [`with_payment_policies`](Self::with_payment_policies), and reserving against the run's
/// spend ledger (the harness's by default).
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::{ScopeGuard, StaticScopeResolver};
/// use adk_harness::packs::payments::{PaymentMandate, PaymentsPack};
/// use adk_payments::auth::ALL_PAYMENT_SCOPES;
/// use adk_payments::domain::Money;
///
/// let mandate = PaymentMandate::new("mandate-42", "domain registration", Money::new("USD", 2_000, 2));
/// let pack = PaymentsPack::new(merchant.clone(), merchant, mandate)
///     .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(ALL_PAYMENT_SCOPES.to_vec())));
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(workspace)
///     .pack_payments(pack)
///     .build()?;
/// assert_eq!(harness.info().packs, vec!["payments".to_string()]);
/// ```
pub struct PaymentsPack {
    checkout: Arc<dyn MerchantCheckoutService>,
    transactions: Arc<dyn TransactionStore>,
    mandate: PaymentMandate,
    policies: PaymentPolicySet,
    scope_guard: Option<ScopeGuard>,
    intervention: Option<Arc<dyn InterventionService>>,
}

impl std::fmt::Debug for PaymentsPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaymentsPack").field("mandate", &self.mandate).finish_non_exhaustive()
    }
}

impl PaymentsPack {
    /// A pack buying through `checkout`, journaling in `transactions`, under `mandate`.
    pub fn new(
        checkout: Arc<dyn MerchantCheckoutService>,
        transactions: Arc<dyn TransactionStore>,
        mandate: PaymentMandate,
    ) -> Self {
        Self {
            checkout,
            transactions,
            mandate,
            policies: PaymentPolicySet::new(),
            scope_guard: None,
            intervention: None,
        }
    }

    /// Payment policies evaluated after the mandate's. A guardrail that escalates asks the
    /// person a second time, after the harness's own approval; prefer ones that deny.
    #[must_use]
    pub fn with_payment_policies(mut self, policies: PaymentPolicySet) -> Self {
        self.policies = policies;
        self
    }

    /// The guard that enforces each tool's declared scopes; by default the scopes come from
    /// the run's request context (`adk_auth::ContextScopeResolver`).
    #[must_use]
    pub fn with_scope_guard(mut self, guard: ScopeGuard) -> Self {
        self.scope_guard = Some(guard);
        self
    }

    /// Adds the `payments_intervention_continue` tool.
    #[must_use]
    pub fn with_intervention_service(mut self, service: Arc<dyn InterventionService>) -> Self {
        self.intervention = Some(service);
        self
    }

    /// The mandate.
    pub fn mandate(&self) -> &PaymentMandate {
        &self.mandate
    }

    /// Builds the governed tools and the request reminder for the coordinator `agent`.
    pub(crate) fn into_runtime(self, agent: &str) -> PaymentsRuntime {
        let mut policies = self.mandate.policies();
        for guardrail in self.policies.guardrails() {
            policies = policies.with_arc(guardrail.clone());
        }
        let mut builder = PaymentToolsetBuilder::new(self.checkout, self.transactions.clone())
            .with_payment_policies(policies);
        if let Some(guard) = self.scope_guard {
            builder = builder.with_scope_guard(guard);
        }
        if let Some(service) = self.intervention {
            builder = builder.with_intervention_service(service);
        }
        let mandate = Arc::new(self.mandate);
        let tools = builder
            .build()
            .tools()
            .into_iter()
            .map(|tool| {
                Arc::new(PaymentAuditedTool {
                    inner: tool,
                    mandate: mandate.clone(),
                    transactions: self.transactions.clone(),
                }) as Arc<dyn Tool>
            })
            .collect();
        let reminder = Arc::new(PaymentsReminder {
            mandate,
            agent: agent.to_string(),
            runs: Mutex::new(HashMap::new()),
        });
        PaymentsRuntime { tools, reminder }
    }
}

/// What the builder installs for a configured pack.
pub(crate) struct PaymentsRuntime {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) reminder: Arc<PaymentsReminder>,
}

/// Upper bound of remembered invocations; the map is cleared beyond it.
const TRACKED_RUNS_CAP: usize = 4096;

/// A run's budget tracker and spend ledger, as its `RunConfig` carries them.
#[derive(Clone)]
struct RunMeters {
    budget: Option<Arc<BudgetTracker>>,
    ledger: Option<Arc<dyn SpendLedger>>,
}

/// Names the mandate and the remaining budget in every coordinator request
/// (spec 3.1 §7.16).
///
/// The budget is the run's: what its `RunBudget` still allows, and what the run's spend
/// ledger has committed at the mandate's merchants — the key `SpendLimitGuardrail`
/// reserves payments under, which model calls never use. Appended at the end of the
/// request, so the cached prefix stays stable. Members cannot pay, so their requests carry
/// no reminder.
pub(crate) struct PaymentsReminder {
    mandate: Arc<PaymentMandate>,
    agent: String,
    runs: Mutex<HashMap<String, RunMeters>>,
}

impl PaymentsReminder {
    /// Records the run's budget tracker and spend ledger as its invocation starts.
    pub(crate) fn observe_invocation(&self, ctx: &dyn InvocationContext) {
        let config = ctx.run_config();
        let meters = RunMeters {
            budget: config.budget_tracker.clone(),
            ledger: config.spend_ledger.clone(),
        };
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        if runs.len() > TRACKED_RUNS_CAP {
            runs.clear();
        }
        runs.insert(ctx.invocation_id().to_string(), meters);
    }

    fn meters(&self, invocation_id: &str) -> Option<RunMeters> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(invocation_id).cloned()
    }

    /// What has been paid at the mandate's merchants, from the run's spend ledger.
    async fn paid(
        &self,
        ctx: &dyn ReadonlyContext,
        ledger: Option<&Arc<dyn SpendLedger>>,
    ) -> String {
        if self.mandate.merchants.is_empty() {
            return "paid so far: not tracked (the mandate names no merchant)".to_string();
        }
        let Some(ledger) = ledger else {
            return "paid so far: not tracked (the run has no spend ledger)".to_string();
        };
        let agent = ctx.acting_context().agent.id.to_string();
        let mut total: u64 = 0;
        for merchant in &self.mandate.merchants {
            let key = SpendKey::org(ctx.app_name())
                .with_agent(agent.clone())
                .with_vendor(merchant.clone());
            match ledger.spent(&key, SpendPeriod::Lifetime).await {
                Ok(spent) => total = total.saturating_add(spent),
                Err(error) => {
                    tracing::warn!(error = %error, "spend ledger unavailable for the payments reminder");
                    return "paid so far: unknown (the spend ledger is unavailable)".to_string();
                }
            }
        }
        format!("paid so far at these merchants: {}", usd_text(total))
    }
}

/// What a run's budget still allows, for the limits it sets.
fn run_budget_text(tracker: Option<&BudgetTracker>) -> String {
    let Some(tracker) = tracker else {
        return "run budget: no limits set".to_string();
    };
    let budget = tracker.budget();
    let usage = tracker.usage();
    let mut parts = Vec::new();
    if let Some(remaining) = tracker.tool_calls_remaining() {
        parts.push(format!("{remaining} tool calls"));
    }
    if let Some(max) = budget.max_model_calls {
        parts.push(format!("{} model calls", max.saturating_sub(usage.model_calls)));
    }
    if let Some(max) = budget.max_cost_micro_usd {
        parts.push(format!("{} of model cost", usd_text(max.saturating_sub(usage.cost_micro_usd))));
    }
    if parts.is_empty() {
        "run budget: no limits set".to_string()
    } else {
        format!("run budget remaining: {}", parts.join(", "))
    }
}

fn usd_text(micro: u64) -> String {
    format!("{}.{:02} USD", micro / 1_000_000, (micro % 1_000_000) / 10_000)
}

#[async_trait]
impl RequestObserver for PaymentsReminder {
    async fn before_request(
        &self,
        ctx: &Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        if ctx.agent_name() != self.agent {
            return Ok(());
        }
        let meters = self.meters(ctx.invocation_id());
        let paid = self.paid(ctx.as_ref(), meters.as_ref().and_then(|m| m.ledger.as_ref())).await;
        let budget = run_budget_text(meters.as_ref().and_then(|m| m.budget.as_deref()));
        request.contents.push(Content::new("user").with_text(format!(
            "<system-reminder>\nPayments: {}; {paid}; {budget}. Every payment needs the \
             user's approval, which the harness asks for when you call the tool.\n</system-reminder>",
            self.mandate.describe()
        )));
        Ok(())
    }
}

/// A payment tool whose every attempt writes `harness.payment` with the mandate, the
/// amount, the decision and the journal receipt (spec 3.1 §9.3). It forwards every
/// declaration method, so the tool's origin — and every held call bound to it — is
/// unchanged.
struct PaymentAuditedTool {
    inner: Arc<dyn Tool>,
    mandate: Arc<PaymentMandate>,
    transactions: Arc<dyn TransactionStore>,
}

impl PaymentAuditedTool {
    /// The transaction total: the masked summary's, else the call's cart total, else the
    /// journal's record.
    async fn total(
        &self,
        ctx: &dyn ToolContext,
        args: &Value,
        response: Option<&Value>,
    ) -> Option<Money> {
        let from_summary = response
            .and_then(|response| response.pointer("/summary/total"))
            .and_then(|total| serde_json::from_value(total.clone()).ok());
        if from_summary.is_some() {
            return from_summary;
        }
        let from_cart = args
            .pointer("/cart/total")
            .and_then(|total| serde_json::from_value(total.clone()).ok());
        if from_cart.is_some() {
            return from_cart;
        }
        let transaction_id = transaction_id_of(args, response)?;
        self.record(ctx, &transaction_id).await.map(|record| record.cart.total)
    }

    async fn record(
        &self,
        ctx: &dyn ToolContext,
        transaction_id: &TransactionId,
    ) -> Option<adk_payments::domain::TransactionRecord> {
        self.transactions
            .get(TransactionLookup {
                transaction_id: transaction_id.clone(),
                session_identity: ctx.try_identity().ok(),
            })
            .await
            .ok()
            .flatten()
    }

    /// The journal reference of the transaction: its id, its session-state key, and the
    /// order and receipt ids once the journal holds them.
    async fn receipt_ref(
        &self,
        ctx: &dyn ToolContext,
        args: &Value,
        response: Option<&Value>,
    ) -> Option<Value> {
        let transaction_id = transaction_id_of(args, response)?;
        let state_key = ctx
            .try_identity()
            .ok()
            .map(|identity| transaction_state_storage_key(&identity, &transaction_id));
        let record = self.record(ctx, &transaction_id).await;
        let order = record.as_ref().and_then(|record| record.order.as_ref());
        Some(json!({
            "transactionId": transaction_id.as_str(),
            "stateKey": state_key,
            "orderId": order.and_then(|order| order.order_id.clone()),
            "receiptId": order.and_then(|order| order.receipt_id.clone()),
            "receiptState": order.map(|order| order.receipt_state),
            "state": record.as_ref().map(|record| record.safe_summary.state),
        }))
    }
}

fn transaction_id_of(args: &Value, response: Option<&Value>) -> Option<TransactionId> {
    response
        .and_then(|response| response.pointer("/summary/transactionId"))
        .or_else(|| args.get("transactionId"))
        .and_then(Value::as_str)
        .map(TransactionId::from)
}

/// The `decision` field of a `harness.payment` record.
fn decision_of(result: &Result<Value>) -> &'static str {
    match result {
        Ok(_) => "executed",
        Err(error) if error.code == PAYMENT_POLICY_DENIED_CODE => "refused",
        Err(error) if error.code == PAYMENT_APPROVAL_REQUIRED_CODE => "pending",
        Err(error) if error.code == PAYMENT_APPROVAL_DENIED_CODE => "denied",
        Err(_) => "failed",
    }
}

#[async_trait]
impl Tool for PaymentAuditedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let result = self.inner.execute(ctx.clone(), args.clone()).await;
        let Some(audit) = HarnessAudit::for_tool(ctx.as_ref()) else {
            return result;
        };
        let response = result.as_ref().ok();
        let total = self.total(ctx.as_ref(), &args, response).await;
        let receipt = self.receipt_ref(ctx.as_ref(), &args, response).await;
        let decision = decision_of(&result);
        let recorded = audit
            .record(
                PAYMENT_AUDIT_ACTION,
                json!({
                    "mandateId": self.mandate.id,
                    "tool": self.inner.name(),
                    "functionCallId": ctx.function_call_id(),
                    "amount": total.as_ref().map(Money::to_decimal_string),
                    "amountMinor": total.as_ref().map(|money| money.amount_minor),
                    "currency": total.as_ref().map(|money| money.currency.clone()),
                    "journalReceiptRef": receipt,
                    "decision": decision,
                    "error": result.as_ref().err().map(|error| error.code),
                }),
            )
            .await;
        if let Err(error) = recorded {
            // The kernel has already answered, and may have moved money; the record cannot
            // undo it, so the failure is logged rather than returned.
            tracing::error!(
                error = %error,
                tool.name = %self.inner.name(),
                harness.payment.decision = decision,
                "could not record harness.payment"
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mandate_line_names_id_limit_and_merchants() {
        let mandate =
            PaymentMandate::new("m-1", "domain registration", Money::new("USD", 2_000, 2))
                .with_merchants(["domainmart", "namecheap"]);
        assert_eq!(
            mandate.describe(),
            "mandate m-1 (domain registration) allows at most 20.00 USD per transaction at merchants domainmart, namecheap"
        );
        assert_eq!(mandate.limit_micro_usd(), Some(20_000_000));
        let policies = mandate.policies();
        assert!(policies.contains("amount_threshold"));
        assert!(policies.contains("merchant_allowlist"));
        assert!(
            !PaymentMandate::new("m-2", "x", Money::new("EUR", 1, 2))
                .policies()
                .contains("merchant_allowlist")
        );
    }

    #[test]
    fn usd_text_prints_cents() {
        assert_eq!(usd_text(14_990_000), "14.99 USD");
        assert_eq!(usd_text(0), "0.00 USD");
        assert_eq!(usd_text(5_000), "0.00 USD");
        assert_eq!(usd_text(20_000_000), "20.00 USD");
    }

    #[test]
    fn the_run_budget_line_names_each_limit_it_sets() {
        use adk_core::RunBudget;
        assert_eq!(run_budget_text(None), "run budget: no limits set");
        let unlimited = BudgetTracker::new(RunBudget::new());
        assert_eq!(run_budget_text(Some(&unlimited)), "run budget: no limits set");
        let tracker = BudgetTracker::new(
            RunBudget::new().max_tool_calls(3).max_model_calls(5).max_cost_micro_usd(2_500_000),
        );
        tracker.begin_tool_calls(1).unwrap();
        assert_eq!(
            run_budget_text(Some(&tracker)),
            "run budget remaining: 2 tool calls, 5 model calls, 2.50 USD of model cost"
        );
    }

    #[test]
    fn transaction_ids_come_from_the_summary_then_the_args() {
        let args = json!({ "transactionId": "tx-args" });
        let response = json!({ "summary": { "transactionId": "tx-summary" } });
        assert_eq!(
            transaction_id_of(&args, Some(&response)),
            Some(TransactionId::from("tx-summary"))
        );
        assert_eq!(transaction_id_of(&args, None), Some(TransactionId::from("tx-args")));
        assert_eq!(transaction_id_of(&json!({}), None), None);
    }

    #[test]
    fn decisions_follow_the_kernel_codes() {
        use adk_core::{AdkError, ErrorCategory, ErrorComponent};
        let error =
            |code| Err(AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, code, "x"));
        assert_eq!(decision_of(&Ok(json!({}))), "executed");
        assert_eq!(decision_of(&error(PAYMENT_POLICY_DENIED_CODE)), "refused");
        assert_eq!(decision_of(&error(PAYMENT_APPROVAL_REQUIRED_CODE)), "pending");
        assert_eq!(decision_of(&error(PAYMENT_APPROVAL_DENIED_CODE)), "denied");
        assert_eq!(decision_of(&error("tool.other")), "failed");
    }
}
