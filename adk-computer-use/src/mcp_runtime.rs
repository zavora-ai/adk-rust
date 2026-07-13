use crate::{
    ActionEnvelope, ActionPreview, ComputerUseRuntime, ControlLease, ExecutionReceipt,
    SessionDeletionResult, SessionFollowUp, SessionFollowUpPage, TargetReservation,
};
use adk_tool::McpToolset;
use async_trait::async_trait;
use rmcp::{RoleClient, service::Service};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::Instrument;

#[derive(Debug, Clone)]
pub struct ComputerUseMcpConfig {
    pub session_id: String,
    pub expected_principal_id: String,
    pub capability_tool: String,
    pub target_app: Option<String>,
    pub target_window_id: Option<u64>,
    pub correlation: TraceCorrelation,
}

#[derive(Debug, Clone, Default)]
pub struct TraceCorrelation {
    pub adk_session_id: Option<String>,
    pub adk_invocation_id: Option<String>,
    pub adk_graph_thread_id: Option<String>,
    pub trace_id: Option<String>,
}

/// Actual MCP-backed runtime for the deterministic ADK graph.
pub struct ComputerUseMcpRuntime<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    toolset: Arc<McpToolset<S>>,
    config: ComputerUseMcpConfig,
    proposed: Mutex<HashMap<String, Map<String, Value>>>,
}

impl<S> ComputerUseMcpRuntime<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    pub fn new(toolset: Arc<McpToolset<S>>, config: ComputerUseMcpConfig) -> Self {
        Self { toolset, config, proposed: Mutex::new(HashMap::new()) }
    }

    /// Run a read-only desktop tool through the governed v8 shadow facade.
    /// Useful for bootstrapping exact target evidence before graph execution.
    pub async fn observe_tool(&self, tool: &str, arguments: Value) -> Result<Value, String> {
        self.observe_through_v8(tool, arguments).await
    }

    /// Permanently remove this adapter's terminal v8 session through the
    /// authenticated MCP principal. The server, not graph state, owns identity.
    pub async fn delete_terminal_session(&self) -> Result<SessionDeletionResult, String> {
        let value = output(
            self.call(
                "delete_session",
                object(json!({
                    "session_id": self.config.session_id,
                    "confirm": true,
                }))?,
            )
            .await?,
        );
        serde_json::from_value(value.get("deletion").cloned().unwrap_or(value))
            .map_err(|error| error.to_string())
    }

    /// Explicitly prune caller-owned terminal sessions before the supplied
    /// timestamp. The MCP server enforces principal ownership and the limit.
    pub async fn prune_terminal_sessions(
        &self,
        older_than: &str,
        limit: u32,
    ) -> Result<Vec<SessionDeletionResult>, String> {
        let value = output(
            self.call(
                "prune_sessions",
                object(json!({
                    "older_than": older_than,
                    "limit": limit,
                    "confirm": true,
                }))?,
            )
            .await?,
        );
        serde_json::from_value(value.get("deletions").cloned().unwrap_or_else(|| json!([])))
            .map_err(|error| error.to_string())
    }

    /// Consume remote/local steering with a monotonic cursor. The server binds
    /// both the session and returned instructions to the authenticated principal.
    pub async fn get_follow_ups(
        &self,
        after_sequence: u64,
        limit: u32,
    ) -> Result<SessionFollowUpPage, String> {
        if limit == 0 || limit > 1000 {
            return Err("follow-up limit must be between 1 and 1000".into());
        }
        let value = output(
            self.call(
                "get_follow_ups",
                object(json!({
                    "session_id": self.config.session_id,
                    "after_sequence": after_sequence,
                    "limit": limit,
                }))?,
            )
            .await?,
        );
        let page: SessionFollowUpPage =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        if page.follow_ups.iter().any(|item| {
            item.session_id != self.config.session_id
                || item.principal_id != self.config.expected_principal_id
        }) {
            return Err("v8 follow-up identity does not match authenticated ADK context".into());
        }
        Ok(page)
    }

    /// Submit steering through the same principal-bound MCP boundary. Hosts
    /// normally call this; exposing it here supports supervisor/graph tests.
    pub async fn submit_follow_up(&self, instruction: &str) -> Result<SessionFollowUp, String> {
        let value = output(
            self.call(
                "submit_follow_up",
                object(json!({
                    "session_id": self.config.session_id,
                    "instruction": instruction,
                }))?,
            )
            .await?,
        );
        let follow_up: SessionFollowUp =
            serde_json::from_value(value.get("follow_up").cloned().unwrap_or(value))
                .map_err(|error| error.to_string())?;
        if follow_up.session_id != self.config.session_id
            || follow_up.principal_id != self.config.expected_principal_id
        {
            return Err("v8 follow-up identity does not match authenticated ADK context".into());
        }
        Ok(follow_up)
    }

    async fn call(&self, name: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let span = tracing::info_span!(
            "computer_use.mcp",
            mcp.tool = name,
            v8.session_id = %self.config.session_id,
            adk.session_id = ?self.config.correlation.adk_session_id,
            adk.invocation_id = ?self.config.correlation.adk_invocation_id,
            adk.graph_thread_id = ?self.config.correlation.adk_graph_thread_id,
            trace_id = ?self.config.correlation.trace_id,
        );
        self.toolset
            .call_tool_value(name, arguments)
            .instrument(span)
            .await
            .map_err(|error| error.to_string())
    }

    async fn observe_through_v8(&self, tool: &str, arguments: Value) -> Result<Value, String> {
        self.call(
            "execute_action",
            object(json!({
                "session_id": self.config.session_id,
                "action_id": uuid::Uuid::new_v4().to_string(),
                "tool": tool,
                "arguments": arguments,
                "mode": "shadow",
                "data_labels": ["private"],
            }))?,
        )
        .await
    }
}

