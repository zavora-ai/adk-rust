use std::sync::Arc;

use adk_core::{
    AdkError, ApprovalScope, ErrorCategory, ErrorComponent, Result, Tool, ToolConfirmationDecision,
    ToolConfirmationRequest, ToolContext, ToolEffect, ToolGate, ToolOrigin,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{
    CHECKOUT_CANCEL_SCOPES, CHECKOUT_COMPLETE_SCOPES, CHECKOUT_CREATE_SCOPES,
    CHECKOUT_UPDATE_SCOPES,
};
use crate::domain::{
    Cart, CommerceMode, FulfillmentSelection, MerchantRef, PaymentMethodSelection,
    ProtocolDescriptor, ProtocolExtensionEnvelope, ProtocolExtensions, SafeTransactionSummary,
    TransactionId, TransactionRecord,
};
use crate::guardrail::{
    PaymentPolicyFinding, PaymentPolicySet, SpendLimitGuardrail, redact_tool_output,
};
use crate::kernel::commands::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    UpdateCheckoutCommand,
};
use crate::kernel::service::MerchantCheckoutService;
use crate::kernel::{GovernedCheckoutService, PaymentApproval, PaymentApprover, PaymentCaller};
use adk_core::audit_log::AuditActor;

use super::{caller_identity, calling_agent};

/// JSON parameters accepted by `payments_checkout_create`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateParams {
    merchant_id: String,
    merchant_name: String,
    cart: Cart,
    #[serde(default)]
    fulfillment: Option<FulfillmentSelection>,
    #[serde(default)]
    mode: Option<CommerceMode>,
}

/// JSON parameters accepted by `payments_checkout_update`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateParams {
    transaction_id: String,
    #[serde(default)]
    cart: Option<Cart>,
    #[serde(default)]
    fulfillment: Option<FulfillmentSelection>,
}

/// JSON parameters accepted by `payments_checkout_complete`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteParams {
    transaction_id: String,
    #[serde(default)]
    selected_payment_method: Option<PaymentMethodSelection>,
}

/// JSON parameters accepted by `payments_checkout_cancel`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelParams {
    transaction_id: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Masked tool response wrapping a safe transaction summary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResponse {
    status: &'static str,
    summary: SafeTransactionSummary,
}

pub(super) fn parse_args<T: serde::de::DeserializeOwned>(
    tool_name: &str,
    args: Value,
) -> Result<T> {
    serde_json::from_value(args).map_err(|err| {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "payments.tools.invalid_args",
            format!("invalid arguments for `{tool_name}`: {err}"),
        )
    })
}

/// Builds the commerce context for one tool call.
///
/// The call's idempotency key travels in the `idempotency_key` extension field, where
/// the ACP adapter places the `Idempotency-Key` header, so a backend deduplicates a
/// retried or replayed tool call the same way it deduplicates a replayed HTTP request.
pub(super) fn tool_context(
    ctx: &dyn ToolContext,
    transaction_id: &str,
    merchant_id: &str,
    merchant_name: &str,
    mode: Option<CommerceMode>,
) -> CommerceContext {
    let protocol = ProtocolDescriptor::new("adk-tool", Some("1.0".to_string()));
    let idempotency = ProtocolExtensionEnvelope::new(protocol.clone())
        .with_field("idempotency_key", Value::String(ctx.idempotency_key()));
    CommerceContext {
        transaction_id: TransactionId::from(transaction_id),
        session_identity: caller_identity(ctx),
        actor: calling_agent(ctx),
        merchant_of_record: MerchantRef {
            merchant_id: merchant_id.to_string(),
            legal_name: merchant_name.to_string(),
            display_name: Some(merchant_name.to_string()),
            statement_descriptor: None,
            country_code: None,
            website: None,
            extensions: ProtocolExtensions::default(),
        },
        payment_processor: None,
        mode: mode.unwrap_or(CommerceMode::HumanPresent),
        protocol,
        extensions: ProtocolExtensions::from(vec![idempotency]),
    }
}

fn masked_response(summary: SafeTransactionSummary) -> Result<Value> {
    let response = ToolResponse { status: "ok", summary };
    let value = serde_json::to_value(&response).map_err(|err| {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Internal,
            "payments.tools.serialize_failed",
            format!("failed to serialize tool response: {err}"),
        )
    })?;
    Ok(redact_tool_output(&value))
}

// ---------------------------------------------------------------------------
// Governance shared by the create and complete tools
// ---------------------------------------------------------------------------

/// Routes a payment escalation through the run's tool confirmation flow.
///
/// The escalation is decided by [`adk_core::confirm_tool_call`] from the sources the
/// governed path reads — a call-ID decision, a fingerprint approval, the approval store,
/// then the confirmation handler — for a request bound to the calling agent and the
/// tool's origin. Without a decision the request is recorded as pending, the call's event
/// carries it as `tool_confirmation`, and the payment is deferred until the agent resumes
/// the held call.
struct ToolCallApprover {
    ctx: Arc<dyn ToolContext>,
    tool_name: String,
    origin: ToolOrigin,
    args: Value,
}

