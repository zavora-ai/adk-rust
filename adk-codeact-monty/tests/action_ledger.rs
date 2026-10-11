//! A `CodeActAgent` on the Monty runtime records non-idempotent tool calls in the
//! action ledger, so a Python program's side effects do not repeat when a step is
//! replayed from a checkpoint.
//!
//! Every test drives a real `CodeActAgent` through a `Runner` with a scripted model.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_agent::codeact::{CodeActAgent, CodeActCheckpoint, Disposition, PENDING_STATE_KEY};
use adk_codeact_monty::MontyRuntime;
use adk_core::{
    ActionLedger, Content, Event, InMemoryActionLedger, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Result, RunConfig, SessionId, Tool, ToolConfirmationDecision, ToolContext,
    ToolEffect, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "codeact-ledger";
const SESSION: &str = "session";

/// Replays scripted Python programs, one per model turn.
struct ScriptedModel {
    programs: Mutex<VecDeque<String>>,
}

impl ScriptedModel {
    fn new(programs: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            programs: Mutex::new(programs.iter().map(|program| program.to_string()).collect()),
        })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let program = self.programs.lock().unwrap().pop_front().expect("a scripted program");
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(format!("```python\n{program}\n```"))),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// A non-idempotent `charge` tool that records the idempotency key of every execution.
struct ChargeTool {
    keys: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for ChargeTool {
    fn name(&self) -> &str {
        "charge"
    }

    fn description(&self) -> &str {
        "charges the customer's card"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let key = ctx.idempotency_key();
        self.keys.lock().unwrap().push(key.clone());
        Ok(json!({ "charged": args["amount"], "key": key }))
    }
}

struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
    keys: Arc<Mutex<Vec<String>>>,
}

async fn harness(programs: &[&str], confirm_charge: bool) -> Harness {
    let keys = Arc::new(Mutex::new(Vec::new()));
    let mut builder = CodeActAgent::builder()
        .name("cashier")
        .model(ScriptedModel::new(programs))
        .runtime(Arc::new(MontyRuntime::new()))
        .tool(Arc::new(ChargeTool { keys: keys.clone() }));
    if confirm_charge {
        builder = builder.require_tool_confirmation("charge");
    }
    let agent = builder.build().unwrap();

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "user".to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    Harness { runner, sessions, keys }
}

impl Harness {
    async fn run(&self, text: &str, config: RunConfig) -> Vec<Event> {
        let stream = self
            .runner
            .run_with_config(
                UserId::new("user").unwrap(),
                SessionId::new(SESSION).unwrap(),
                Content::new("user").with_text(text),
                Some(config),
            )
            .await
            .unwrap();
        stream.map(|event| event.expect("no run error")).collect().await
    }

    async fn pending(&self) -> Option<CodeActCheckpoint> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: "user".to_string(),
                session_id: SESSION.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        session.state().get(PENDING_STATE_KEY).and_then(|value| serde_json::from_value(value).ok())
    }

    /// Writes `checkpoint` back to session state, as a crash before the next
    /// checkpoint landed would leave it.
    async fn restore(&self, checkpoint: &CodeActCheckpoint) {
        let mut event = Event::new("restore");
        event.author = "test".to_string();
        event
            .actions
            .state_delta
            .insert(PENDING_STATE_KEY.to_string(), serde_json::to_value(checkpoint).unwrap());
        self.sessions.append_event(SESSION, event).await.unwrap();
    }
}

fn final_value(events: &[Event]) -> Value {
    let text: String = events
        .last()
        .and_then(|event| event.llm_response.content.as_ref())
        .map(|content| content.parts.iter().filter_map(|part| part.text()).collect())
        .unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

#[tokio::test]
async fn identical_calls_in_one_python_program_each_execute() {
    let ledger = Arc::new(InMemoryActionLedger::new());
    let harness = harness(
        &["a = call_tool(\"charge\", {\"amount\": 5})\n\
           b = call_tool(\"charge\", {\"amount\": 5})\n\
           {\"type\": \"final_result\", \"value\": [a[\"key\"], b[\"key\"]]}"],
        false,
    )
    .await;

    let events = harness
        .run("charge me twice", RunConfig::builder().action_ledger(ledger.clone()).build())
        .await;

    let keys = harness.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 2, "both calls execute");
    assert_ne!(keys[0], keys[1], "each call has its own idempotency key");
    assert_eq!(final_value(&events), json!(keys));
    for key in &keys {
        let record = ledger.get(key).await.unwrap().expect("a ledger record");
        assert!(record.is_completed());
    }
}

#[tokio::test]
async fn a_confirmed_python_call_executes_once_when_its_resume_is_replayed() {
    let ledger = Arc::new(InMemoryActionLedger::new());
    let harness = harness(
        &[
            "r = call_tool(\"charge\", {\"amount\": 5})\n{\"type\": \"final_result\", \"value\": r}",
            // The replayed call raises its ledger answer; the model then finishes.
            "{\"type\": \"final_result\", \"value\": \"already charged\"}",
        ],
        true,
    )
    .await;

    // Run 1 holds the call and persists the live Python continuation.
    harness.run("charge me", RunConfig::builder().action_ledger(ledger.clone()).build()).await;
    let held = harness.pending().await.expect("a held checkpoint");
    assert_eq!(held.disposition, Disposition::AwaitingConfirmation);
    assert!(harness.keys.lock().unwrap().is_empty());

    let approved = || {
        let mut config = RunConfig::builder().action_ledger(ledger.clone()).build();
        config.tool_confirmation_decisions =
            HashMap::from([(held.call.call_id.to_string(), ToolConfirmationDecision::Approve)]);
        config
    };

    // Run 2 approves: the snapshot resumes and the call executes once.
    let events = harness.run("", approved()).await;
    let keys = harness.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 1);
    assert_eq!(final_value(&events), json!({ "charged": 5, "key": keys[0] }));
    assert!(harness.pending().await.is_none());

    // Run 3 replays the same approved resume, as a crash before its SAVE-AFTER
    // checkpoint landed would: the ledger answers and the card is not charged again.
    harness.restore(&held).await;
    let events = harness.run("", approved()).await;
    assert_eq!(harness.keys.lock().unwrap().len(), 1, "the replayed call does not execute");
    assert_eq!(final_value(&events), json!("already charged"));
    let record = ledger.get(&keys[0]).await.unwrap().expect("a ledger record");
    assert!(record.is_completed());
}
