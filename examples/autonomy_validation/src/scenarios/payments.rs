//! `payment_timeout_once`: a timed-out payment never repeats (Phase 1 gate). The charge
//! tool is non-idempotent, so the agent's retry budget does not apply to it; its timeout
//! is answered with an unknown outcome, and the action ledger answers a replay of the same
//! call after a crash without charging again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ActionLedger, Agent, Content, Event, Llm, LlmResponse, Part, Result as AdkResult, RetryBudget,
    RunConfig, Tool, ToolContext, ToolEffect, async_trait, is_outcome_unknown,
};
use adk_runner::Runner;
use adk_session::{
    CreateRequest, GetRequest, InMemorySessionService, SessionService, SqliteActionLedger,
};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::common::{
    Config, Provider, ScriptedLlm, Verdict, brief, fail, pair_calls, responses_for, run_turn,
    session_events,
};

const USER: &str = "user-payments";
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);
const HANG: Duration = Duration::from_secs(8);

/// What one execution of `charge_card` saw.
#[derive(Clone, Debug)]
struct Attempt {
    invocation_id: String,
    function_call_id: String,
    idempotency_key: String,
    args: Value,
}

/// Records the charge, then outlives the tool timeout.
struct ChargeCard {
    charges: Arc<AtomicUsize>,
    attempts: Arc<Mutex<Vec<Attempt>>>,
    hang: Duration,
}

#[async_trait]
impl Tool for ChargeCard {
    fn name(&self) -> &str {
        "charge_card"
    }

    fn description(&self) -> &str {
        "Charges the customer's card. Each call moves money."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string" },
                "amount_usd": { "type": "integer" }
            },
            "required": ["order_id", "amount_usd"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.charges.fetch_add(1, Ordering::SeqCst);
        self.attempts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(Attempt {
            invocation_id: ctx.invocation_id().to_string(),
            function_call_id: ctx.function_call_id().to_string(),
            idempotency_key: ctx.idempotency_key(),
            args,
        });
        tokio::time::sleep(self.hang).await;
        Ok(json!({ "status": "charged", "charge_id": "ch_0001" }))
    }
}