#[async_trait]
impl PaymentApprover for ToolCallApprover {
    async fn approve(
        &self,
        _record: &TransactionRecord,
        findings: &[PaymentPolicyFinding],
    ) -> Result<PaymentApproval> {
        let call_id = self.ctx.function_call_id().to_string();
        tracing::info!(
            tool.name = %self.tool_name,
            call.id = %call_id,
            findings = findings.len(),
            "payment escalated for approval"
        );
        let acting = self.ctx.acting_context();
        let request = ToolConfirmationRequest {
            tool_name: self.tool_name.clone(),
            function_call_id: Some(call_id),
            args: self.args.clone(),
            acting: Some(Box::new(acting.clone())),
            origin: Some(self.origin.clone()),
        };
        // A caller outside a run has no decision sources; its event carries the request.
        if let Some(config) = self.ctx.run_config() {
            let gate = ToolGate::new(
                config,
                self.ctx.agent_name(),
                self.ctx.invocation_id(),
                ApprovalScope::new(self.ctx.app_name(), self.ctx.user_id(), self.ctx.session_id()),
            )
            .with_acting_context(acting);
            let reasons =
                findings.iter().map(|finding| finding.reason.as_str()).collect::<Vec<_>>();
            let reason = format!(
                "Tool '{}' requires confirmation: payment escalated for review ({})",
                self.tool_name,
                reasons.join("; ")
            );
            if let Some(approval) = adk_core::confirm_tool_call(&gate, &request, &reason).await? {
                return Ok(match approval.decision {
                    // The payment is already being made with the call's own arguments, so
                    // an approval for different ones does not authorize it.
                    ToolConfirmationDecision::Approve
                        if approval
                            .edited_args
                            .as_ref()
                            .is_some_and(|edited| *edited != self.args) =>
                    {
                        tracing::warn!(
                            tool.name = %self.tool_name,
                            "escalated payment refused: the approval edited its arguments"
                        );
                        PaymentApproval::Denied
                    }
                    ToolConfirmationDecision::Approve => PaymentApproval::Approved,
                    ToolConfirmationDecision::Deny => PaymentApproval::Denied,
                });
            }
        }
        let mut actions = self.ctx.actions();
        actions.tool_confirmation = Some(request);
        self.ctx.set_actions(actions);
        Ok(PaymentApproval::Pending)
    }
}

/// The paying caller for a tool call: the app, the agent, the run's spend ledger, and
/// the confirmation flow for escalations.
pub(super) fn tool_caller(
    ctx: &Arc<dyn ToolContext>,
    tool: &dyn Tool,
    args: Value,
) -> PaymentCaller {
    let mut caller = PaymentCaller::new()
        .with_org(ctx.app_name())
        .with_agent(ctx.acting_context().agent.id.as_str())
        .with_approver(Arc::new(ToolCallApprover {
            ctx: ctx.clone(),
            tool_name: tool.name().to_string(),
            origin: ToolOrigin::of(tool),
            args,
        }));
    if let Some(ledger) = ctx.run_config().and_then(|config| config.spend_ledger.clone()) {
        caller = caller.with_spend_ledger(ledger);
    }
    if let Some(trail) = ctx.run_config().and_then(|config| config.audit.clone()) {
        caller = caller.with_audit(trail, AuditActor::for_context(ctx.as_ref()));
    }
    caller
}

/// Governs a bare checkout service with only the spend guardrail, which reserves when the
/// run carries a spend ledger.
fn default_governance(
    checkout_service: Arc<dyn MerchantCheckoutService>,
) -> Arc<GovernedCheckoutService> {
    Arc::new(GovernedCheckoutService::new(
        checkout_service,
        PaymentPolicySet::new().with(SpendLimitGuardrail::when_configured()),
    ))
}

// ---------------------------------------------------------------------------
// Create checkout tool
// ---------------------------------------------------------------------------

struct CreateCheckoutTool {
    checkout: Arc<GovernedCheckoutService>,
}

#[async_trait]
impl Tool for CreateCheckoutTool {
    fn name(&self) -> &str {
        "payments_checkout_create"
    }

    fn description(&self) -> &str {
        "Create a new merchant-backed checkout session. Returns a masked transaction summary."
    }

    fn required_scopes(&self) -> &[&str] {
        CHECKOUT_CREATE_SCOPES
    }

    // Each call opens a new checkout session.
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let caller = tool_caller(&ctx, self, args.clone());
        let params: CreateParams = parse_args("checkout_create", args)?;
        // Derived from the idempotency key, so a replayed call names the same transaction
        // instead of opening a second one.
        let key_digest = adk_core::json_digest(&Value::String(ctx.idempotency_key()));
        let tx_id = format!(
            "tool_tx_{}",
            key_digest.split_once(':').map_or(key_digest.as_str(), |(_, hex)| hex)
        );
        let context = tool_context(
            ctx.as_ref(),
            &tx_id,
            &params.merchant_id,
            &params.merchant_name,
            params.mode,
        );
        let command =
            CreateCheckoutCommand { context, cart: params.cart, fulfillment: params.fulfillment };
        let record = self.checkout.create_checkout_as(command, &caller).await?;
        masked_response(record.safe_summary)
    }
}

