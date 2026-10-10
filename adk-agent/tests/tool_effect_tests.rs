//! A tool's declared effect governs retries, and a delegation sets its own timeout.
//!
//! The retry loop repeated any failed call — including one that timed out after its side
//! effect had already happened — so a payment could be charged once per retry. A
//! non-idempotent call now runs at most once, and an agent delegation is bounded by its
//! own timeout rather than the parent's.

use adk_agent::{CustomAgentBuilder, LlmAgentBuilder};
use adk_core::{
    AdkError, Agent, CallbackContext, Content, ErrorCategory, ErrorComponent, Event,
    InvocationContext, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RetryBudget,
    RunConfig, Session, State, Tool, ToolContext, ToolEffect,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ----- harness ---------------------------------------------------------------

struct EmptyState;

impl State for EmptyState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct EmptySession;

impl Session for EmptySession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn state(&self) -> &dyn State {
        &EmptyState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

/// One invocation, `inv-1`, of app `shop` for user `alice` in `session-1`.
struct Invocation {
    user_content: Content,
    run_config: RunConfig,
}

impl Invocation {
    fn with_config(run_config: RunConfig) -> Arc<dyn InvocationContext> {
        Arc::new(Self { user_content: Content::new("user").with_text("buy it"), run_config })
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for Invocation {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "shopper"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for Invocation {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for Invocation {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not used by these tests")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &EmptySession
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

/// Replays scripted responses, then answers "done".
struct ScriptedModel(Mutex<VecDeque<LlmResponse>>);

impl ScriptedModel {
    fn replaying(responses: Vec<LlmResponse>) -> Arc<dyn Llm> {
        Arc::new(Self(Mutex::new(responses.into())))
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self.0.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn call(name: &str, id: Option<&str>) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args: json!({"amount": 50}),
                id: id.map(str::to_string),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

async fn run(agent: &dyn Agent, ctx: Arc<dyn InvocationContext>) -> Vec<Event> {
    let mut stream = agent.run(ctx).await.expect("agent starts");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("agent event"));
    }
    events
}

/// The response payloads, in order, of every function response in `events`.
fn responses(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

fn unavailable() -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "tool.test.down",
        "upstream down",
    )
}

/// Counts executions; fails the first `fail_times` with `error`, and hangs after
/// counting when `hang` is set.
struct CountingTool {
    name: &'static str,
    effect: ToolEffect,
    calls: Arc<AtomicUsize>,
    fail_times: usize,
    error: fn() -> AdkError,
    hang: Option<Duration>,
}

impl CountingTool {
    fn new(name: &'static str, effect: ToolEffect) -> Self {
        Self {
            name,
            effect,
            calls: Arc::new(AtomicUsize::new(0)),
            fail_times: 0,
            error: unavailable,
            hang: None,
        }
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "counts executions"
    }
    fn effect(&self) -> ToolEffect {
        self.effect
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let mut actions = ctx.actions();
        actions.state_delta.insert("attempt".to_string(), json!(attempt));
        if attempt <= self.fail_times {
            actions.escalate = true;
            ctx.set_actions(actions);
            return Err((self.error)());
        }
        ctx.set_actions(actions);
        if let Some(hang) = self.hang {
            tokio::time::sleep(hang).await;
        }
        Ok(json!({ "charged": true, "attempt": attempt }))
    }
}

// ----- retries -----------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_timed_out_payment_never_repeats() {
    let mut pay = CountingTool::new("pay", ToolEffect::NonIdempotent);
    // The charge lands, then the provider never answers.
    pay.hang = Some(Duration::from_secs(3600));
    let calls = pay.calls.clone();
    let agent = LlmAgentBuilder::new("shopper")
        .model(ScriptedModel::replaying(vec![call("pay", Some("pay-1"))]))
        .tool(Arc::new(pay))
        .tool_timeout(Duration::from_secs(300))
        .default_retry_budget(RetryBudget::new(3, Duration::from_millis(10)))
        .build()
        .unwrap();

    let events = run(&agent, Invocation::with_config(RunConfig::default())).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "the payment must execute exactly once");
    let responses = responses(&events);
    assert_eq!(responses.len(), 1);
    assert!(responses[0]["error"].as_str().is_some_and(|e| e.contains("timed out")));
}

#[tokio::test(start_paused = true)]
async fn a_read_only_tool_with_a_retryable_error_is_retried() {
    let mut lookup = CountingTool::new("lookup", ToolEffect::ReadOnly);
    lookup.fail_times = 2;
    let calls = lookup.calls.clone();
    let agent = LlmAgentBuilder::new("shopper")
        .model(ScriptedModel::replaying(vec![call("lookup", Some("lookup-1"))]))
        .tool(Arc::new(lookup))
        .default_retry_budget(RetryBudget::new(3, Duration::from_millis(100)))
        .build()
        .unwrap();

    let events = run(&agent, Invocation::with_config(RunConfig::default())).await;

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(responses(&events), [json!({ "charged": true, "attempt": 3 })]);
}

#[tokio::test(start_paused = true)]
async fn a_non_retryable_error_is_not_retried_even_for_a_read_only_tool() {
    let mut lookup = CountingTool::new("lookup", ToolEffect::ReadOnly);
    lookup.fail_times = 1;
    lookup.error = || AdkError::tool("bad arguments");
    let calls = lookup.calls.clone();
    let agent = LlmAgentBuilder::new("shopper")
        .model(ScriptedModel::replaying(vec![call("lookup", Some("lookup-1"))]))
        .tool(Arc::new(lookup))
        .default_retry_budget(RetryBudget::new(3, Duration::ZERO))
        .build()
        .unwrap();

    run(&agent, Invocation::with_config(RunConfig::default())).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_non_idempotent_tool_with_a_retryable_error_runs_once() {
    let mut pay = CountingTool::new("pay", ToolEffect::NonIdempotent);
    pay.fail_times = 1;
    let calls = pay.calls.clone();
    let agent = LlmAgentBuilder::new("shopper")
        .model(ScriptedModel::replaying(vec![call("pay", Some("pay-1"))]))
        .tool(Arc::new(pay))
        .tool_retry_budget("pay", RetryBudget::new(3, Duration::ZERO))
        .build()
        .unwrap();

    let events = run(&agent, Invocation::with_config(RunConfig::default())).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(responses(&events)[0]["error"].as_str().is_some_and(|e| e.contains("upstream down")));
}

#[tokio::test(start_paused = true)]
async fn a_retried_attempts_state_and_escalation_are_not_committed() {
    let mut sync = CountingTool::new("sync", ToolEffect::Idempotent);
    sync.fail_times = 1;
    let agent = LlmAgentBuilder::new("shopper")
        .model(ScriptedModel::replaying(vec![call("sync", Some("sync-1"))]))
        .tool(Arc::new(sync))
        .default_retry_budget(RetryBudget::new(1, Duration::ZERO))
        .build()
        .unwrap();

    let events = run(&agent, Invocation::with_config(RunConfig::default())).await;

    let tool_event = events
        .iter()
        .find(|event| {
            event.llm_response.content.as_ref().is_some_and(|content| {
                content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
            })
        })
        .expect("the call is answered");
    assert_eq!(tool_event.actions.state_delta.get("attempt"), Some(&json!(2)));
    assert!(!tool_event.actions.escalate, "the failed attempt's escalation leaked");
    // Escalation would have ended the run before the final model turn.
    assert!(
        events.last().unwrap().llm_response.content.as_ref().unwrap().parts[0]
            .text()
            .is_some_and(|text| text == "done")
    );
}

// ----- delegation timeout ------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_twenty_minute_delegation_is_not_cut_at_the_parent_tool_timeout() {
    let researcher = CustomAgentBuilder::new("researcher")
        .description("researches for twenty minutes")
        .handler(|ctx| async move {
            tokio::time::sleep(Duration::from_secs(20 * 60)).await;
            let mut event = Event::new(ctx.invocation_id());
            event.author = "researcher".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("findings"));
            Ok(Box::pin(futures::stream::iter([Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();
    let lead = LlmAgentBuilder::new("lead")
        .model(ScriptedModel::replaying(vec![call("researcher", Some("delegate-1"))]))
        .tool(Arc::new(adk_tool::AgentTool::new(Arc::new(researcher))))
        // The default parent tool timeout, sized for ordinary tool calls.
        .tool_timeout(Duration::from_secs(300))
        .build()
        .unwrap();

    let started = tokio::time::Instant::now();
    let events = run(&lead, Invocation::with_config(RunConfig::default())).await;

    assert!(started.elapsed() >= Duration::from_secs(20 * 60));
    let response = &responses(&events)[0];
    assert!(response.get("error").is_none(), "the delegation was cut short: {response}");
    assert!(response.to_string().contains("findings"), "{response}");
}

#[tokio::test(start_paused = true)]
async fn a_delegation_timeout_comes_from_the_agent_tool() {
    let researcher = CustomAgentBuilder::new("researcher")
        .description("never finishes")
        .handler(|_ctx| async move {
            futures::future::pending::<()>().await;
            Ok(Box::pin(futures::stream::empty()) as adk_core::EventStream)
        })
        .build()
        .unwrap();
    let lead = LlmAgentBuilder::new("lead")
        .model(ScriptedModel::replaying(vec![call("researcher", Some("delegate-1"))]))
        .tool(Arc::new(
            adk_tool::AgentTool::new(Arc::new(researcher)).timeout(Duration::from_secs(45 * 60)),
        ))
        .tool_timeout(Duration::from_secs(300))
        .build()
        .unwrap();

    let started = tokio::time::Instant::now();
    let events = run(&lead, Invocation::with_config(RunConfig::default())).await;

    assert!(started.elapsed() >= Duration::from_secs(45 * 60));
    assert!(responses(&events)[0].to_string().contains("timed out"));
}

#[tokio::test(start_paused = true)]
async fn an_ordinary_tool_is_still_cut_at_the_tool_timeout() {
    let mut slow = CountingTool::new("slow_lookup", ToolEffect::ReadOnly);
    slow.hang = Some(Duration::from_secs(20 * 60));
    let agent = LlmAgentBuilder::new("shopper")
        .model(ScriptedModel::replaying(vec![call("slow_lookup", Some("lookup-1"))]))
        .tool(Arc::new(slow))
        .tool_timeout(Duration::from_secs(300))
        .build()
        .unwrap();

    let started = tokio::time::Instant::now();
    let events = run(&agent, Invocation::with_config(RunConfig::default())).await;

    assert!(started.elapsed() < Duration::from_secs(20 * 60));
    assert!(responses(&events)[0]["error"].as_str().is_some_and(|e| e.contains("timed out")));
}