pub async fn payment_timeout_once(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let model = cfg.model(provider)?;
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite:{}?mode=rwc", dir.path().join("actions.db").display());
    let ledger = SqliteActionLedger::new(&url).await?;
    ledger.migrate().await?;

    let charges = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let charge = Arc::new(ChargeCard {
        charges: Arc::clone(&charges),
        attempts: Arc::clone(&attempts),
        hang: HANG,
    });
    let agent = LlmAgentBuilder::new("cashier")
        .model(model as Arc<dyn Llm>)
        .instruction(
            "You take payments. When asked to charge an order, call charge_card exactly once. \
             Never call charge_card a second time for the same order, whatever the result: if \
             the result is an error or its status is unknown, report that to the user and stop.",
        )
        .tool(charge as Arc<dyn Tool>)
        .tool_timeout(TOOL_TIMEOUT)
        // Retries are configured for every tool; a non-idempotent tool must not use them.
        .default_retry_budget(RetryBudget::new(3, Duration::from_millis(100)))
        .build()?;

    let app = "autonomy-payments";
    let session_id = "payment-timeout";
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().action_ledger(Arc::new(ledger)).build())
        .build()?;

    let started = Instant::now();
    let turn = run_turn(&runner, USER, session_id, "Charge order A-17 for 25 USD.").await;
    let elapsed = started.elapsed();
    let events = session_events(sessions.as_ref(), app, USER, session_id).await?;
    let (pairings, problems) = pair_calls(&events);
    let calls = pairings.iter().filter(|pairing| pairing.name == "charge_card").count();
    let charged = charges.load(Ordering::SeqCst);
    if calls == 0 {
        if let Some(error) = &turn.error {
            return Ok(Verdict::Fail(format!("turn failed before any charge: {}", brief(error))));
        }
        return Ok(Verdict::Retry("model never called charge_card".to_string()));
    }
    if calls > 1 {
        return Ok(Verdict::Retry(format!(
            "model issued charge_card {calls}x despite the instruction ({charged} charge(s))"
        )));
    }
    if charged != 1 {
        return Ok(Verdict::Fail(format!(
            "one charge_card call with 3 retries configured charged {charged}x"
        )));
    }
    let answers = responses_for(&events, "charge_card");
    let Some(answer) = answers.first() else {
        return Ok(Verdict::Fail(format!(
            "the timed-out call was not answered: {}",
            problems.join("; ")
        )));
    };
    if !is_outcome_unknown(answer) || !answer.to_string().contains("timed out") {
        return Ok(Verdict::Fail(format!(
            "the model received {answer} instead of an unknown outcome"
        )));
    }
    if !problems.is_empty() {
        return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
    }
    if let Some(error) = &turn.error {
        return Ok(Verdict::Fail(format!("turn error: {}", brief(error))));
    }

    // Simulated crash: the process dies, a new one reopens the ledger and the host replays
    // the call that was in flight, in the same invocation and under the same call ID.
    drop(runner);
    let attempt = attempts.lock().unwrap_or_else(|poisoned| poisoned.into_inner())[0].clone();
    let reopened = SqliteActionLedger::new(&url).await?;
    reopened.migrate().await?;
    let record = reopened.get(&attempt.idempotency_key).await?;
    let Some(record) = record else {
        return Ok(Verdict::Fail(format!(
            "no ledger record survived for key {}",
            attempt.idempotency_key
        )));
    };
    if record.outcome.is_some() {
        return Ok(Verdict::Fail(format!(
            "the timed-out call recorded outcome {:?}; it must stay unknown",
            record.outcome
        )));
    }
    let replay =
        replay_call(&sessions, app, session_id, &attempt, Arc::new(reopened), &charges).await?;
    let charged_after = charges.load(Ordering::SeqCst);
    let replay_answers = responses_for(&replay, "charge_card");
    if charged_after != 1 {
        return Ok(Verdict::Fail(format!(
            "the replayed call charged again ({charged_after} charges)"
        )));
    }
    match replay_answers.first() {
        Some(answer) if is_outcome_unknown(answer) => Ok(Verdict::Pass(format!(
            "1 call, 1 charge with 3 retries configured; answered outcome_unknown after {:.1}s; ledger record begun; replay after reopen answered outcome_unknown, still 1 charge",
            elapsed.as_secs_f64()
        ))),
        other => Ok(Verdict::Fail(format!("the replay was answered with {other:?}"))),
    }
}

/// Re-runs the in-flight call in its original invocation, as a host resuming after a crash.
async fn replay_call(
    sessions: &Arc<InMemorySessionService>,
    app: &str,
    session_id: &str,
    attempt: &Attempt,
    ledger: Arc<dyn ActionLedger>,
    charges: &Arc<AtomicUsize>,
) -> anyhow::Result<Vec<Event>> {
    let replayed = LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "charge_card".to_string(),
                args: attempt.args.clone(),
                id: Some(attempt.function_call_id.clone()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    };
    let charge = Arc::new(ChargeCard {
        charges: Arc::clone(charges),
        attempts: Arc::new(Mutex::new(Vec::new())),
        hang: Duration::ZERO,
    });
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("cashier")
            .model(ScriptedLlm::new("replay", vec![replayed]) as Arc<dyn Llm>)
            .tool(charge as Arc<dyn Tool>)
            .build()?,
    );
    let session = sessions
        .get(GetRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let ctx = adk_runner::InvocationContext::new(
        attempt.invocation_id.clone(),
        Arc::clone(&agent),
        USER.to_string(),
        app.to_string(),
        session_id.to_string(),
        Content::new("user").with_text("Charge order A-17 for 25 USD."),
        Arc::from(session),
    )?
    .with_run_config(RunConfig::builder().action_ledger(ledger).build());
    let mut stream = agent.run(Arc::new(ctx)).await?;
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item?);
    }
    Ok(events)
}
