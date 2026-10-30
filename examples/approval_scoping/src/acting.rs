//! Approvals name the acting agent and the tool's origin (#1210).
//!
//! The live scenarios serve real `LlmAgent`s on Anthropic `claude-sonnet-5-5` (`ANTHROPIC_MODEL`
//! overrides) over the SQLite session service and approval store. The realtime scenario drives
//! a `RealtimeRunner`'s native handler through its own governed dispatch; no Anthropic model
//! speaks the realtime protocol, so it runs locally.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, DeclarativePolicy, InMemoryApprovalStore, Llm, RunConfig,
    Tool, ToolApproval, ToolConfirmationDecision, ToolConfirmationRequest,
};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_realtime::RealtimeRunner;
use adk_realtime::config::ToolDefinition;
use adk_realtime::scripted::ScriptedRealtimeModel;
use adk_runner::Runner;
use adk_session::CreateRequest;
use serde_json::json;

use super::{APP, Durable, Transfer, USER, Verdict, agent_of, no_errors, run_turn, verdict};

const LIVE_ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";

/// Every agent here gets the same instruction, so only its name tells two calls apart.
const INSTRUCTION: &str = "You move money only when told to. If the latest user message is \
    exactly PAY, call the transfer tool once with amount 40 and nothing else. In every other \
    case reply with one short sentence and call no tools.";

fn claude() -> anyhow::Result<Option<(Arc<dyn Llm>, String)>> {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        return Ok(None);
    };
    let model =
        std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| LIVE_ANTHROPIC_MODEL.to_string());
    let config = AnthropicConfig::new(key, model.as_str()).with_max_tokens(256);
    Ok(Some((Arc::new(AnthropicClient::new(config)?), model)))
}