/// Creates a `payments_checkout_create` tool backed by the given checkout service.
///
/// The tool reserves against `RunConfig::spend_ledger` when the run carries one. Use
/// [`PaymentToolsetBuilder::with_payment_policies`](super::PaymentToolsetBuilder::with_payment_policies)
/// to apply further payment policies.
pub fn create_checkout_tool(checkout_service: Arc<dyn MerchantCheckoutService>) -> impl Tool {
    CreateCheckoutTool { checkout: default_governance(checkout_service) }
}

pub(crate) fn governed_create_checkout_tool(checkout: Arc<GovernedCheckoutService>) -> impl Tool {
    CreateCheckoutTool { checkout }
}

// ---------------------------------------------------------------------------
// Update checkout tool
// ---------------------------------------------------------------------------

struct UpdateCheckoutTool {
    checkout_service: Arc<dyn MerchantCheckoutService>,
}

#[async_trait]
impl Tool for UpdateCheckoutTool {
    fn name(&self) -> &str {
        "payments_checkout_update"
    }

    fn description(&self) -> &str {
        "Update cart or fulfillment details on an existing checkout session."
    }

    fn required_scopes(&self) -> &[&str] {
        CHECKOUT_UPDATE_SCOPES
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: UpdateParams = parse_args("checkout_update", args)?;
        let context = tool_context(ctx.as_ref(), &params.transaction_id, "", "unknown", None);
        let command =
            UpdateCheckoutCommand { context, cart: params.cart, fulfillment: params.fulfillment };
        let record = self.checkout_service.update_checkout(command).await?;
        masked_response(record.safe_summary)
    }
}

/// Creates a `payments_checkout_update` tool backed by the given checkout service.
pub fn update_checkout_tool(checkout_service: Arc<dyn MerchantCheckoutService>) -> impl Tool {
    UpdateCheckoutTool { checkout_service }
}

// ---------------------------------------------------------------------------
// Complete checkout tool
// ---------------------------------------------------------------------------

struct CompleteCheckoutTool {
    checkout: Arc<GovernedCheckoutService>,
}

#[async_trait]
impl Tool for CompleteCheckoutTool {
    fn name(&self) -> &str {
        "payments_checkout_complete"
    }

    fn description(&self) -> &str {
        "Finalize a checkout session and produce an order. The continuation identifier is returned explicitly for follow-up."
    }

    fn required_scopes(&self) -> &[&str] {
        CHECKOUT_COMPLETE_SCOPES
    }

    // Completing a checkout authorizes payment and places an order.
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let caller = tool_caller(&ctx, self, args.clone());
        let params: CompleteParams = parse_args("checkout_complete", args)?;
        let context = tool_context(ctx.as_ref(), &params.transaction_id, "", "unknown", None);
        let command = CompleteCheckoutCommand {
            context,
            selected_payment_method: params.selected_payment_method,
            extensions: ProtocolExtensions::default(),
        };
        let record = self.checkout.complete_checkout_as(command, &caller).await?;
        masked_response(record.safe_summary)
    }
}

/// Creates a `payments_checkout_complete` tool backed by the given checkout service.
///
/// The tool reserves the checkout total against `RunConfig::spend_ledger` when the run
/// carries one, commits it once the checkout completes, and releases it if it fails.
pub fn complete_checkout_tool(checkout_service: Arc<dyn MerchantCheckoutService>) -> impl Tool {
    CompleteCheckoutTool { checkout: default_governance(checkout_service) }
}

pub(crate) fn governed_complete_checkout_tool(checkout: Arc<GovernedCheckoutService>) -> impl Tool {
    CompleteCheckoutTool { checkout }
}

// ---------------------------------------------------------------------------
// Cancel checkout tool
// ---------------------------------------------------------------------------

struct CancelCheckoutTool {
    checkout_service: Arc<dyn MerchantCheckoutService>,
}

#[async_trait]
impl Tool for CancelCheckoutTool {
    fn name(&self) -> &str {
        "payments_checkout_cancel"
    }

    fn description(&self) -> &str {
        "Cancel an active checkout session or transaction."
    }

    fn required_scopes(&self) -> &[&str] {
        CHECKOUT_CANCEL_SCOPES
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: CancelParams = parse_args("checkout_cancel", args)?;
        let context = tool_context(ctx.as_ref(), &params.transaction_id, "", "unknown", None);
        let command = CancelCheckoutCommand {
            context,
            reason: params.reason,
            extensions: ProtocolExtensions::default(),
        };
        let record = self.checkout_service.cancel_checkout(command).await?;
        masked_response(record.safe_summary)
    }
}

/// Creates a `payments_checkout_cancel` tool backed by the given checkout service.
pub fn cancel_checkout_tool(checkout_service: Arc<dyn MerchantCheckoutService>) -> impl Tool {
    CancelCheckoutTool { checkout_service }
}
