use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::PAYMENT_EXECUTE_SCOPES;
use crate::domain::{CommerceMode, SafeTransactionSummary};
use crate::guardrail::redact_tool_output;
use crate::kernel::PaymentExecutionOutcome;
use crate::protocol::ap2::{Ap2Adapter, PaymentMandate};

use super::checkout::{parse_args, tool_caller, tool_context};

/// JSON parameters accepted by `payments_ap2_payment_submit`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitParams {
    transaction_id: String,
    /// The AP2 payment mandate, in its protocol wire format.
    payment_mandate: PaymentMandate,
    #[serde(default)]
    mode: Option<CommerceMode>,
}

/// Masked response for one submitted payment mandate.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitResponse {
    status: &'static str,
    outcome: PaymentExecutionOutcome,
    summary: SafeTransactionSummary,
    #[cfg(feature = "ap2-mcp")]
    continuation: crate::protocol::ap2::Ap2McpPaymentContinuation,
}

struct Ap2PaymentTool {
    adapter: Arc<Ap2Adapter>,
}

#[async_trait]
impl Tool for Ap2PaymentTool {
    fn name(&self) -> &str {
        "payments_ap2_payment_submit"
    }

    fn description(&self) -> &str {
        "Submit an AP2 payment mandate for a transaction that holds a cart mandate. The payment executes at most once. Returns a masked transaction summary."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "transactionId": { "type": "string" },
                "paymentMandate": {
                    "type": "object",
                    "description": "AP2 PaymentMandate with payment_mandate_contents and an optional user_authorization."
                },
                "mode": { "type": "string", "enum": ["human_present", "human_not_present"] }
            },
            "required": ["transactionId", "paymentMandate"]
        }))
    }

    fn required_scopes(&self) -> &[&str] {
        PAYMENT_EXECUTE_SCOPES
    }

    // Executing a payment mandate moves money.
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let caller = tool_caller(&ctx, self.name(), args.clone());
        let params: SubmitParams = parse_args(self.name(), args)?;
        let context =
            tool_context(ctx.as_ref(), &params.transaction_id, "", "unknown", params.mode);
        let result = self
            .adapter
            .submit_payment_mandate_as(context, params.payment_mandate, &caller)
            .await?;
        let response = SubmitResponse {
            status: "ok",
            outcome: result.outcome,
            summary: result.transaction.safe_summary.clone(),
            #[cfg(feature = "ap2-mcp")]
            continuation: crate::protocol::ap2::Ap2McpPaymentContinuation::from_record(
                &result.transaction,
            ),
        };
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
}

/// Creates a `payments_ap2_payment_submit` tool that submits AP2 payment mandates
/// through `adapter`.
///
/// The payment runs under the adapter's governance — payment policies, spend ledger, and
/// audit — attributed to the calling app and agent. The run's `RunConfig::spend_ledger`
/// is used when the adapter has no ledger of its own, and an escalated payment goes
/// through the run's tool confirmation flow. The tool declares
/// [`ToolEffect::NonIdempotent`], so an agent never retries it and, with an action
/// ledger, a timed-out call is answered `outcome_unknown` instead of paying twice. The
/// call's `ToolContext::idempotency_key()` reaches the payment backend in the
/// `idempotency_key` extension field.
///
/// The tool declares the `payments:payment:execute` scope but does not enforce it; use
/// [`PaymentToolsetBuilder::with_ap2_adapter`](super::PaymentToolsetBuilder::with_ap2_adapter)
/// or wrap it with `adk_auth::ScopeGuard::protect`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_payments::protocol::ap2::Ap2Adapter;
/// use adk_payments::tools::ap2_payment_tool;
/// use std::sync::Arc;
///
/// let adapter = Arc::new(
///     Ap2Adapter::new(checkout, payments, transactions, evidence)
///         .with_user_authorization_verifier(user_verifier)
///         .with_merchant_authorization_verifier(merchant_verifier),
/// );
/// let tool = ap2_payment_tool(adapter);
/// ```
pub fn ap2_payment_tool(adapter: Arc<Ap2Adapter>) -> impl Tool {
    Ap2PaymentTool { adapter }
}
