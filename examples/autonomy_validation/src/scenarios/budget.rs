//! Run budgets and the spend ledger (Phase 1): a cost cap stops a team once its priced
//! spend reaches the cap, and a daily org cap on the spend ledger refuses a run before its
//! model is called.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, BudgetTracker, ErrorCategory, InMemorySpendLedger, Llm, LlmRequest, LlmResponseStream,
    Result as AdkResult, RunBudget, RunConfig, SchemaAdapter, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod, Tool, async_trait,
};
use adk_runner::{LlmSpendEstimate, Runner};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::AgentTool;
use futures::StreamExt;
use serde_json::json;

use crate::common::{
    Config, CountingTool, Provider, Verdict, brief, fail, run_turn, run_turn_with,
};
use crate::scenarios::commerce::{MERCHANT, MemoryMerchant, buy};

const USER: &str = "user-budget";

/// What one model call reported.
#[derive(Clone, Debug, Default)]
struct CallReport {
    saw_usage: bool,
    cost: Option<f64>,
}

/// Records the usage and cost every call reports, in call order.
struct CostTap {
    inner: Arc<dyn Llm>,
    calls: Arc<Mutex<Vec<CallReport>>>,
}

impl CostTap {
    fn reports(&self) -> Vec<CallReport> {
        self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

#[async_trait]
impl Llm for CostTap {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        let index = {
            let mut calls = self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            calls.push(CallReport::default());
            calls.len() - 1
        };
        let calls = Arc::clone(&self.calls);
        let responses = self.inner.generate_content(req, stream).await?;
        Ok(Box::pin(responses.map(move |item| {
            if let Ok(response) = &item
                && let Some(usage) = &response.usage_metadata
            {
                let mut calls = calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let report = &mut calls[index];
                report.saw_usage = true;
                // Streaming chunks can repeat cumulative usage; the largest cost is the call's.
                if let Some(cost) = usage.cost {
                    report.cost = Some(report.cost.map_or(cost, |seen| seen.max(cost)));
                }
            }
            item
        })))
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }
}

fn micro(usd: f64) -> u64 {
    (usd * 1_000_000.0).round() as u64
}

async fn session(app: &str, session_id: &str) -> anyhow::Result<Arc<InMemorySessionService>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    Ok(sessions)
}

// ─── team_budget_cap ────────────────────────────────────────────────────────

/// The run's cost cap, in micro-USD: a few calls on the cheapest models.
const COST_CAP_MICRO_USD: u64 = 400;

