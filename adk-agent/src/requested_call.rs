//! A call a person asked for through something the agent drew (spec 3.3 §7.4 rule 5).
//!
//! The run that carries a [`RequestedCall`] writes it as the agent's own call, then holds it
//! for the person's decision or refuses it; either way the run ends before any model call.
//! A later run resumes the held call as it resumes any other.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{
    Content, Event, FunctionResponseData, GovernedCall, InvocationContext, Part,
    REQUESTED_CALL_METADATA_KEY, RequestedCall, Result, Tool, ToolAuthorization, ToolGate,
    authorize_tool_call,
};

use crate::guardrails::{GuardrailScreen, ToolGuardrailSet};

/// Whether the session already carries `call`: a run that resumes its hold, or a woken run
/// that starts from the same input, never takes it twice.
pub(crate) fn taken(ctx: &dyn InvocationContext, call: &RequestedCall) -> bool {
    ctx.session().conversation_history().iter().flat_map(|content| &content.parts).any(|part| {
        match part {
            Part::FunctionCall { id, .. } | Part::FunctionResponse { id, .. } => {
                id.as_deref() == Some(call.function_call_id.as_str())
            }
            _ => false,
        }
    })
}

/// Why `ctx`'s run may not carry `call` at all, before its tool is looked up.
fn refused_run(ctx: &dyn InvocationContext, call: &RequestedCall) -> Option<String> {
    if !ctx.is_authenticated() {
        return Some("a call a person asks for needs a signed-in person".to_string());
    }
    if call.requested_by.id != ctx.user_id() {
        return Some("it was asked for by someone other than this session's person".to_string());
    }
    if !RequestedCall::is_requested_id(&call.function_call_id) {
        return Some("the request does not carry a requested call ID".to_string());
    }
    RequestedCall::refused_tool(&call.tool_name).map(str::to_string)
}

/// Why `args` do not fit `tool`'s parameters, or `None` when they do.
fn invalid_arguments(tool: &dyn Tool, args: &serde_json::Value) -> Option<String> {
    if !args.is_object() {
        return Some("its arguments are not one JSON object".to_string());
    }
    let schema = tool.parameters_schema()?;
    let validator = match jsonschema::validator_for(&schema) {
        Ok(validator) => validator,
        Err(error) => return Some(format!("the tool's parameters cannot be checked: {error}")),
    };
    let violations: Vec<String> = validator
        .iter_errors(args)
        .take(4)
        .map(|error| {
            let path = error.instance_path().as_str().to_string();
            let location = if path.is_empty() { "arguments".to_string() } else { path };
            format!("{location}: {error}")
        })
        .collect();
    (!violations.is_empty())
        .then(|| format!("its arguments do not fit the tool: {}", violations.join("; ")))
}

/// The events of `call` taken by `agent_name`: the call, its answer, and for a held call
/// its confirmation request.
fn call_events(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    call: &RequestedCall,
    response: serde_json::Value,
) -> Vec<Event> {
    let metadata = call.metadata();
    let event = |content: Content| {
        let mut event = Event::new(ctx.invocation_id());
        event.author = agent_name.to_string();
        event.branch = ctx.branch().to_string();
        event.provider_metadata.insert(REQUESTED_CALL_METADATA_KEY.to_string(), metadata.clone());
        event.llm_response.content = Some(content);
        event
    };
    let issued = event(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: call.tool_name.clone(),
            args: call.args.clone(),
            id: Some(call.function_call_id.clone()),
            thought_signature: None,
        }],
    });
    let answered = event(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new(call.tool_name.clone(), response),
            id: Some(call.function_call_id.clone()),
            annotations: None,
        }],
    });
    vec![issued, answered]
}

/// The events that refuse `call` with `error`, the answer the model reads.
fn refused(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    call: &RequestedCall,
    error: String,
) -> Vec<Event> {
    tracing::info!(
        tool.name = %call.tool_name,
        agent.name = %agent_name,
        "requested call refused"
    );
    call_events(ctx, agent_name, call, serde_json::json!({ "error": error }))
}

/// The events that refuse `call` for `reason`, answered as not run.
pub(crate) fn refusal_events(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    call: &RequestedCall,
    reason: &str,
) -> Vec<Event> {
    refused(ctx, agent_name, call, format!("Tool '{}' was not run: {reason}", call.tool_name))
}

/// Takes `call` for `agent_name`, whose tools are `tools`: the events that hold it for the
/// person's decision, or refuse it.
///
/// # Errors
///
/// Returns the governed path's error: the kill switch, or an approval store, approval
/// service or audit trail that fails.
pub(crate) async fn take(
    ctx: &Arc<dyn InvocationContext>,
    agent_name: &str,
    tools: &HashMap<String, Arc<dyn Tool>>,
    guardrails: &ToolGuardrailSet,
    call: &RequestedCall,
) -> Result<Vec<Event>> {
    if let Some(reason) = refused_run(ctx.as_ref(), call) {
        return Ok(refusal_events(ctx.as_ref(), agent_name, call, &reason));
    }
    let Some(tool) = tools.get(&call.tool_name) else {
        let reason = format!("this agent has no tool named '{}'", call.tool_name);
        return Ok(refusal_events(ctx.as_ref(), agent_name, call, &reason));
    };
    if let Some(reason) = invalid_arguments(tool.as_ref(), &call.args) {
        return Ok(refusal_events(ctx.as_ref(), agent_name, call, &reason));
    }
    let screen = GuardrailScreen(guardrails);
    let mut gate = ToolGate::for_context(ctx.as_ref());
    if !guardrails.is_empty() {
        gate = gate.with_screen(&screen);
    }
    let governed =
        GovernedCall::for_tool(tool.as_ref(), call.function_call_id.clone(), call.args.clone())
            .requested(true);
    match authorize_tool_call(&gate, governed).await? {
        ToolAuthorization::Pending { request, reason } => {
            tracing::info!(
                tool.name = %call.tool_name,
                agent.name = %agent_name,
                "requested call held for the person's decision"
            );
            let mut events = call_events(
                ctx.as_ref(),
                agent_name,
                call,
                adk_core::confirmation_pending_response(reason),
            );
            let mut held =
                crate::llm_agent::confirmation_event(ctx.invocation_id(), agent_name, request);
            held.branch = ctx.branch().to_string();
            held.provider_metadata.insert(REQUESTED_CALL_METADATA_KEY.to_string(), call.metadata());
            events.push(held);
            Ok(events)
        }
        ToolAuthorization::Refuse { reason, .. } => {
            Ok(refused(ctx.as_ref(), agent_name, call, reason))
        }
        // Authorized for a call with no decision would break the primitive's rule, so it is
        // refused rather than run.
        ToolAuthorization::Execute { .. } => Ok(refusal_events(
            ctx.as_ref(),
            agent_name,
            call,
            "it was not held for the person's decision",
        )),
    }
}
