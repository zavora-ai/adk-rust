//! Portable, dependency-free showcase of the deterministic computer-use graph.
//!
//! Unlike the macOS examples under `examples/macos/`, this example needs no
//! `computer-use-mcp` server, no network, and no platform tooling. It supplies
//! an in-process [`ComputerUseRuntime`] so you can see the graph's control flow
//! — parallel observation, preview, one-writer lease, single execution, and
//! independent verification — on any platform.
//!
//! Run it with:
//!
//! ```bash
//! cargo run -p adk-computer-use --example minimal_graph
//! ```

use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPreview, ComputerUseError, ComputerUseRuntime, ControlLease,
    ExecutionCapability, ExecutionMode, ExecutionReceipt, LeaseBoundaries, PolicyDecision,
    ReceiptStatus, ScopeAuthorizer, build_reference_graph,
};
use adk_graph::{ExecutionConfig, State};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// A deterministic in-process runtime that mimics an allow-listed backend.
///
/// It observes trivially, previews an immediately-executable clipboard write,
/// grants a cooperative lease, and returns a committed receipt.
struct InProcessRuntime;

#[async_trait]
impl ComputerUseRuntime for InProcessRuntime {
    async fn discover_capabilities(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "capabilities": ["write_clipboard"] }))
    }

    async fn observe_visual(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "frame": "visual" }))
    }

    async fn observe_semantic(&self) -> Result<Value, ComputerUseError> {
        Ok(json!({ "frame": "semantic" }))
    }

    async fn preview_action(
        &self,
        _proposed_action: Value,
    ) -> Result<ActionPreview, ComputerUseError> {
        Ok(ActionPreview {
            envelope: ActionEnvelope {
                action_id: "action-1".into(),
                session_id: "session-1".into(),
                execution_group_id: Some("group-1".into()),
                principal_id: "adk-local-operator".into(),
                agent_id: Some("minimal-executor".into()),
                tool: "write_clipboard".into(),
                operation: "write_clipboard".into(),
                action_class: ActionClass::EditReversible,
                requested_mode: ExecutionMode::Background,
                target: None,
                target_sensitivity: None,
                resource: None,
                provenance: None,
                data_labels: vec!["public".into()],
                postcondition: None,
                reversible: true,
                external_side_effect: false,
                proposed_at: "2026-07-13T10:00:00Z".into(),
                expires_at: "2026-07-13T10:01:00Z".into(),
                args_digest: "digest-1".into(),
            },
            executable: true,
            blocker: None,
            policy: PolicyDecision {
                decision: "allow".into(),
                policy_digest: "policy-1".into(),
                reasons: vec!["allow-listed showcase action".into()],
                grant_id: None,
            },
            capability: ExecutionCapability {
                app_id: "*".into(),
                operation: "write_clipboard".into(),
                backend: "in-process".into(),
                supported_modes: vec![ExecutionMode::Shadow, ExecutionMode::Background],
                interference: "none".into(),
                confidence: 1.0,
                verified_at: None,
                verification_source: "example".into(),
            },
        })
    }

    async fn acquire_lease(
        &self,
        envelope: &ActionEnvelope,
    ) -> Result<ControlLease, ComputerUseError> {
        Ok(ControlLease {
            lease_id: "lease-1".into(),
            revision: 1,
            session_id: envelope.session_id.clone(),
            principal_id: envelope.principal_id.clone(),
            agent_id: envelope.agent_id.clone(),
            kind: "cooperative".into(),
            execution_mode: envelope.requested_mode,
            state: "active".into(),
            acquired_at: None,
            expires_at: "2026-07-13T10:01:00Z".into(),
            action_budget: 1,
            actions_used: 0,
            boundaries: LeaseBoundaries::default(),
        })
    }

    async fn execute_action(
        &self,
        envelope: &ActionEnvelope,
        _lease: &ControlLease,
        _approval_grant_id: Option<&str>,
    ) -> Result<ExecutionReceipt, ComputerUseError> {
        Ok(ExecutionReceipt {
            receipt_id: "receipt-1".into(),
            session_id: envelope.session_id.clone(),
            action_id: envelope.action_id.clone(),
            action_digest: envelope.args_digest.clone(),
            attempt: 1,
            status: ReceiptStatus::Committed,
            created_at: None,
            updated_at: None,
            result: Some(json!({ "ok": true })),
            error: None,
        })
    }

    async fn verify(&self, receipt: &ExecutionReceipt) -> Result<bool, ComputerUseError> {
        Ok(receipt.status == ReceiptStatus::Committed)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(InProcessRuntime);
    let authorizer =
        Arc::new(ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]));
    let graph = build_reference_graph(runtime, authorizer)?;

    let mut input = State::new();
    input.insert("proposed_action".into(), json!({ "tool": "write_clipboard" }));

    let result = graph.invoke(input, ExecutionConfig::new("minimal-graph")).await?;

    println!(
        "GRAPH_RESULT: {}",
        serde_json::to_string_pretty(&json!({
            "observations_joined": result.get("observations_joined"),
            "route": result.get("route"),
            "receipt_status": result.get("receipt").and_then(|value| value.get("status")),
            "verified": result.get("verified"),
            "result": result.get("result"),
        }))?
    );
    Ok(())
}
