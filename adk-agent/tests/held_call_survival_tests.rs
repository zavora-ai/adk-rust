//! A call held for confirmation survives what happens to the session while it waits, and
//! runs exactly once when its decision arrives.
//!
//! - Session compaction never covers it, with any overlap.
//! - A workflow agent stops at it, and the resume starts at the sub-agent that holds it:
//!   sub-agents that finished in that turn do not run again.
//! - A provider that sends calls without IDs gets generated ones, so two calls to one tool
//!   keep their own arguments and the IDs never reach the provider.
//! - A shared run budget counts it once.
//!
//! Every test drives real `LlmAgent`s through a `Runner` with scripted models and counts
//! model calls per agent and tool executions.

use adk_agent::{
    ConditionalAgent, LlmAgentBuilder, LlmConditionalAgent, LoopAgent, ParallelAgent,
    SequentialAgent,
};
use adk_core::{
    Agent, BaseEventsSummarizer, BudgetTracker, Content, Event, EventActions, EventCompaction,
    EventsCompactionConfig, GENERATED_CALL_ID_PREFIX, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunBudget, RunConfig, SessionId, Tool, ToolApproval,
    ToolConfirmationRequest, ToolContext, UserId, held_tool_calls,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// --- Harness ---

type Script = Box<dyn Fn(&LlmRequest) -> Vec<Part> + Send + Sync>;

/// Answers each request from a script and records every request.
struct ScriptedModel {
    script: Script,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(script: impl Fn(&LlmRequest) -> Vec<Part> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { script: Box::new(script), requests: Mutex::new(Vec::new()) })
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let parts = (self.script)(&req);
        self.requests.lock().unwrap().push(req);
        let response = LlmResponse {
            content: Some(Content { role: "model".to_string(), parts }),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn text(text: &str) -> Vec<Part> {
    vec![Part::Text { text: text.to_string() }]
}

fn function_call(name: &str, args: Value, id: Option<&str>) -> Part {
    Part::FunctionCall {
        name: name.to_string(),
        args,
        id: id.map(str::to_string),
        thought_signature: None,
    }
}

fn ends_on_tool_result(request: &LlmRequest) -> bool {
    request.contents.last().is_some_and(|content| {
        content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
    })
}

fn last_user_text(request: &LlmRequest) -> String {
    request
        .contents
        .iter()
        .rev()
        .filter(|content| content.role == "user")
        .flat_map(|content| &content.parts)
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default()
}

/// Calls `transfer` when the user asks to send money, and answers its result with text.
fn transferring() -> Arc<ScriptedModel> {
    let issued = AtomicUsize::new(0);
    ScriptedModel::new(move |request| {
        if ends_on_tool_result(request) {
            return text("transfer handled");
        }
        if !last_user_text(request).contains("send") {
            return text("nothing to send");
        }
        let n = issued.fetch_add(1, Ordering::SeqCst) + 1;
        vec![function_call("transfer", json!({ "amount": 10 }), Some(&format!("call-{n}")))]
    })
}

fn talking(reply: &'static str) -> Arc<ScriptedModel> {
    ScriptedModel::new(move |_| text(reply))
}

/// Records the arguments of every execution.
struct Transfer {
    executions: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for Transfer {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "moves money"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "status": "sent" }))
    }
}

type Executions = Arc<Mutex<Vec<Value>>>;

fn holder(name: &str, model: Arc<ScriptedModel>, executions: &Executions) -> Arc<dyn Agent> {
    Arc::new(
        LlmAgentBuilder::new(name)
            .model(model)
            .tool(Arc::new(Transfer { executions: executions.clone() }))
            .require_tool_confirmation("transfer")
            .build()
            .unwrap(),
    )
}

fn talker(name: &str, model: Arc<ScriptedModel>) -> Arc<dyn Agent> {
    Arc::new(LlmAgentBuilder::new(name).model(model).build().unwrap())
}

/// Summarizes any window into one line, ending where the window ends.
struct OneLineSummarizer;

#[async_trait]
impl BaseEventsSummarizer for OneLineSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        let (Some(first), Some(last)) = (events.first(), events.last()) else {
            return Ok(None);
        };
        let mut event = Event::new("compaction");
        event.author = "system".to_string();
        event.actions = EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: first.timestamp,
                end_timestamp: last.timestamp,
                compacted_content: Content::new("model").with_text("summary"),
            }),
            ..Default::default()
        };
        Ok(Some(event))
    }
}

struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

async fn harness(agent: Arc<dyn Agent>, compaction_overlap: Option<u32>) -> Harness {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "held".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name("held")
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>);
    if let Some(overlap) = compaction_overlap {
        builder = builder.compaction_config(EventsCompactionConfig {
            compaction_interval: 1,
            overlap_size: overlap,
            summarizer: Arc::new(OneLineSummarizer),
        });
    }
    Harness { runner: builder.build().unwrap(), sessions }
}

impl Harness {
    async fn run(&self, text: &str, config: Option<RunConfig>) {
        let stream = self
            .runner
            .run_with_config(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                Content::new("user").with_text(text),
                config,
            )
            .await
            .unwrap();
        let events = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish");
        for event in events {
            event.expect("no run error");
        }
    }

    async fn events(&self) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: "held".to_string(),
                user_id: "user".to_string(),
                session_id: "session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all()
    }

    /// Every confirmation request saved so far, compacted or not.
    async fn requests(&self) -> Vec<ToolConfirmationRequest> {
        self.events()
            .await
            .into_iter()
            .filter_map(|event| event.actions.tool_confirmation)
            .collect()
    }

    /// A run config approving every call held so far.
    async fn approving_all(&self) -> RunConfig {
        approving(&self.requests().await)
    }
}

fn approving(requests: &[ToolConfirmationRequest]) -> RunConfig {
    requests
        .iter()
        .fold(RunConfig::builder(), |builder, request| {
            builder.tool_approval(request.fingerprint(), ToolApproval::approve())
        })
        .build()
}

/// Function-response IDs that appear more than once in `request`.
fn duplicated_responses(request: &LlmRequest) -> Vec<String> {
    let mut seen = HashMap::<String, usize>::new();
    for part in request.contents.iter().flat_map(|content| &content.parts) {
        if let Part::FunctionResponse { id: Some(id), .. } = part {
            *seen.entry(id.clone()).or_default() += 1;
        }
    }
    seen.into_iter().filter(|(_, count)| *count > 1).map(|(id, _)| id).collect()
}

// --- Compaction ---

async fn held_call_survives_compaction(overlap: u32, message_while_held: bool) {
    let model = transferring();
    let executions = Executions::default();
    let harness = harness(holder("ops", model.clone(), &executions), Some(overlap)).await;

    harness.run("send 10", None).await;
    if message_while_held {
        harness.run("what is the status?", None).await;
    }
    let events = harness.events().await;
    assert!(
        events.iter().any(|event| event.actions.compaction.is_some()),
        "compaction ran before the approval"
    );
    assert_eq!(held_tool_calls(&events, "ops", "").len(), 1, "the call is still held");

    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 10 })]);
    assert!(held_tool_calls(&harness.events().await, "ops", "").is_empty());
    let last = model.requests().pop().unwrap();
    let results: Vec<Value> = last
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(results, vec![json!({ "status": "sent" })], "the model sees the resumed result");
}

#[tokio::test]
async fn a_held_call_survives_compaction_right_after_its_run() {
    held_call_survives_compaction(0, false).await;
}

#[tokio::test]
async fn a_held_call_survives_compaction_after_a_later_message() {
    held_call_survives_compaction(1, true).await;
}

// --- Workflows ---

#[tokio::test]
async fn a_sequential_workflow_stops_at_a_hold_and_resumes_at_the_holder() {
    let (intake, ops, report) = (talking("noted"), transferring(), talking("report"));
    let executions = Executions::default();
    let pipeline = SequentialAgent::new(
        "pipeline",
        vec![
            talker("intake", intake.clone()),
            holder("ops", ops.clone(), &executions),
            talker("report", report.clone()),
        ],
    );
    let harness = harness(Arc::new(pipeline), None).await;

    harness.run("send 10", None).await;
    assert_eq!((intake.calls(), ops.calls(), report.calls()), (1, 1, 0), "stops at the hold");

    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!(
        (intake.calls(), ops.calls(), report.calls()),
        (1, 2, 1),
        "resumes at ops; intake does not run again"
    );
    assert_eq!(executions.lock().unwrap().len(), 1);
    let report_request = report.requests().pop().unwrap();
    assert!(
        duplicated_responses(&report_request).is_empty(),
        "a later sub-agent sees one response per call: {:?}",
        report_request.contents
    );
}

