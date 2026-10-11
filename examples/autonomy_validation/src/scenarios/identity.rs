//! Agent identity (Phase 2): a configured principal reaches every policy decision and
//! every spend record, and a redeploy under a new instance keeps the agent id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AgentId, AgentPrincipal, DeclarativePolicy, InMemorySpendLedger, Llm, PolicyDecision,
    PolicyRule, RunConfig, SpendKey, SpendLedger, SpendPeriod, Tool, ToolPolicy, ToolPolicyRequest,
    async_trait,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::json;

use crate::common::{Config, CountingTool, Provider, Verdict, brief, fail, run_turn};

const APP: &str = "autonomy-identity";
const USER: &str = "user-identity";
const AGENT_ID: &str = "spiffe://autonomy.test/agent/billing";

/// Delegates to a declarative policy and records the agent id of every decision.
#[derive(Debug)]
struct RecordingPolicy {
    inner: DeclarativePolicy,
    decided: Mutex<Vec<(Option<String>, Option<String>, PolicyDecision)>>,
}

#[async_trait]
impl ToolPolicy for RecordingPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        let decision = self.inner.decide(request);
        let principal = request.acting.as_ref().map(|acting| &acting.agent);
        self.decided.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((
            principal.map(|principal| principal.id.to_string()),
            principal.and_then(|principal| principal.instance.clone()),
            decision.clone(),
        ));
        decision
    }
}

pub async fn agent_identity(cfg: &Config, provider: Provider) -> Verdict {
    match run(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn run(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    // Only the configured agent may look invoices up; any other identity is denied by default.
    let policy = Arc::new(RecordingPolicy {
        inner: DeclarativePolicy::builder()
            .rule(PolicyRule::allow("lookup_invoice").for_agent(AGENT_ID).for_tenant("acme"))
            .build(),
        decided: Mutex::new(Vec::new()),
    });
    let ledger = Arc::new(InMemorySpendLedger::default());
    let lookup = CountingTool::new(
        "lookup_invoice",
        "Looks up an invoice's status.",
        json!({
            "type": "object",
            "properties": { "invoice_id": { "type": "string" } },
            "required": ["invoice_id"]
        }),
        |ctx, args| {
            Ok(json!({
                "invoice_id": args["invoice_id"],
                "status": "paid",
                "checked_by": ctx.acting_context().agent.id.to_string(),
            }))
        },
    );

    // Two deployments of the same agent: a redeploy changes the instance, not the id.
    for (instance, session_id) in [("replica-a", "deploy-a"), ("replica-b", "deploy-b")] {
        let model = cfg.model(provider)?;
        let agent = LlmAgentBuilder::new("billing")
            .model(model as Arc<dyn Llm>)
            .instruction(
                "You are a billing agent. Call lookup_invoice exactly once with the invoice id \
                 the user gives, then report its status in one sentence.",
            )
            .tool(lookup.clone() as Arc<dyn Tool>)
            .build()?;
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await?;
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(sessions as Arc<dyn SessionService>)
            .run_config(
                RunConfig::builder()
                    .tool_policy(policy.clone())
                    .spend_ledger(ledger.clone() as Arc<dyn SpendLedger>)
                    .build(),
            )
            .agent_principal(
                AgentPrincipal::new(AgentId::new(AGENT_ID)?)
                    .with_tenant("acme")
                    .with_class("billing-agent")
                    .with_instance(instance),
            )
            .build()?;
        let turn =
            run_turn(&runner, USER, session_id, "What is the status of invoice INV-42?").await;
        if let Some(error) = &turn.error {
            return Ok(Verdict::Fail(format!("{instance}: turn failed: {}", brief(error))));
        }
    }

    let decided = policy.decided.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if decided.is_empty() {
        return Ok(Verdict::Fail("the model never called lookup_invoice".to_string()));
    }
    if let Some(wrong) = decided.iter().find(|(id, _, _)| id.as_deref() != Some(AGENT_ID)) {
        return Ok(Verdict::Fail(format!("a policy decision carried agent id {:?}", wrong.0)));
    }
    if let Some(denied) = decided.iter().find(|(_, _, decision)| *decision != PolicyDecision::Allow)
    {
        return Ok(Verdict::Fail(format!("the configured agent was refused: {:?}", denied.2)));
    }
    let instances: Vec<_> =
        decided.iter().filter_map(|(_, instance, _)| instance.clone()).collect();
    if !instances.iter().any(|i| i == "replica-a") || !instances.iter().any(|i| i == "replica-b") {
        return Ok(Verdict::Fail(format!("both deployments must decide calls; saw {instances:?}")));
    }
    if lookup.executions() == 0 {
        return Ok(Verdict::Fail("the allowed lookup never executed".to_string()));
    }

    let total = ledger.spent(&SpendKey::org(APP), SpendPeriod::Day).await?;
    let by_agent = ledger.spent(&SpendKey::org(APP).with_agent(AGENT_ID), SpendPeriod::Day).await?;
    let derived = AgentPrincipal::derived(APP, "billing");
    let by_derived =
        ledger.spent(&SpendKey::org(APP).with_agent(derived.id.as_str()), SpendPeriod::Day).await?;
    if total == 0 || by_agent != total || by_derived != 0 {
        return Ok(Verdict::Fail(format!(
            "spend {total} micro-USD: {by_agent} under {AGENT_ID}, {by_derived} under {}",
            derived.id
        )));
    }

    Ok(Verdict::Pass(format!(
        "{} policy decisions across replica-a and replica-b all carried {AGENT_ID}; {} lookups executed; {total} micro-USD of model spend recorded under the agent id, none under the derived id",
        decided.len(),
        lookup.executions()
    )))
}