pub async fn team_budget_cap(cfg: &Config, provider: Provider) -> Verdict {
    match team_budget(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn team_budget(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let tap = Arc::new(CostTap {
        inner: cfg.model(provider)? as Arc<dyn Llm>,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let facts = CountingTool::new(
        "lookup_fact",
        "Returns research fact number n.",
        json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"]
        }),
        |_, args| Ok(json!({ "n": args["n"], "fact": format!("fact #{} is verified", args["n"]) })),
    );
    let researcher: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Looks up one research fact by number.")
            .model(Arc::clone(&tap) as Arc<dyn Llm>)
            .instruction(
                "Call lookup_fact with the number you are given, then reply with the fact.",
            )
            .tool(facts.clone() as Arc<dyn Tool>)
            .build()?,
    );
    let coordinator: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("coordinator")
            .model(Arc::clone(&tap) as Arc<dyn Llm>)
            .instruction(
                "You coordinate research. Ask the researcher tool for facts 1 through 20, one \
                 fact per call, in order. Keep going until you have all 20 or a tool fails.",
            )
            .tool(Arc::new(AgentTool::new(researcher)) as Arc<dyn Tool>)
            .build()?,
    );
    let app = "autonomy-budget";
    let session_id = "team-budget";
    let sessions = session(app, session_id).await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(coordinator)
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .build()?;
    // A caller-supplied tracker is kept for the run, so its counters can be read afterwards.
    let budget = RunBudget::new().max_cost_micro_usd(COST_CAP_MICRO_USD).max_model_calls(40);
    let tracker = Arc::new(BudgetTracker::new(budget.clone()));
    let mut config = RunConfig::builder().budget(budget).build();
    config.budget_tracker = Some(Arc::clone(&tracker));

    let turn =
        run_turn_with(&runner, USER, session_id, "Collect the research facts.", Some(config)).await;
    let reports = tap.reports();
    let usage = tracker.usage();

    match &turn.failure {
        Some((code, ErrorCategory::ResourceExhausted)) if code == "budget.cost" => {}
        other => {
            return Ok(Verdict::Fail(format!(
                "the run did not stop on the cost cap: {other:?} ({}) after {} calls, {} micro-USD",
                turn.error.as_deref().map(brief).unwrap_or_default(),
                reports.len(),
                usage.cost_micro_usd
            )));
        }
    }
    if let Some(missing) = reports.iter().position(|report| report.cost.is_none()) {
        return Ok(Verdict::Fail(format!(
            "call {} reported {} but no UsageMetadata::cost",
            missing + 1,
            if reports[missing].saw_usage { "usage" } else { "no usage" }
        )));
    }
    let costs: Vec<u64> =
        reports.iter().map(|report| micro(report.cost.unwrap_or_default())).collect();
    let mut running = 0;
    let crossing = costs.iter().position(|cost| {
        running += cost;
        running >= COST_CAP_MICRO_USD
    });
    let Some(crossing) = crossing else {
        return Ok(Verdict::Fail(format!(
            "stopped with ResourceExhausted below the cap: costs {costs:?}"
        )));
    };
    if costs.len() != crossing + 1 {
        return Ok(Verdict::Fail(format!(
            "{} model call(s) started after the cap was reached at call {} (costs {costs:?})",
            costs.len() - crossing - 1,
            crossing + 1
        )));
    }
    let total: u64 = costs.iter().sum();
    let largest = costs.iter().copied().max().unwrap_or_default();
    if total > COST_CAP_MICRO_USD + largest {
        return Ok(Verdict::Fail(format!(
            "spend {total} exceeds the cap {COST_CAP_MICRO_USD} plus one call ({largest})"
        )));
    }
    if usage.model_calls != costs.len() as u64
        || usage.cost_micro_usd.abs_diff(total) > costs.len() as u64
    {
        return Ok(Verdict::Fail(format!(
            "tracker counted {} calls / {} micro-USD; the provider reported {} calls / {total}",
            usage.model_calls,
            usage.cost_micro_usd,
            costs.len()
        )));
    }
    if crossing == 0 {
        return Ok(Verdict::Retry(format!(
            "the first call alone ({total} micro-USD) reached the cap"
        )));
    }
    Ok(Verdict::Pass(format!(
        "stopped budget.cost (ResourceExhausted) after {} priced calls: {total} micro-USD ≤ cap {COST_CAP_MICRO_USD} + one call; no call after the cap; lookup_fact {}x",
        costs.len(),
        facts.executions()
    )))
}

// ─── spend_ledger_daily_cap ─────────────────────────────────────────────────

pub async fn spend_ledger_daily_cap(cfg: &Config, provider: Provider) -> Verdict {
    match daily_cap(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

/// Runs one single-call turn against `ledger`, returning the turn and the model's call count.
async fn priced_run(
    cfg: &Config,
    provider: Provider,
    app: &str,
    session_id: &str,
    ledger: Arc<dyn SpendLedger>,
    estimate: u64,
) -> anyhow::Result<(crate::common::Turn, usize)> {
    let model = cfg.model(provider)?;
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::clone(&model) as Arc<dyn Llm>)
        .instruction("Reply with exactly one word: OK")
        .build()?;
    let sessions = session(app, session_id).await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().spend_ledger(ledger).build())
        .build()?
        .with_llm_spend_estimate(LlmSpendEstimate::per_call(estimate));
    let turn = run_turn(&runner, USER, session_id, "Status?").await;
    Ok((turn, model.calls()))
}