#[tokio::test]
async fn a_workflow_whose_holder_ran_without_a_decision_starts_over_next_time() {
    let (intake, ops, report) = (talking("noted"), transferring(), talking("report"));
    let executions = Executions::default();
    let pipeline = SequentialAgent::new(
        "pipeline",
        vec![
            talker("intake", intake.clone()),
            holder("ops", ops.clone(), &executions),
            talker("report", report.clone()),
        ],
    );
    let harness = harness(Arc::new(pipeline), None).await;

    harness.run("send 10", None).await;
    // No decision: the paused turn continues at ops, whose model answers, and report runs.
    harness.run("", None).await;
    assert_eq!((intake.calls(), report.calls()), (1, 1));
    assert!(executions.lock().unwrap().is_empty());
    // The turn finished, so the next message runs the whole workflow again.
    harness.run("next", None).await;
    assert_eq!((intake.calls(), report.calls()), (2, 2));
}

#[tokio::test]
async fn a_loop_resumes_in_the_iteration_that_held_and_runs_its_remaining_iterations() {
    let counter = talking("counted");
    let fresh = AtomicUsize::new(0);
    let looper = ScriptedModel::new(move |request| {
        if ends_on_tool_result(request) {
            return text("transfer handled");
        }
        match fresh.fetch_add(1, Ordering::SeqCst) + 1 {
            2 => vec![function_call("transfer", json!({ "amount": 10 }), Some("call-loop"))],
            n => text(&format!("round {n}")),
        }
    });
    let executions = Executions::default();
    let rounds = LoopAgent::new(
        "rounds",
        vec![talker("counter", counter.clone()), holder("looper", looper.clone(), &executions)],
    )
    .with_max_iterations(3);
    let harness = harness(Arc::new(rounds), None).await;

    harness.run("go", None).await;
    assert_eq!((counter.calls(), looper.calls()), (2, 2), "stops in round 2");

    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!(
        (counter.calls(), looper.calls()),
        (3, 4),
        "finishes round 2 from the holder, then runs round 3 only"
    );
    assert_eq!(executions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_parallel_workflow_resumes_only_the_held_branch() {
    let (fast, ops, report) = (talking("fast"), transferring(), talking("report"));
    let executions = Executions::default();
    let both = ParallelAgent::new(
        "both",
        vec![talker("fast", fast.clone()), holder("ops", ops.clone(), &executions)],
    );
    let fanout =
        SequentialAgent::new("fanout", vec![Arc::new(both), talker("report", report.clone())]);
    let harness = harness(Arc::new(fanout), None).await;

    harness.run("send 10", None).await;
    assert_eq!((fast.calls(), ops.calls(), report.calls()), (1, 1, 0));

    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!((fast.calls(), ops.calls(), report.calls()), (1, 2, 1));
    assert_eq!(executions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_conditional_agent_resumes_its_branch_without_evaluating_the_condition_again() {
    let (ops, other) = (transferring(), talking("other"));
    let executions = Executions::default();
    let evaluations = Arc::new(AtomicUsize::new(0));
    let counted = evaluations.clone();
    // True only on its first evaluation.
    let router = ConditionalAgent::new(
        "router",
        move |_ctx| counted.fetch_add(1, Ordering::SeqCst) == 0,
        holder("ops", ops.clone(), &executions),
    )
    .with_else(talker("other", other.clone()));
    let harness = harness(Arc::new(router), None).await;

    harness.run("send 10", None).await;
    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);
    assert_eq!((ops.calls(), other.calls()), (2, 0));
    assert_eq!(executions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_llm_router_resumes_its_route_without_classifying_again() {
    let classifier = ScriptedModel::new(|request| {
        let asked =
            request.contents.iter().flat_map(|content| &content.parts).any(
                |part| matches!(part, Part::Text { text } if text.contains("User input: send")),
            );
        text(if asked { "payments" } else { "general" })
    });
    let (ops, general) = (transferring(), talking("general"));
    let executions = Executions::default();
    let router = LlmConditionalAgent::builder("router", classifier.clone())
        .instruction("Classify as 'payments' or 'general'.")
        .route("payments", holder("ops", ops.clone(), &executions))
        .route("general", talker("general", general.clone()))
        .build()
        .unwrap();
    let harness = harness(Arc::new(router), None).await;

    harness.run("send 10", None).await;
    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!((classifier.calls(), ops.calls(), general.calls()), (1, 2, 0));
    assert_eq!(executions.lock().unwrap().len(), 1);
}

// --- Calls without IDs ---

#[tokio::test]
async fn two_calls_without_ids_keep_their_arguments_and_one_approval_runs_one() {
    let model = ScriptedModel::new(|request| {
        if ends_on_tool_result(request) {
            return text("handled");
        }
        vec![
            function_call("transfer", json!({ "amount": 10 }), None),
            function_call("transfer", json!({ "amount": 20 }), None),
        ]
    });
    let executions = Executions::default();
    let harness = harness(holder("ops", model.clone(), &executions), None).await;

    harness.run("send 10 and 20", None).await;
    let held = held_tool_calls(&harness.events().await, "ops", "");
    let amounts: Vec<Value> = held.iter().map(|call| call.args["amount"].clone()).collect();
    assert_eq!(amounts, vec![json!(10), json!(20)]);
    assert!(
        held.iter().all(|call| call
            .id
            .as_deref()
            .is_some_and(|id| id.starts_with(GENERATED_CALL_ID_PREFIX))),
        "{held:?}"
    );

    let ten: Vec<_> = harness
        .requests()
        .await
        .into_iter()
        .filter(|request| request.args["amount"] == 10)
        .collect();
    harness.run("", Some(approving(&ten))).await;
    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 10 })]);
    let still_held: Vec<Value> = held_tool_calls(&harness.events().await, "ops", "")
        .iter()
        .map(|call| call.args["amount"].clone())
        .collect();
    assert_eq!(still_held, vec![json!(20)]);

    // The provider never receives an ID it did not issue, and sees one response per call.
    let resumed = model.requests().pop().unwrap();
    let ids: Vec<Option<String>> = resumed
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionCall { id, .. } | Part::FunctionResponse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![None, None, None, None], "{:?}", resumed.contents);
    let responses: Vec<Value> = resumed
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(responses[0], json!({ "status": "sent" }), "call 1 answered in its own place");
    assert!(adk_core::is_confirmation_pending(&responses[1]), "call 2 still pending");
}

