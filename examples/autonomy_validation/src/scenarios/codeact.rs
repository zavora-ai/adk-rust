//! `codeact_payment_once`: a `CodeActAgent` whose generated Python charges a card never
//! charges twice. The charge tool is non-idempotent and outlives the tool timeout, so the
//! program sees an `outcome_unknown` error; after a simulated crash, the step is replayed
//! from its write-ahead checkpoint in a new process and the action ledger answers it
//! without charging again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::codeact::checkpoint::ResolutionRecord;
use adk_agent::codeact::{CodeActAgent, CodeActCheckpoint, Disposition, PENDING_STATE_KEY};
use adk_codeact_monty::MontyRuntime;
use adk_core::{
    ActionLedger, Event, Llm, Result as AdkResult, RetryBudget, RunConfig, Tool, ToolContext,
    ToolEffect, async_trait, is_outcome_unknown,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService, SqliteActionLedger};
use serde_json::{Value, json};

use crate::common::{Config, Provider, Verdict, brief, fail, run_turn, session_events};

const APP: &str = "autonomy-codeact";
const USER: &str = "user-codeact";
const SESSION: &str = "codeact-payment";
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// Records the idempotency key of every charge, then outlives the tool timeout when
/// `hang` is set.
struct Charge {
    keys: Arc<Mutex<Vec<String>>>,
    hang: Option<Duration>,
}

#[async_trait]
impl Tool for Charge {
    fn name(&self) -> &str {
        "charge"
    }

