use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::INTERVENTION_CONTINUE_SCOPES;
use crate::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, ProtocolDescriptor,
    ProtocolExtensions, SafeTransactionSummary, TransactionId,
};
use crate::guardrail::redact_tool_output;
use crate::kernel::commands::{CommerceContext, ContinueInterventionCommand};
use crate::kernel::service::InterventionService;

/// JSON parameters accepted by `payments_intervention_continue`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinueParams {
    transaction_id: String,
    intervention_id: String,
    #[serde(default)]
    continuation_token: Option<String>,
    #[serde(default)]
    result_summary: Option<String>,
}

/// Masked intervention response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InterventionResponse {
    status: &'static str,
    summary: SafeTransactionSummary,
}

struct ContinueInterventionTool {
    intervention_service: Arc<dyn InterventionService>,
}

#[async_trait]
impl Tool for ContinueInterventionTool {
    fn name(&self) -> &str {
        "payments_intervention_continue"
    }

    fn description(&self) -> &str {
        "Resume or complete a payment intervention such as 3DS or buyer reconfirmation. The continuation token is required for safe resumption."
    }

    fn is_long_running(&self) -> bool {
        true
    }

    fn required_scopes(&self) -> &[&str] {
        INTERVENTION_CONTINUE_SCOPES
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: ContinueParams = serde_json::from_value(args).map_err(|err| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "payments.tools.intervention_continue.invalid_args",
                format!("invalid arguments for `intervention_continue`: {err}"),
            )
        })?;
        let context = CommerceContext {
            transaction_id: TransactionId::from(params.transaction_id),
            session_identity: None,
            actor: CommerceActor {
                actor_id: "agent-tool".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("payment tool".to_string()),
                tenant_id: None,
                extensions: ProtocolExtensions::default(),
            },
            merchant_of_record: MerchantRef {
                merchant_id: String::new(),
                legal_name: "unknown".to_string(),
                display_name: None,
                statement_descriptor: None,
                country_code: None,
                website: None,
                extensions: ProtocolExtensions::default(),
            },
            payment_processor: None,
            mode: CommerceMode::HumanPresent,
            protocol: ProtocolDescriptor::new("adk-tool", Some("1.0".to_string())),
            extensions: ProtocolExtensions::default(),
        };
        let command = ContinueInterventionCommand {
            context,
            intervention_id: params.intervention_id,
            continuation_token: params.continuation_token,
            result_summary: params.result_summary,
            extensions: ProtocolExtensions::default(),
        };
        let record = self.intervention_service.continue_intervention(command).await?;
        let response = InterventionResponse { status: "ok", summary: record.safe_summary };
        let value = serde_json::to_value(&response).map_err(|err| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "payments.tools.serialize_failed",
                format!("failed to serialize intervention response: {err}"),
            )
        })?;
        Ok(redact_tool_output(&value))
    }
}

/// Creates a `payments_intervention_continue` tool backed by the given intervention service.
pub fn continue_intervention_tool(intervention_service: Arc<dyn InterventionService>) -> impl Tool {
    ContinueInterventionTool { intervention_service }
}
