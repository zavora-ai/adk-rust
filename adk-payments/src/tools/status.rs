use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::CHECKOUT_CREATE_SCOPES;
use crate::domain::{SafeTransactionSummary, TransactionId};
use crate::guardrail::redact_tool_output;
use crate::kernel::commands::TransactionLookup;
use crate::kernel::service::TransactionStore;

/// JSON parameters accepted by `payments_status_lookup`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusParams {
    transaction_id: String,
}

/// Masked status response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    status: &'static str,
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<SafeTransactionSummary>,
}

struct StatusLookupTool {
    transaction_store: Arc<dyn TransactionStore>,
}

#[async_trait]
impl Tool for StatusLookupTool {
    fn name(&self) -> &str {
        "payments_status_lookup"
    }

    fn description(&self) -> &str {
        "Look up the current status of a payment transaction by its identifier. Returns a masked summary."
    }

    fn required_scopes(&self) -> &[&str] {
        CHECKOUT_CREATE_SCOPES
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: StatusParams = serde_json::from_value(args).map_err(|err| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "payments.tools.status_lookup.invalid_args",
                format!("invalid arguments for `status_lookup`: {err}"),
            )
        })?;
        let lookup = TransactionLookup {
            transaction_id: TransactionId::from(params.transaction_id),
            session_identity: None,
        };
        let record = self.transaction_store.get(lookup).await?;
        let response = StatusResponse {
            status: "ok",
            found: record.is_some(),
            summary: record.map(|r| r.safe_summary),
        };
        let value = serde_json::to_value(&response).map_err(|err| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "payments.tools.serialize_failed",
                format!("failed to serialize status response: {err}"),
            )
        })?;
        Ok(redact_tool_output(&value))
    }
}

/// Creates a `payments_status_lookup` tool backed by the given transaction store.
pub fn status_lookup_tool(transaction_store: Arc<dyn TransactionStore>) -> impl Tool {
    StatusLookupTool { transaction_store }
}