#[tokio::test]
async fn an_agent_tool_call_without_an_id_resumes_its_delegates_held_call() {
    let desk_model = ScriptedModel::new(|request| {
        if ends_on_tool_result(request) {
            return text("the helper finished");
        }
        vec![function_call("helper", json!({ "request": "send 10" }), None)]
    });
    let executions = Executions::default();
    let helper = holder("helper", transferring(), &executions);
    let desk = LlmAgentBuilder::new("desk")
        .model(desk_model.clone())
        .tool(Arc::new(adk_tool::AgentTool::new(helper)))
        .build()
        .unwrap();
    let harness = harness(Arc::new(desk), None).await;

    harness.run("send 10", None).await;
    let held = held_tool_calls(&harness.events().await, "desk", "");
    assert_eq!(held.len(), 1);
    assert_eq!((held[0].name.as_str(), held[0].request.tool_name.as_str()), ("helper", "transfer"));

    let config = harness.approving_all().await;
    harness.run("", Some(config)).await;
    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 10 })]);
    assert!(held_tool_calls(&harness.events().await, "desk", "").is_empty());
    let last = desk_model.requests().pop().unwrap();
    assert!(ends_on_tool_result(&last), "desk's model sees the delegation's result");
}

// --- Budget ---

#[tokio::test]
async fn a_held_then_resumed_call_counts_once_against_a_shared_budget() {
    let executions = Executions::default();
    let harness = harness(holder("ops", transferring(), &executions), None).await;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_tool_calls(1)));
    let first = RunConfig { budget_tracker: Some(tracker.clone()), ..Default::default() };

    harness.run("send 10", Some(first)).await;
    assert_eq!(tracker.usage().tool_calls, 0, "a held call has not run");

    let mut resume = harness.approving_all().await;
    resume.budget_tracker = Some(tracker.clone());
    harness.run("", Some(resume)).await;
    assert_eq!(executions.lock().unwrap().len(), 1);
    assert_eq!(tracker.usage().tool_calls, 1);
}
