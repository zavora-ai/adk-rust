//! `shared_state_fresh`: `app:` state written through one session is what a turn on another
//! session reads, and concurrent deltas all persist (#763).

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Event, Llm, Tool};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService, SqliteSessionService};
use serde_json::{Value, json};

use crate::common::{Config, CountingTool, Provider, Verdict, brief, fail, run_turn};

const APP: &str = "autonomy-state";

fn state_event(key: &str, value: Value) -> Event {
    let mut event = Event::new("inv-external-operator");
    event.author = "operator".to_string();
    event.actions.state_delta.insert(key.to_string(), value);
    event
}

async fn create(
    service: &SqliteSessionService,
    user_id: &str,
    session_id: &str,
    state: HashMap<String, Value>,
) -> anyhow::Result<()> {
    service
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: user_id.to_string(),
            session_id: Some(session_id.to_string()),
            state,
        })
        .await?;
    Ok(())
}

async fn state_of(
    service: &SqliteSessionService,
    user_id: &str,
    session_id: &str,
) -> anyhow::Result<HashMap<String, Value>> {
    let session = service
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    Ok(session.state().all())
}

pub async fn run(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("sessions.db").display());
    let service = Arc::new(SqliteSessionService::new(&url).await?);
    service.migrate().await?;

    let (alice, bob) = ("alice", "bob");
    let (session_a, session_b, session_c) = ("ops-a", "ops-b", "ops-c");
    create(
        &service,
        alice,
        session_a,
        HashMap::from([("app:kill_switch".to_string(), json!(false))]),
    )
    .await?;
    create(&service, bob, session_b, HashMap::new()).await?;
    create(&service, alice, session_c, HashMap::new()).await?;

    let kill_switch = CountingTool::new(
        "check_kill_switch",
        "Returns the current value of the application kill switch.",
        json!({ "type": "object", "properties": {} }),
        |ctx, _| {
            let value = ctx
                .session()
                .and_then(|session| session.state().get("app:kill_switch"))
                .unwrap_or(Value::Null);
            Ok(json!({ "kill_switch": value }))
        },
    );
    let agent = LlmAgentBuilder::new("ops_agent")
        .model(cfg.model(provider)? as Arc<dyn Llm>)
        .instruction(
            "Before answering, always call check_kill_switch exactly once, then reply with \
             'kill switch is <value>'.",
        )
        .tool(kill_switch.clone() as Arc<dyn Tool>)
        .build()?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(Arc::clone(&service) as Arc<dyn SessionService>)
        .build()?;

    let prompt = "Is the kill switch engaged? Check it now.";
    let first = run_turn(&runner, alice, session_a, prompt).await;
    if let Some(error) = &first.error {
        return Ok(Verdict::Fail(format!("turn 1 failed: {}", brief(error))));
    }
    if kill_switch.executions() == 0 {
        return Ok(Verdict::Retry("turn 1: model never called check_kill_switch".to_string()));
    }

    // An operator flips the switch through bob's session, not alice's.
    service.append_event(session_b, state_event("app:kill_switch", json!(true))).await?;
    let read_back = state_of(&service, alice, session_a).await?.get("app:kill_switch").cloned();

    let before = kill_switch.executions();
    let second = run_turn(&runner, alice, session_a, prompt).await;
    if let Some(error) = &second.error {
        return Ok(Verdict::Fail(format!("turn 2 failed: {}", brief(error))));
    }
    if kill_switch.executions() == before {
        return Ok(Verdict::Retry("turn 2: model never called check_kill_switch".to_string()));
    }
    let observations: Vec<Value> = {
        // `check_kill_switch` returns what it read; recover it from the tool's own results.
        let mut seen = Vec::new();
        for turn in [&first, &second] {
            for event in &turn.events {
                let Some(content) = &event.llm_response.content else { continue };
                for part in &content.parts {
                    if let adk_core::Part::FunctionResponse { function_response, .. } = part
                        && function_response.name == "check_kill_switch"
                    {
                        seen.push(function_response.response["kill_switch"].clone());
                    }
                }
            }
        }
        seen
    };
    let first_seen = observations.first().cloned().unwrap_or(Value::Null);
    let last_seen = observations.last().cloned().unwrap_or(Value::Null);

    // Concurrent deltas to different shared keys through different sessions.
    let writes = 12;
    let appends = (0..writes).map(|i| {
        let service = Arc::clone(&service);
        async move {
            let (session, key) = match i % 3 {
                0 => (session_a, format!("app:counter_{i}")),
                1 => (session_b, format!("app:counter_{i}")),
                _ => (session_c, format!("user:pref_{i}")),
            };
            service.append_event(session, state_event(&key, json!(i))).await
        }
    });
    let results = futures::future::join_all(appends).await;
    let append_errors: Vec<String> = results
        .into_iter()
        .filter_map(|result| result.err().map(|error| error.to_string()))
        .collect();
    let state = state_of(&service, alice, session_a).await?;
    let missing: Vec<String> = (0..writes)
        .map(|i| if i % 3 == 2 { format!("user:pref_{i}") } else { format!("app:counter_{i}") })
        .filter(|key| !state.contains_key(key))
        .collect();

    let mut issues = Vec::new();
    if read_back != Some(json!(true)) {
        issues.push(format!(
            "SessionService::get on session A returned app:kill_switch={read_back:?}"
        ));
    }
    if first_seen != json!(false) || last_seen != json!(true) {
        issues
            .push(format!("tool observed {first_seen} then {last_seen}, expected false then true"));
    }
    if !append_errors.is_empty() {
        issues.push(format!("concurrent appends failed: {}", brief(&append_errors.join("; "))));
    }
    if !missing.is_empty() {
        issues.push(format!("lost concurrent deltas: {missing:?}"));
    }
    Ok(if issues.is_empty() {
        Verdict::Pass(format!(
            "tool on session A saw kill_switch false→true after a write via session B; {writes} concurrent app:/user: deltas across 3 sessions all persisted"
        ))
    } else {
        Verdict::Fail(issues.join("; "))
    })
}
