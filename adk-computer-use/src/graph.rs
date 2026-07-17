use crate::{
    ActionClass, ActionPreview, ComputerUseAuthContext, ComputerUseRuntime, ControlLease,
    ExecutionMode, ExecutionReceipt, ScopeAuthorizer, TargetReservation,
};
use adk_graph::{
    Checkpointer, CompiledGraph, DeferredNodeConfig, END, GraphError, MergeStrategy, NodeOutput,
    START, StateGraph,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn node_error(node: &str, message: impl Into<String>) -> GraphError {
    GraphError::NodeExecutionFailed { node: node.to_string(), message: message.into() }
}

/// Build the flagship deterministic ADK graph.
///
/// The graph fans capability, visual, and semantic observation out concurrently,
/// joins them once, previews before mutation, interrupts for approval, and has
/// exactly one node that can call `execute_action`.
pub fn build_reference_graph(
    runtime: Arc<dyn ComputerUseRuntime>,
    authorizer: Arc<ScopeAuthorizer>,
) -> Result<CompiledGraph, GraphError> {
    build_reference_graph_with_checkpointer(runtime, authorizer, None)
}

/// Build the reference graph with a durable host-supplied checkpointer.
pub fn build_reference_graph_with_checkpointer(
    runtime: Arc<dyn ComputerUseRuntime>,
    authorizer: Arc<ScopeAuthorizer>,
    checkpointer: Option<Arc<dyn Checkpointer>>,
) -> Result<CompiledGraph, GraphError> {
    let capability_runtime = runtime.clone();
    let visual_runtime = runtime.clone();
    let semantic_runtime = runtime.clone();
    let preview_runtime = runtime.clone();
    let reservation_runtime = runtime.clone();
    let lease_runtime = runtime.clone();
    let execute_runtime = runtime.clone();
    let verify_runtime = runtime;

    StateGraph::with_channels(&[
        "proposed_action",
        "capabilities",
        "visual_evidence",
        "semantic_evidence",
        "observations_joined",
        "preview",
        "route",
        "approval",
        "approval_grant_id",
        "reservation",
        "lease",
        "receipt",
        "verified",
        "result",
    ])
    .add_node_fn("discover", move |_| {
        let runtime = capability_runtime.clone();
        async move {
            let value = runtime
                .discover_capabilities()
                .await
                .map_err(|error| node_error("discover", error.to_string()))?;
            Ok(NodeOutput::new().with_update("capabilities", value))
        }
    })
    .add_node_fn("observe_visual", move |_| {
        let runtime = visual_runtime.clone();
        async move {
            let value = runtime
                .observe_visual()
                .await
                .map_err(|error| node_error("observe_visual", error.to_string()))?;
            Ok(NodeOutput::new().with_update("visual_evidence", value))
        }
    })
    .add_node_fn("observe_semantic", move |_| {
        let runtime = semantic_runtime.clone();
        async move {
            let value = runtime
                .observe_semantic()
                .await
                .map_err(|error| node_error("observe_semantic", error.to_string()))?;
            Ok(NodeOutput::new().with_update("semantic_evidence", value))
        }
    })
    .add_deferred_node_fn(
        "join_observations",
        |ctx| async move {
            let complete = ctx.get("capabilities").is_some()
                && ctx.get("visual_evidence").is_some()
                && ctx.get("semantic_evidence").is_some();
            if !complete {
                return Err(node_error("join_observations", "fan-in completed without all evidence"));
            }
            Ok(NodeOutput::new().with_update("observations_joined", json!(true)))
        },
        DeferredNodeConfig { merge_strategy: MergeStrategy::Collect, fan_in_timeout: None },
    )
    .add_node_fn("plan", |ctx| async move {
        let proposed = ctx
            .get("proposed_action")
            .cloned()
            .ok_or_else(|| node_error("plan", "missing proposed_action"))?;
        Ok(NodeOutput::new().with_update("proposed_action", proposed))
    })
    .add_node_fn("preview", move |ctx| {
        let runtime = preview_runtime.clone();
        async move {
            let proposed = ctx
                .get("proposed_action")
                .cloned()
                .ok_or_else(|| node_error("preview", "missing proposed_action"))?;
            let preview = runtime
                .preview_action(proposed)
                .await
                .map_err(|error| node_error("preview", error.to_string()))?;
            let route = if preview.executable {
                "allowed"
            } else if preview.blocker.as_deref() == Some("approval_required") {
                "approval"
            } else {
                "blocked"
            };
            Ok(NodeOutput::new()
                .with_update("preview", serde_json::to_value(preview)?)
                .with_update("route", json!(route)))
        }
    })
    .add_node_fn("request_approval", |ctx| async move {
        let preview: ActionPreview = serde_json::from_value(
            ctx.get("preview")
                .cloned()
                .ok_or_else(|| node_error("request_approval", "missing preview"))?,
        )?;
        let approval = ctx.get("approval").and_then(Value::as_object);
        let approved_digest = approval
            .and_then(|value| value.get("actionDigest"))
            .and_then(Value::as_str);
        let grant_id = approval
            .and_then(|value| value.get("grantId"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let approved_policy = approval
            .and_then(|value| value.get("policyDigest"))
            .and_then(Value::as_str);
        let runtime_approved = approval
            .and_then(|value| value.get("runtimeApproved"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match (approved_digest, approved_policy, grant_id, runtime_approved) {
            (None, None, None, false) => Ok(NodeOutput::interrupt_with_data(
                "computer-use action requires scoped approval",
                serde_json::to_value(&preview)?,
            )),
            (Some(digest), Some(policy), Some(grant_id), _)
                if digest == preview.envelope.args_digest
                    && policy == preview.policy.policy_digest =>
            {
                Ok(NodeOutput::new().with_update("approval_grant_id", json!(grant_id)))
            }
            (Some(digest), Some(policy), None, true)
                if digest == preview.envelope.args_digest
                    && policy == preview.policy.policy_digest =>
            {
                Ok(NodeOutput::new())
            }
            _ => Err(node_error(
                "request_approval",
                "approval does not match the interrupted action and policy digests",
            )),
        }
    })
    .add_node_fn("blocked", |ctx| async move {
        let blocker = ctx
            .get("preview")
            .and_then(|value| value.get("blocker"))
            .cloned()
            .unwrap_or_else(|| json!("policy_denied"));
        Ok(NodeOutput::new().with_update("result", json!({ "status": "blocked", "reason": blocker })))
    })
    .add_node_fn("reserve_target", move |ctx| {
        let runtime = reservation_runtime.clone();
        async move {
            let preview: ActionPreview = serde_json::from_value(
                ctx.get("preview")
                    .cloned()
                    .ok_or_else(|| node_error("reserve_target", "missing preview"))?,
            )?;
            let reservation = runtime
                .reserve_target(&preview.envelope)
                .await
                .map_err(|error| node_error("reserve_target", error.to_string()))?;
            Ok(NodeOutput::new().with_update("reservation", serde_json::to_value(reservation)?))
        }
    })
    .add_node_fn("acquire_lease", move |ctx| {
        let runtime = lease_runtime.clone();
        async move {
            let preview: ActionPreview = serde_json::from_value(
                ctx.get("preview")
                    .cloned()
                    .ok_or_else(|| node_error("acquire_lease", "missing preview"))?,
            )?;
            let lease = runtime
                .acquire_lease(&preview.envelope)
                .await
                .map_err(|error| node_error("acquire_lease", error.to_string()))?;
            Ok(NodeOutput::new().with_update("lease", serde_json::to_value(lease)?))
        }
    })
    .add_node_fn("execute", move |ctx| {
        let runtime = execute_runtime.clone();
        let authorizer = authorizer.clone();
        async move {
            let preview: ActionPreview = serde_json::from_value(
                ctx.get("preview")
                    .cloned()
                    .ok_or_else(|| node_error("execute", "missing preview"))?,
            )?;
            let lease: ControlLease = serde_json::from_value(
                ctx.get("lease")
                    .cloned()
                    .ok_or_else(|| node_error("execute", "missing lease"))?,
            )?;
            let envelope = &preview.envelope;
            let auth = ComputerUseAuthContext {
                principal_id: envelope.principal_id.clone(),
                tenant_id: authorizer.verified_tenant_id().map(str::to_owned),
                session_id: envelope.session_id.clone(),
                execution_group_id: envelope.execution_group_id.clone().unwrap_or_default(),
                requested_mode: envelope.requested_mode,
                action_class: envelope.action_class,
                target_app: envelope.target.as_ref().map(|target| target.app_id.clone()),
                target_window: envelope
                    .target
                    .as_ref()
                    .and_then(|target| target.window_id.as_ref())
                    .map(|value| match value {
                        Value::String(value) => value.clone(),
                        value => value.to_string(),
                    }),
                policy_digest: preview.policy.policy_digest.clone(),
            };
            authorizer
                .authorize(&auth)
                .map_err(|error| node_error("execute", error.to_string()))?;
            let receipt = runtime
                .execute_action(
                    envelope,
                    &lease,
                    ctx.get("approval_grant_id").and_then(Value::as_str),
                )
                .await
                .map_err(|error| node_error("execute", error.to_string()))?;
            Ok(NodeOutput::new().with_update("receipt", serde_json::to_value(receipt)?))
        }
    })
    .add_node_fn("verify", move |ctx| {
        let runtime = verify_runtime.clone();
        async move {
            let receipt: ExecutionReceipt = serde_json::from_value(
                ctx.get("receipt")
                    .cloned()
                    .ok_or_else(|| node_error("verify", "missing receipt"))?,
            )?;
            let verified = runtime
                .verify(&receipt)
                .await
                .map_err(|error| node_error("verify", error.to_string()))?;
            if let Some(reservation) = ctx.get("reservation")
                && !reservation.is_null()
            {
                let reservation: TargetReservation = serde_json::from_value(reservation.clone())?;
                runtime
                    .release_target(&reservation)
                    .await
                    .map_err(|error| node_error("verify", error.to_string()))?;
            }
            Ok(NodeOutput::new()
                .with_update("verified", json!(verified))
                .with_update(
                    "result",
                    json!({ "status": if verified { "completed" } else { "verification_failed" }, "receiptId": receipt.receipt_id }),
                ))
        }
    })
    .add_edge(START, "discover")
    .add_edge(START, "observe_visual")
    .add_edge(START, "observe_semantic")
    .add_edge("discover", "join_observations")
    .add_edge("observe_visual", "join_observations")
    .add_edge("observe_semantic", "join_observations")
    .add_edge("join_observations", "plan")
    .add_edge("plan", "preview")
    .add_conditional_edges(
        "preview",
        |state| state.get("route").and_then(Value::as_str).unwrap_or("blocked").to_string(),
        [
            ("allowed", "reserve_target"),
            ("approval", "request_approval"),
            ("blocked", "blocked"),
        ],
    )
    .add_edge("request_approval", "reserve_target")
    .add_edge("blocked", END)
    .add_edge("reserve_target", "acquire_lease")
    .add_edge("acquire_lease", "execute")
    .add_edge("execute", "verify")
    .add_edge("verify", END)
    .compile()
    .map(|graph| match checkpointer {
        Some(checkpointer) => graph.with_checkpointer_arc(checkpointer),
        None => graph,
    })
}

// Keep the public contract variants referenced by rustdoc and exhaustive builds.
const _: (ExecutionMode, ActionClass) = (ExecutionMode::Shadow, ActionClass::Observe);