    fn description(&self) -> &str {
        "Charges the customer's card for an order. Each call moves money."
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

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> AdkResult<Value> {
        self.keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(ctx.idempotency_key());
        if let Some(hang) = self.hang {
            tokio::time::sleep(hang).await;
        }
        Ok(json!({ "status": "charged", "charge_id": "ch_0001" }))
    }
}

pub async fn codeact_payment_once(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

fn runner(
    model: Arc<dyn Llm>,
    charge: Arc<Charge>,
    sessions: &Arc<InMemorySessionService>,
    ledger: Arc<dyn ActionLedger>,
) -> anyhow::Result<Runner> {
    let agent = CodeActAgent::builder()
        .name("cashier")
        .model(model)
        .runtime(Arc::new(MontyRuntime::new()))
        .instruction(
            "You take payments. To charge an order, run exactly this program once, with the \
             order's id and amount filled in, and never call charge again for that order:\n\n\
             try:\n    \
             outcome = call_tool(\"charge\", {\"order_id\": \"<id>\", \"amount_usd\": <amount>})\n\
             except Exception as error:\n    \
             outcome = str(error)\n\
             {\"type\": \"final_result\", \"value\": outcome}",
        )
        .tool(charge as Arc<dyn Tool>)
        .tool_timeout(TOOL_TIMEOUT)
        // Retries are configured for every tool; a non-idempotent tool must not use them.
        .default_retry_budget(RetryBudget::new(3, Duration::from_millis(100)))
        .max_iterations(4)
        .build()?;
    Ok(Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(sessions) as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().action_ledger(ledger).build())
        .build()?)
}

/// Every CodeAct checkpoint persisted in `events`, in order.
fn checkpoints(events: &[Event]) -> Vec<CodeActCheckpoint> {
    events
        .iter()
        .filter_map(|event| event.actions.state_delta.get(PENDING_STATE_KEY))
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}

/// The error each resolved charge raised into the program, parsed as JSON when it is.
fn raised(checkpoints: &[CodeActCheckpoint]) -> Vec<Value> {
    checkpoints
        .iter()
        .filter(|cp| cp.call.tool == "charge")
        .filter_map(|cp| match &cp.disposition {
            Disposition::Resolved(ResolutionRecord::Raise(message)) => {
                Some(serde_json::from_str(message).unwrap_or(Value::String(message.clone())))
            }
            _ => None,
        })
        .collect()
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite:{}?mode=rwc", dir.path().join("actions.db").display());
    let ledger = SqliteActionLedger::new(&url).await?;
    ledger.migrate().await?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;

    let keys = Arc::new(Mutex::new(Vec::new()));
    let charge = Arc::new(Charge { keys: Arc::clone(&keys), hang: Some(Duration::from_secs(8)) });
    let first = runner(cfg.model(provider)?, charge, &sessions, Arc::new(ledger))?;
    let turn = run_turn(&first, USER, SESSION, "Charge order A-17 for 25 USD.").await;
    let events = session_events(sessions.as_ref(), APP, USER, SESSION).await?;
    let recorded = checkpoints(&events);
    let charged = keys.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if charged.is_empty() {
        if let Some(error) = &turn.error {
            return Ok(Verdict::Fail(format!("turn failed before any charge: {}", brief(error))));
        }
        return Ok(Verdict::Retry("the program never called charge".to_string()));
    }
    if charged.len() > 1 {
        let distinct = charged.iter().collect::<std::collections::HashSet<_>>().len();
        return Ok(if distinct == charged.len() {
            Verdict::Retry(format!("the model's code called charge {}x", charged.len()))
        } else {
            Verdict::Fail(format!("one call key charged twice: {charged:?}"))
        });
    }
    let answers = raised(&recorded);
    match answers.first() {
        Some(answer) if is_outcome_unknown(answer) && answer.to_string().contains("timed out") => {}
        other => {
            return Ok(Verdict::Fail(format!(
                "the program saw {other:?} instead of an outcome_unknown error"
            )));
        }
    }
    if let Some(error) = &turn.error {
        return Ok(Verdict::Fail(format!("turn error: {}", brief(error))));
    }

    // Simulated crash after the charge, before its SAVE-AFTER checkpoint landed: a new
    // process reopens the ledger and finds the write-ahead checkpoint in session state.
    drop(first);
    let Some(save_before) = recorded
        .iter()
        .find(|cp| cp.call.tool == "charge" && cp.disposition == Disposition::PendingResult)
    else {
        return Ok(Verdict::Fail("no write-ahead checkpoint for the charge".to_string()));
    };
    let reopened = SqliteActionLedger::new(&url).await?;
    reopened.migrate().await?;
    match reopened.get(&charged[0]).await? {
        Some(record) if !record.is_completed() => {}
        other => {
            return Ok(Verdict::Fail(format!(
                "the timed-out charge must stay begun in the ledger, found {other:?}"
            )));
        }
    }
    let mut restore = Event::new("crash-recovery");
    restore.author = "host".to_string();
    restore
        .actions
        .state_delta
        .insert(PENDING_STATE_KEY.to_string(), serde_json::to_value(save_before)?);
    sessions.append_event(SESSION, restore).await?;

    let charge = Arc::new(Charge { keys: Arc::clone(&keys), hang: None });
    let second = runner(cfg.model(provider)?, charge, &sessions, Arc::new(reopened))?;
    let replay = run_turn(&second, USER, SESSION, "").await;
    let charged_after = keys.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if charged_after.len() != 1 {
        return Ok(if charged_after[1..].contains(&charged[0]) {
            Verdict::Fail(format!("the replayed call charged again: {charged_after:?}"))
        } else {
            Verdict::Retry(format!(
                "after the replay the model's new code charged again under a new key ({} charges)",
                charged_after.len()
            ))
        });
    }
    let replay_answers = raised(&checkpoints(&replay.events));
    match replay_answers.first() {
        Some(answer) if is_outcome_unknown(answer) => Ok(Verdict::Pass(format!(
            "generated code called charge once: 1 charge with 3 retries configured, raised outcome_unknown after the {}s timeout; ledger record begun; replay of the step after reopen raised outcome_unknown, still 1 charge",
            TOOL_TIMEOUT.as_secs()
        ))),
        other => Ok(Verdict::Fail(format!("the replay was answered with {other:?}"))),
    }
}