async fn daily_cap(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let app = "autonomy-spend";
    let vendor = provider.vendor();

    // A probe run prices one call, so the cap holds a few calls on any model.
    let probe = Arc::new(InMemorySpendLedger::default());
    let (turn, _) = priced_run(cfg, provider, app, "probe", probe.clone(), 1).await?;
    if let Some(error) = &turn.error {
        return Ok(Verdict::Fail(format!("probe run failed: {}", brief(error))));
    }
    let per_call = probe.spent(&SpendKey::org(app).with_vendor(vendor), SpendPeriod::Day).await?;
    if per_call <= 1 {
        return Ok(Verdict::Fail(format!(
            "the probe call was not recorded under vendor '{vendor}' at its reported cost ({per_call} micro-USD)"
        )));
    }
    // One org cap covers model spend and payments: a one-cent checkout plus about three
    // model calls.
    const CHECKOUT_CENTS: i64 = 1;
    const CHECKOUT_MICRO_USD: u64 = 10_000;
    let cap = CHECKOUT_MICRO_USD + per_call * 7 / 2;
    let ledger = Arc::new(InMemorySpendLedger::new(
        SpendLimits::new().limit(SpendKey::org(app).per(SpendPeriod::Day), cap),
    ));
    let merchant = Arc::new(MemoryMerchant::default());
    let with_ledger = RunConfig::builder().spend_ledger(ledger.clone()).build();
    if let Err(error) =
        buy(&merchant, with_ledger.clone(), app, "assistant", "buy-1", CHECKOUT_CENTS).await
    {
        return Ok(Verdict::Fail(format!("a checkout within the cap was refused: {error}")));
    }

    let mut completed = 0;
    let mut refused = None;
    for attempt in 0..10 {
        let session_id = format!("run-{attempt}");
        let (turn, model_calls) =
            priced_run(cfg, provider, app, &session_id, ledger.clone(), per_call).await?;
        match &turn.failure {
            Some((code, _)) if code == "spend.limit_exceeded" => {
                refused = Some(model_calls);
                break;
            }
            Some(_) => {
                return Ok(Verdict::Fail(format!(
                    "run {} failed: {}",
                    attempt + 1,
                    turn.error.as_deref().map(brief).unwrap_or_default()
                )));
            }
            None => completed += 1,
        }
    }
    let Some(model_calls) = refused else {
        return Ok(Verdict::Fail(format!(
            "{completed} runs completed and none was refused under a {cap} micro-USD daily cap"
        )));
    };
    if model_calls != 0 {
        return Ok(Verdict::Fail(format!("the refused run still called the model {model_calls}x")));
    }
    // The same cap now refuses a payment, before the merchant completes it.
    let completions = merchant.completions();
    let over_cap = buy(&merchant, with_ledger, app, "assistant", "buy-2", CHECKOUT_CENTS).await;
    match &over_cap {
        // The payment policy set reports the spend guardrail's refusal.
        Err(error)
            if error.code == "payments.policy.denied"
                && error.message.contains("spend limit exceeded") => {}
        other => {
            return Ok(Verdict::Fail(format!(
                "a checkout over the cap was not refused by the ledger: {other:?}"
            )));
        }
    }
    if merchant.completions() != completions {
        return Ok(Verdict::Fail("the refused checkout reached the merchant".to_string()));
    }

    let total = ledger.spent(&SpendKey::org(app), SpendPeriod::Day).await?;
    let model_spend =
        ledger.spent(&SpendKey::org(app).with_vendor(vendor), SpendPeriod::Day).await?;
    let payments =
        ledger.spent(&SpendKey::org(app).with_vendor(MERCHANT), SpendPeriod::Day).await?;
    let by_agent =
        ledger.spent(&SpendKey::org(app).with_agent("assistant"), SpendPeriod::Day).await?;
    if payments != CHECKOUT_MICRO_USD || model_spend + payments != total || by_agent != total {
        return Ok(Verdict::Fail(format!(
            "spend {total} not split into model spend under '{vendor}' ({model_spend}) and the checkout under '{MERCHANT}' ({payments}); agent total {by_agent}"
        )));
    }
    if total > cap + per_call {
        return Ok(Verdict::Fail(format!("committed {total} micro-USD past the {cap} cap")));
    }
    if completed == 0 {
        return Ok(Verdict::Fail("the cap refused the first run".to_string()));
    }
    Ok(Verdict::Pass(format!(
        "org cap {cap}: 1c checkout ({payments}) + {completed} runs ({model_spend} under '{vendor}'); run {} refused before its model call; a further 1c checkout refused by the spend_limit policy before the merchant",
        completed + 1
    )))
}
