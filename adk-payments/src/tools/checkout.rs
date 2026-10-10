use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
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
    TransactionId,
};
use crate::guardrail::redact_tool_output;
use crate::kernel::commands::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    UpdateCheckoutCommand,
};
use crate::kernel::service::MerchantCheckoutService;

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

fn parse_args<T: serde::de::DeserializeOwned>(tool_name: &str, args: Value) -> Result<T> {
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
fn tool_context(
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
// Create checkout tool
// ---------------------------------------------------------------------------

struct CreateCheckoutTool {
    checkout_service: Arc<dyn MerchantCheckoutService>,
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
        let params: CreateParams = parse_args("checkout_create", args)?;
        let tx_id = format!(
            "tool_tx_{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
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
        let record = self.checkout_service.create_checkout(command).await?;
        masked_response(record.safe_summary)
    }
}

/// Creates a `payments_checkout_create` tool backed by the given checkout service.
pub fn create_checkout_tool(checkout_service: Arc<dyn MerchantCheckoutService>) -> impl Tool {
    CreateCheckoutTool { checkout_service }
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
    checkout_service: Arc<dyn MerchantCheckoutService>,
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
        let params: CompleteParams = parse_args("checkout_complete", args)?;
        let context = tool_context(ctx.as_ref(), &params.transaction_id, "", "unknown", None);
        let command = CompleteCheckoutCommand {
            context,
            selected_payment_method: params.selected_payment_method,
            extensions: ProtocolExtensions::default(),
        };
        let record = self.checkout_service.complete_checkout(command).await?;
        masked_response(record.safe_summary)
    }
}

/// Creates a `payments_checkout_complete` tool backed by the given checkout service.
pub fn complete_checkout_tool(checkout_service: Arc<dyn MerchantCheckoutService>) -> impl Tool {
    CompleteCheckoutTool { checkout_service }
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
