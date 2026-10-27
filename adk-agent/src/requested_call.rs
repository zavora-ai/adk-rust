//! A call a person asked for through something the agent drew (spec 3.3 §7.4 rule 5).
//!
//! The run that carries a [`RequestedCall`] writes the runtime's notice of what the person
//! did, then the call as the agent's own, then holds it for the person's decision or refuses
//! it; either way the run ends before any model call. The call goes through the tool path a
//! model's call takes, before-tool stages included, and is authorized as
//! [`GovernedCall::requested`](adk_core::GovernedCall::requested). The model reads the call
//! through the notice and the answer as delimited data ([`for_model`]): never as a call it
//! made, and never as the person's own words. A later run resumes the held call as it
//! resumes any other.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{
    Content, Event, EventStream, InvocationContext, Part, RequestedCall, Tool,
    ToolConfirmationRequest,
};
use serde_json::Value;

/// Whether the session already carries `call`: a run that resumes its hold, or a woken run
/// that starts from the same input, never takes it twice.
pub(crate) fn taken(ctx: &dyn InvocationContext, call: &RequestedCall) -> bool {
    call.taken_in(&ctx.session().conversation_history())
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

/// Why the agent whose tools are `tools` refuses `call` before the tool path, or `None` when
/// the call goes on to it.
pub(crate) fn refusal(
    ctx: &dyn InvocationContext,
    tools: &HashMap<String, Arc<dyn Tool>>,
    call: &RequestedCall,
) -> Option<String> {
    if let Some(reason) = refused_run(ctx, call) {
        return Some(reason);
    }
    let Some(tool) = tools.get(&call.tool_name) else {
        return Some(format!("this agent has no tool named '{}'", call.tool_name));
    };
    invalid_arguments(tool.as_ref(), &call.args)
}

/// The events that refuse `call` for `reason`, answered as not run.
pub(crate) fn refusal_events(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    call: &RequestedCall,
    reason: &str,
) -> Vec<Event> {
    tracing::info!(
        tool.name = %call.tool_name,
        agent.name = %agent_name,
        "requested call refused"
    );
    call.refusal_events(ctx.invocation_id(), ctx.branch(), agent_name, reason)
}

/// The events written before `call` goes through the tool path as `agent_name`'s: the notice
/// and the call. The tool path writes the answer, and a held call's confirmation follows it.
pub(crate) fn asked_events(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    call: &RequestedCall,
) -> Vec<Event> {
    let mut events = call.events(ctx.invocation_id(), ctx.branch(), agent_name, Value::Null);
    events.truncate(2);
    events
}

/// The marked confirmation event of `call`, held as `agent_name` with `request`.
pub(crate) fn held_event(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    call: &RequestedCall,
    request: ToolConfirmationRequest,
) -> Event {
    tracing::info!(
        tool.name = %call.tool_name,
        agent.name = %agent_name,
        "requested call held for the person's decision"
    );
    let mut held = crate::llm_agent::confirmation_event(ctx.invocation_id(), agent_name, request);
    held.branch = ctx.branch().to_string();
    call.mark(&mut held);
    held
}

/// Why a workflow or team that starts a run refuses its requested call.
pub(crate) const RUNS_OTHER_AGENTS: &str =
    "this agent runs other agents and cannot hold a call for a person's decision";

/// The stream that refuses the run's requested call when `agent_name`, an agent that cannot
/// hold a call for a person's decision, starts the run: a workflow, a team or a script
/// answers it as not run, before any sub-agent or model runs. `None` when the run carries no
/// call for it.
pub(crate) fn refused_by_root(
    ctx: &dyn InvocationContext,
    agent_name: &str,
    reason: &str,
) -> Option<EventStream> {
    let call = ctx.run_config().requested_call.as_ref()?;
    if ctx.agent_name() != agent_name || taken(ctx, call) {
        return None;
    }
    let events = refusal_events(ctx, agent_name, call, reason);
    Some(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
}

/// `contents` as the model reads them: a requested call leaves the model's turns, and its
/// answer is a runtime notice after the notice of what the person did, with the tool's
/// output delimited as data. A model that reads a call it did not make as its own disowns
/// it, a provider that pairs a call with its result by ID never receives one it did not
/// issue, and a tool's output never reads as the person's instruction.
pub(crate) fn for_model(contents: Vec<Content>) -> Vec<Content> {
    let mut out: Vec<Content> = Vec::with_capacity(contents.len());
    for mut content in contents {
        let mut said = Vec::new();
        content.parts.retain(|part| match part {
            Part::FunctionCall { id: Some(id), .. } => !RequestedCall::is_requested_id(id),
            Part::FunctionResponse { function_response, id: Some(id), .. }
                if RequestedCall::is_requested_id(id) =>
            {
                said.push(Part::Text {
                    text: format!(
                        "{}The {} call the person asked for answered:\n{}",
                        adk_core::history::SYSTEM_NOTICE_PREFIX,
                        function_response.name,
                        adk_core::untrusted_block(
                            "tool",
                            Some(&function_response.name),
                            &function_response.response.to_string()
                        )
                    ),
                });
                false
            }
            _ => true,
        });
        if !content.parts.is_empty() {
            out.push(content);
        }
        if !said.is_empty() {
            match out.last_mut() {
                Some(last) if last.role == "user" => last.parts.extend(said),
                _ => out.push(Content { role: "user".to_string(), parts: said }),
            }
        }
    }
    out
}