fn output(value: Value) -> Value {
    value
        .get("response")
        .and_then(|value| value.get("output"))
        .or_else(|| value.get("output"))
        .cloned()
        .unwrap_or(value)
}

fn object(value: Value) -> Result<Map<String, Value>, String> {
    let mut value = value
        .as_object()
        .cloned()
        .ok_or_else(|| "proposed action must be an object".to_string())?;
    value.retain(|_, entry| !entry.is_null());
    Ok(value)
}

#[async_trait]
impl<S> ComputerUseRuntime for ComputerUseMcpRuntime<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    async fn discover_capabilities(&self) -> Result<Value, String> {
        self.call(
            "get_execution_capabilities",
            object(json!({
                "tool": self.config.capability_tool,
                "app_id": self.config.target_app,
            }))?,
        )
        .await
    }

    async fn observe_visual(&self) -> Result<Value, String> {
        self.observe_through_v8(
            "snapshot",
            json!({
                "use_vision": true,
                "use_annotation": true,
                "target_app": self.config.target_app,
            }),
        )
        .await
    }

    async fn observe_semantic(&self) -> Result<Value, String> {
        let (name, args) = match self.config.target_window_id {
            Some(window_id) => ("get_ui_tree", json!({ "window_id": window_id })),
            None => ("list_windows", json!({ "bundle_id": self.config.target_app })),
        };
        self.observe_through_v8(name, args).await
    }

    async fn preview_action(&self, proposed_action: Value) -> Result<ActionPreview, String> {
        let mut args = object(proposed_action)?;
        args.insert("session_id".into(), json!(self.config.session_id));
        let preview: ActionPreview =
            serde_json::from_value(output(self.call("preview_action", args.clone()).await?))
                .map_err(|error| error.to_string())?;
        if preview.envelope.session_id != self.config.session_id {
            return Err("v8 preview returned a different session identity".into());
        }
        if preview.envelope.principal_id != self.config.expected_principal_id {
            return Err("v8 preview principal does not match authenticated ADK identity".into());
        }
        self.proposed.lock().await.insert(preview.envelope.action_id.clone(), args);
        Ok(preview)
    }

    async fn acquire_lease(&self, envelope: &ActionEnvelope) -> Result<ControlLease, String> {
        let kind = if envelope.requested_mode == crate::ExecutionMode::Foreground {
            "exclusive"
        } else {
            "cooperative"
        };
        let value = output(
            self.call(
                "acquire_control_lease",
                object(json!({
                    "session_id": envelope.session_id,
                    "agent_id": envelope.agent_id,
                    "kind": kind,
                    "mode": envelope.requested_mode,
                    "ttl_ms": 30_000,
                    "action_budget": 1,
                    "app_ids": envelope.target.as_ref().map(|target| vec![target.app_id.clone()]),
                    "window_ids": envelope.target.as_ref().and_then(|target| target.window_id.clone()).map(|id| vec![id]),
                }))?,
            )
            .await?,
        );
        serde_json::from_value(value.get("lease").cloned().unwrap_or(value))
            .map_err(|error| error.to_string())
    }

    async fn reserve_target(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<Option<TargetReservation>, String> {
        let Some(target) = envelope.target.as_ref() else {
            return Ok(None);
        };
        let value = output(
            self.call(
                "reserve_target",
                object(json!({
                    "session_id": envelope.session_id,
                    "intent_id": envelope.action_id,
                    "execution_group_id": envelope.execution_group_id,
                    "agent_id": envelope.agent_id,
                    "app_id": target.app_id,
                    "window_id": target.window_id,
                    "ttl_ms": 30_000,
                }))?,
            )
            .await?,
        );
        serde_json::from_value(value.get("reservation").cloned().unwrap_or(value))
            .map(Some)
            .map_err(|error| error.to_string())
    }

    async fn release_target(&self, reservation: &TargetReservation) -> Result<(), String> {
        self.call(
            "release_target_reservation",
            object(json!({
                "session_id": reservation.session_id,
                "reservation_id": reservation.reservation_id,
            }))?,
        )
        .await?;
        Ok(())
    }

    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        lease: &ControlLease,
        approval_grant_id: Option<&str>,
    ) -> Result<ExecutionReceipt, String> {
        let mut args = self
            .proposed
            .lock()
            .await
            .get(&envelope.action_id)
            .cloned()
            .ok_or_else(|| "missing exact proposed action for execution".to_string())?;
        args.insert("action_id".into(), json!(envelope.action_id));
        args.insert("lease_id".into(), json!(lease.lease_id));
        if let Some(grant) = approval_grant_id {
            args.insert("approval_grant_id".into(), json!(grant));
        }
        let value = output(self.call("execute_action", args).await?);
        let receipt: ExecutionReceipt =
            serde_json::from_value(value.get("receipt").cloned().unwrap_or(value))
                .map_err(|error| error.to_string())?;
        tracing::info!(
            v8.session_id = %receipt.session_id,
            v8.action_id = %receipt.action_id,
            v8.receipt_id = %receipt.receipt_id,
            v8.action_digest = %receipt.action_digest,
            "computer-use action receipt"
        );
        Ok(receipt)
    }

    async fn verify(&self, receipt: &ExecutionReceipt) -> Result<bool, String> {
        Ok(receipt.status == crate::ReceiptStatus::Committed)
    }

    async fn pause_session(&self, session_id: &str, reason: &str) -> Result<(), String> {
        self.call("pause_session", object(json!({ "session_id": session_id, "reason": reason }))?)
            .await?;
        Ok(())
    }

    async fn stop_session(&self, session_id: &str, reason: &str) -> Result<(), String> {
        self.call("stop_session", object(json!({ "session_id": session_id, "reason": reason }))?)
            .await?;
        Ok(())
    }

    async fn emergency_stop(&self, reason: &str) -> Result<(), String> {
        self.call("emergency_stop", object(json!({ "reason": reason }))?).await?;
        Ok(())
    }
}