/// A runner serving one `LlmAgent` named `name`, its `transfer` held for confirmation. Every
/// agent's tool has the same declaration, so its origin is the same too.
fn treasury_runner(
    env: &Durable,
    name: &'static str,
    model: Arc<dyn Llm>,
) -> anyhow::Result<(Runner, Arc<Transfer>)> {
    let tool = Transfer::new(name, "operating");
    let agent = LlmAgentBuilder::new(name)
        .instruction(INSTRUCTION)
        .model(model)
        .tool(tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(env.sessions.clone())
        .run_config(env.base_config())
        .build()?;
    Ok((runner, tool))
}

async fn add_session(env: &Durable, session_id: &str) -> anyhow::Result<()> {
    env.sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    Ok(())
}

/// One agent's approval, recorded run-wide, does not authorize another agent's identical call
/// to an identically declared tool, and authorizes its own agent's call once.
pub(crate) async fn live_claude_approval_stays_with_its_agent() -> anyhow::Result<Verdict> {
    let Some((model, model_name)) = claude()? else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    let env = Durable::open("treasurer-session").await?;
    add_session(&env, "intern-session").await?;
    let (treasurer, treasurer_tool) = treasury_runner(&env, "treasurer", model.clone())?;
    let (intern, intern_tool) = treasury_runner(&env, "intern", model)?;
    let mut errors = Vec::new();

    let first = run_turn(&treasurer, "treasurer-session", "PAY", None).await?;
    errors.extend(first.error.clone());
    let Some(held) = first.held().into_iter().next() else {
        return Ok(Verdict::Skip(format!("{model_name} did not call transfer for the treasurer")));
    };
    let approved = RunConfig::builder()
        .approval_store(env.store.clone())
        .tool_approval(held.fingerprint(), ToolApproval::approve())
        .build();

    let second = run_turn(&intern, "intern-session", "PAY", Some(approved.clone())).await?;
    errors.extend(second.error.clone());
    let Some(intern_held) = second.held().into_iter().next() else {
        return Ok(Verdict::Skip(format!("{model_name} did not call transfer for the intern")));
    };
    if (&intern_held.args, &intern_held.origin) != (&held.args, &held.origin) {
        return Ok(Verdict::Skip(format!(
            "{model_name} made different calls: {} vs {}",
            held.args, intern_held.args
        )));
    }

    let third = run_turn(&treasurer, "treasurer-session", "Thanks.", Some(approved)).await?;
    errors.extend(third.error.clone());

    Ok(verdict(vec![
        no_errors(&errors),
        (
            agent_of(&held) != agent_of(&intern_held),
            format!(
                "{model_name}: the identical calls name {} and {}; arguments {} and origin {:?} \
                 match",
                agent_of(&held),
                agent_of(&intern_held),
                held.args,
                held.origin.as_ref().map(|origin| &origin.source)
            ),
        ),
        (
            intern_tool.executions() == 0,
            format!(
                "the intern's transfer ran {} time(s) under the treasurer's approval (expected 0)",
                intern_tool.executions()
            ),
        ),
        (
            treasurer_tool.executions() == 1,
            format!(
                "the treasurer's held transfer ran {} time(s) under its approval (expected 1)",
                treasurer_tool.executions()
            ),
        ),
    ]))
}

/// An approval built from the held call without its acting agent can never authorize a
/// governed call: the run config refuses it, and the call stays held until an approval that
/// names the agent arrives.
pub(crate) async fn live_claude_approval_naming_no_agent_is_refused() -> anyhow::Result<Verdict> {
    let Some((model, model_name)) = claude()? else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    let env = Durable::open("anonymous-session").await?;
    let (treasurer, tool) = treasury_runner(&env, "treasurer", model)?;
    let mut errors = Vec::new();

    let first = run_turn(&treasurer, "anonymous-session", "PAY", None).await?;
    errors.extend(first.error.clone());
    let Some(held) = first.held().into_iter().next() else {
        return Ok(Verdict::Skip(format!("{model_name} did not call transfer")));
    };
    // The mistake: the approval is built for the call, but without the agent that made it,
    // and recorded by every path that takes a fingerprint: run-wide, as the binding of a
    // call-ID decision, and in the approval store.
    let anonymous = ToolConfirmationRequest { acting: None, ..held.clone() }.fingerprint();
    let call_id = held.function_call_id.clone().unwrap_or_default();
    env.store.decide(&env.scope(), &anonymous, ToolApproval::approve()).await?;
    let refused = RunConfig::builder()
        .approval_store(env.store.clone())
        .tool_approval(anonymous.clone(), ToolApproval::approve())
        .tool_confirmation_decisions(HashMap::from([(
            call_id.clone(),
            ToolConfirmationDecision::Approve,
        )]))
        .tool_confirmation_fingerprints(HashMap::from([(call_id, anonymous.clone())]))
        .build();
    let recorded = refused.tool_approvals.len();

    let second = run_turn(&treasurer, "anonymous-session", "Thanks.", Some(refused)).await?;
    errors.extend(second.error.clone());
    let after_refused = tool.executions();
    let still_pending = env.pending_agents().await?;
    let store_kept = env.store.has_decision(&env.scope(), &anonymous).await?;

    let named = RunConfig::builder()
        .approval_store(env.store.clone())
        .tool_approval(held.fingerprint(), ToolApproval::approve())
        .build();
    let third = run_turn(&treasurer, "anonymous-session", "Thanks again.", Some(named)).await?;
    errors.extend(third.error.clone());

    Ok(verdict(vec![
        no_errors(&errors),
        (
            recorded == 0,
            format!(
                "{model_name}: an approval naming no agent for {}'s held transfer: {recorded} \
                 recorded (expected 0, refused)",
                agent_of(&held)
            ),
        ),
        (
            after_refused == 0 && still_pending.len() == 1,
            format!(
                "under the bound call-ID decision and the store decision naming no agent, the \
                 held transfer ran {after_refused} time(s) and {} call(s) still wait (expected \
                 0 and 1)",
                still_pending.len()
            ),
        ),
        (
            store_kept,
            format!("the store keeps the decision naming no agent, untaken: {store_kept}"),
        ),
        (
            tool.executions() == 1,
            format!(
                "with an approval naming the agent, the held transfer ran {} time(s) (expected 1)",
                tool.executions()
            ),
        ),
    ]))
}

/// A native handler's held call carries the `realtime` origin of its definition, so the
/// approval stops applying once the handler is redefined, and never covers another agent.
pub(crate) async fn realtime_handler_approval_follows_its_definition() -> anyhow::Result<Verdict> {
    let store = Arc::new(InMemoryApprovalStore::new());
    let scope = ApprovalScope::new("voice-desk", USER, "call-42");
    let executions = Arc::new(AtomicUsize::new(0));
    let config = RunConfig::builder()
        .tool_policy(Arc::new(
            DeclarativePolicy::builder()
                .require_approval("refund", "refunds need a person")
                .build(),
        ))
        .approval_store(store.clone())
        .build();
    let runner = |description: &str, agent: &str| {
        let executions = Arc::clone(&executions);
        RealtimeRunner::builder()
            .model(Arc::new(ScriptedRealtimeModel::new(Vec::new())))
            .agent_name(agent)
            .tool_fn(
                ToolDefinition::new("refund").with_description(description).with_parameters(
                    json!({ "type": "object", "properties": { "order": { "type": "string" } } }),
                ),
                move |call| {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({ "refunded": call.arguments["order"] }))
                },
            )
            .tool_governance(config.clone(), scope.clone())
            .build()
    };
    let args = r#"{"order":"A-1"}"#;

    let desk = runner("Refunds one order up to 50 USD.", "voice")?;
    desk.dispatch_tool_call("call-1", "refund", args).await?;
    let pending = store.pending(&scope).await?;
    let Some(held) = pending.first() else {
        anyhow::bail!("the refund was not held for approval");
    };
    let origin = held.request.origin.as_ref().map(|origin| origin.source.clone());
    store.decide(&scope, &held.fingerprint, ToolApproval::approve()).await?;

    // Each dispatch reports how many times the handler ran for it.
    let ran = |before: usize| executions.load(Ordering::SeqCst) - before;
    let widened = runner("Refunds any order, with no limit.", "voice")?;
    let before = executions.load(Ordering::SeqCst);
    widened.dispatch_tool_call("call-2", "refund", args).await?;
    let after_redefinition = ran(before);

    let other_agent = runner("Refunds one order up to 50 USD.", "after-hours")?;
    let before = executions.load(Ordering::SeqCst);
    other_agent.dispatch_tool_call("call-3", "refund", args).await?;
    let after_other_agent = ran(before);

    let before = executions.load(Ordering::SeqCst);
    desk.dispatch_tool_call("call-4", "refund", args).await?;
    let after_desk = ran(before);

    Ok(verdict(vec![
        (
            origin.as_deref() == Some("realtime"),
            format!(
                "the held call by {} carries origin source {origin:?} (expected \"realtime\")",
                agent_of(&held.request)
            ),
        ),
        (
            after_redefinition == 0,
            format!(
                "the redefined handler ran {after_redefinition} time(s) under the approval \
                 (expected 0)"
            ),
        ),
        (
            after_other_agent == 0,
            format!("another agent's identical call ran {after_other_agent} time(s) (expected 0)"),
        ),
        (
            after_desk == 1,
            format!(
                "the approved handler, by its own agent, ran {after_desk} time(s) (expected 1)"
            ),
        ),
    ]))
}
