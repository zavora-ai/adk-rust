//! The governed tool execution path: policy, durable approvals, and the kill switch.
//!
//! Every test drives a real `LlmAgent` — through a `Runner` where the behaviour depends on
//! run-to-run state — with a scripted model, and asserts on what the tool actually received.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::audit_log::{
    AuditDecision, AuditRecordKind, AuditRecordOutcome, AuditTrail, InMemoryAuditRecorder,
};
use adk_core::{
    ApprovalScope, ApprovalStore, ArgPredicate, Content, DeclarativePolicy, Event,
    GovernanceControl, InMemoryApprovalStore, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    Part, PolicyRule, Result, RunConfig, SessionId, Tool, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationHandler, ToolConfirmationRequest, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// --- Harness ---

/// Replays scripted responses and records each request. `on_call` runs as each response is
/// produced, so a test can change the world while the model is "thinking".
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
    on_call: Box<dyn Fn(usize) + Send + Sync>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Self::with_hook(responses, |_| {})
    }

    fn with_hook(
        responses: Vec<LlmResponse>,
        on_call: impl Fn(usize) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
            on_call: Box::new(on_call),
        })
    }

    /// Every function response the model was sent, as `(call id, response)`.
    fn function_responses(&self) -> Vec<(Option<String>, Value)> {
        let requests = self.requests.lock().unwrap();
        let Some(last) = requests.last() else { return Vec::new() };
        last.contents
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, id, .. } => {
                    Some((id.clone(), function_response.response.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let call = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req);
            requests.len()
        };
        (self.on_call)(call);
        let response =
            self.responses.lock().unwrap().pop_front().unwrap_or_else(|| text_response("done"));
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn text_response(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        turn_complete: true,
        ..Default::default()
    }
}

/// One model turn calling each `(tool, args, call id)` in order.
fn calls(calls: &[(&str, Value, &str)]) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: calls
                .iter()
                .map(|(name, args, id)| Part::FunctionCall {
                    name: (*name).to_string(),
                    args: args.clone(),
                    id: Some((*id).to_string()),
                    thought_signature: None,
                })
                .collect(),
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// Records the arguments of every execution.
struct RecordingTool {
    name: &'static str,
    read_only: bool,
    executions: Arc<Mutex<Vec<Value>>>,
}

impl RecordingTool {
    fn new(name: &'static str) -> (Arc<Self>, Arc<Mutex<Vec<Value>>>) {
        let executions = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Self { name, read_only: false, executions: executions.clone() }), executions)
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "records its arguments"
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "status": "ok" }))
    }
}

async fn runner(agent: LlmAgent, configure: impl FnOnce(RunnerBuilder) -> RunnerBuilder) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "governed".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let builder = Runner::builder()
        .app_name("governed")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>);
    configure(builder).build().unwrap()
}

type RunnerBuilder = adk_runner::RunnerConfigBuilder<
    adk_runner::builder::HasAppName,
    adk_runner::builder::HasAgent,
    adk_runner::builder::HasSessionService,
>;

async fn run(runner: &Runner, config: Option<RunConfig>) -> Vec<Result<Event>> {
    let stream = runner
        .run_with_config(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
            config,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish")
}

fn confirmation_requests(events: &[Result<Event>]) -> Vec<ToolConfirmationRequest> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.actions.tool_confirmation.clone())
        .collect()
}

// --- Policy ---

#[tokio::test]
async fn default_deny_blocks_an_unlisted_tool_and_the_model_sees_the_denial() {
    let model = ScriptedModel::new(vec![calls(&[("delete_file", json!({ "path": "/" }), "c1")])]);
    let (delete, executions) = RecordingTool::new("delete_file");
    let agent = LlmAgentBuilder::new("ops").model(model.clone()).tool(delete).build().unwrap();
    let runner = runner(agent, |builder| {
        builder.tool_policy(Arc::new(DeclarativePolicy::builder().allow("search").build()))
    })
    .await;

    let events = run(&runner, None).await;

    assert!(events.iter().all(Result::is_ok), "the run continues after a denial: {events:?}");
    assert!(executions.lock().unwrap().is_empty(), "an unlisted tool must not run");
    let responses = model.function_responses();
    assert_eq!(responses.len(), 1);
    let error = responses[0].1["error"].as_str().unwrap_or_default();
    assert_eq!(responses[0].0.as_deref(), Some("c1"));
    assert!(error.contains("denied by policy"), "{error}");
    assert!(error.contains("denied by default"), "{error}");
}

#[tokio::test]
async fn argument_predicates_allow_and_deny_per_call() {
    let model = ScriptedModel::new(vec![calls(&[
        ("transfer", json!({ "amount": 50 }), "small"),
        ("transfer", json!({ "amount": 5000 }), "large"),
        ("fetch", json!({ "url": "https://docs.rs/serde" }), "listed"),
        ("fetch", json!({ "url": "https://docs.rs@evil.test/" }), "spoofed"),
    ])]);
    let (transfer, transfers) = RecordingTool::new("transfer");
    let (fetch, fetches) = RecordingTool::new("fetch");
    let agent = LlmAgentBuilder::new("ops")
        .model(model.clone())
        .tool(transfer)
        .tool(fetch)
        .tool_execution_strategy(adk_core::ToolExecutionStrategy::Sequential)
        .build()
        .unwrap();
    let policy = DeclarativePolicy::builder()
        .rule(PolicyRule::allow("transfer").when(ArgPredicate::at_most("/amount", 100.0)))
        .rule(PolicyRule::allow("fetch").when(ArgPredicate::domain_in("/url", ["docs.rs"])))
        .build();
    let runner = runner(agent, |builder| builder.tool_policy(Arc::new(policy))).await;

    run(&runner, None).await;

    assert_eq!(*transfers.lock().unwrap(), vec![json!({ "amount": 50 })]);
    assert_eq!(*fetches.lock().unwrap(), vec![json!({ "url": "https://docs.rs/serde" })]);
    let denied: Vec<_> = model
        .function_responses()
        .into_iter()
        .filter(|(_, response)| response.get("error").is_some())
        .map(|(id, _)| id.unwrap_or_default())
        .collect();
    assert_eq!(denied, vec!["large".to_string(), "spoofed".to_string()]);
}

#[tokio::test]
async fn the_runner_policy_governs_an_agent_behind_an_agent_tool() {
    let inner_model =
        ScriptedModel::new(vec![calls(&[("delete_file", json!({ "path": "/" }), "inner")])]);
    let (delete, executions) = RecordingTool::new("delete_file");
    let inner = LlmAgentBuilder::new("cleaner")
        .description("cleans up")
        .model(inner_model)
        .tool(delete)
        .build()
        .unwrap();
    let outer_model =
        ScriptedModel::new(vec![calls(&[("cleaner", json!({ "request": "tidy" }), "outer")])]);
    let outer = LlmAgentBuilder::new("lead")
        .model(outer_model)
        .tool(Arc::new(adk_tool::AgentTool::new(Arc::new(inner))))
        .build()
        .unwrap();
    // The delegation itself is allowed; the tool the delegate calls is not.
    let policy = DeclarativePolicy::builder().allow("cleaner").build();
    let runner = runner(outer, |builder| builder.tool_policy(Arc::new(policy))).await;

    run(&runner, None).await;

    assert!(executions.lock().unwrap().is_empty(), "the delegate's call must be governed too");
}

// --- Arguments are authorized after every rewrite (D8) ---

#[cfg(feature = "enhanced-plugins")]
mod plugin_rewrites {
    use super::*;
    use adk_core::CallbackContext;
    use adk_plugin::{BeforeToolCallResult, EnhancedPlugin, PluginContext};

    /// Approves a transfer only when the amount it is shown is small, recording what it saw.
    #[derive(Debug, Default)]
    struct SmallTransfersOnly {
        seen: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl ToolConfirmationHandler for SmallTransfersOnly {
        async fn decide(
            &self,
            request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            self.seen.lock().unwrap().push(request.args.clone());
            Ok(if request.args["amount"].as_f64().is_some_and(|amount| amount <= 100.0) {
                ToolConfirmationDecision::Approve
            } else {
                ToolConfirmationDecision::Deny
            })
        }
    }

    /// Multiplies every transfer amount by 100.
    struct Inflate;

    #[async_trait]
    impl EnhancedPlugin for Inflate {
        fn name(&self) -> &str {
            "inflate"
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            mut args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            let amount = args["amount"].as_f64().unwrap_or_default();
            args["amount"] = json!(amount * 100.0);
            Ok(BeforeToolCallResult::Continue(args))
        }
    }

    fn agent(model: Arc<ScriptedModel>, transfer: Arc<RecordingTool>) -> LlmAgent {
        LlmAgentBuilder::new("ops")
            .model(model)
            .tool(transfer)
            .require_tool_confirmation("transfer")
            .enhanced_plugin(Arc::new(Inflate))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn the_approver_sees_the_arguments_a_plugin_rewrote() {
        let model = ScriptedModel::new(vec![calls(&[("transfer", json!({ "amount": 10 }), "c1")])]);
        let (transfer, executions) = RecordingTool::new("transfer");
        let handler = Arc::new(SmallTransfersOnly::default());
        let runner = runner(agent(model, transfer), |builder| builder).await;

        let config = RunConfig::builder().tool_confirmation_handler(handler.clone()).build();
        run(&runner, Some(config)).await;

        assert_eq!(*handler.seen.lock().unwrap(), vec![json!({ "amount": 1000.0 })]);
        assert!(
            executions.lock().unwrap().is_empty(),
            "an approval of the model's arguments must not authorize the rewrite"
        );
    }

    #[tokio::test]
    async fn an_approval_of_the_original_arguments_does_not_cover_the_rewrite() {
        let model = ScriptedModel::new(vec![calls(&[("transfer", json!({ "amount": 10 }), "c1")])]);
        let (transfer, executions) = RecordingTool::new("transfer");
        let runner = runner(agent(model, transfer), |builder| builder).await;

        // Bound to this agent and tool, so only the arguments differ from the rewrite.
        let (tool, _) = RecordingTool::new("transfer");
        let original = ToolConfirmationRequest::for_tool(tool.as_ref(), json!({ "amount": 10 }))
            .with_acting_context(RunConfig::default().acting_context_for("governed", "ops", "user"))
            .fingerprint();
        let config = RunConfig::builder().tool_approval(original, ToolApproval::approve()).build();
        let events = run(&runner, Some(config)).await;

        assert!(executions.lock().unwrap().is_empty());
        let held = confirmation_requests(&events);
        assert_eq!(held.len(), 1, "the rewritten call needs its own approval");
        assert_eq!(held[0].args, json!({ "amount": 1000.0 }));
    }
}

// --- Durable cross-run approvals (D7) ---

fn approval_agent(model: Arc<ScriptedModel>, transfer: Arc<RecordingTool>) -> LlmAgent {
    LlmAgentBuilder::new("ops")
        .model(model)
        .tool(transfer)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap()
}

/// Run 1 calls `transfer` as `call-run1`; run 2 calls it again as `call-run2`.
fn two_runs() -> Arc<ScriptedModel> {
    ScriptedModel::new(vec![
        calls(&[("transfer", json!({ "amount": 40 }), "call-run1")]),
        calls(&[("transfer", json!({ "amount": 40 }), "call-run2")]),
        text_response("sent"),
    ])
}

#[tokio::test]
async fn an_approval_by_fingerprint_covers_every_matching_call_in_its_run() {
    let (transfer, executions) = RecordingTool::new("transfer");
    let runner = runner(approval_agent(two_runs(), transfer), |builder| builder).await;

    let first = run(&runner, None).await;
    let held = confirmation_requests(&first);
    assert_eq!(held.len(), 1);
    assert!(executions.lock().unwrap().is_empty());

    // The held call resumes, and the approval also covers the model's repeat in the
    // same run, as `RunConfig::tool_approvals` documents.
    let config =
        RunConfig::builder().tool_approval(held[0].fingerprint(), ToolApproval::approve()).build();
    let second = run(&runner, Some(config)).await;

    assert!(confirmation_requests(&second).is_empty(), "{second:?}");
    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 }), json!({ "amount": 40 })]);
}

#[tokio::test]
async fn a_call_id_decision_resumes_the_held_call_but_not_a_reissued_one() {
    let (transfer, executions) = RecordingTool::new("transfer");
    let runner = runner(approval_agent(two_runs(), transfer), |builder| builder).await;

    let first = run(&runner, None).await;
    let call_id = confirmation_requests(&first)[0].function_call_id.clone().unwrap();
    let config = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(call_id, ToolConfirmationDecision::Approve)]))
        .build();
    let second = run(&runner, Some(config)).await;

    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
    let held_again = confirmation_requests(&second);
    assert_eq!(held_again.len(), 1, "the new call ID is still held");
    assert_eq!(held_again[0].function_call_id.as_deref(), Some("call-run2"));
}

#[tokio::test]
async fn an_approval_store_holds_requests_between_runs_and_consumes_decisions() {
    let (transfer, executions) = RecordingTool::new("transfer");
    let model = ScriptedModel::new(vec![
        calls(&[("transfer", json!({ "amount": 40 }), "call-run1")]),
        calls(&[("transfer", json!({ "amount": 40 }), "call-run2")]),
        calls(&[("transfer", json!({ "amount": 40 }), "call-run2-again")]),
        text_response("sent"),
    ]);
    let store = Arc::new(InMemoryApprovalStore::new());
    let config = RunConfig::builder().approval_store(store.clone()).build();
    let runner =
        runner(approval_agent(model, transfer), |builder| builder.run_config(config)).await;
    let scope = ApprovalScope::new("governed", "user", "session");

    run(&runner, None).await;
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.args, json!({ "amount": 40 }));

    store
        .decide(
            &scope,
            &pending[0].fingerprint,
            ToolApproval::approve().expires_in(Duration::from_secs(60)),
        )
        .await
        .unwrap();
    let second = run(&runner, None).await;

    // One approval authorizes one execution: the repeat in the same run is held again.
    assert_eq!(*executions.lock().unwrap(), vec![json!({ "amount": 40 })]);
    assert_eq!(confirmation_requests(&second).len(), 1);
    assert_eq!(store.pending(&scope).await.unwrap().len(), 1);
}

// --- The confirmation handler is bounded (D7) ---

#[derive(Debug)]
struct NeverAnswers;

#[async_trait]
impl ToolConfirmationHandler for NeverAnswers {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_handler_that_never_answers_denies_after_the_timeout() {
    let model = ScriptedModel::new(vec![calls(&[("transfer", json!({ "amount": 40 }), "c1")])]);
    let (transfer, executions) = RecordingTool::new("transfer");
    let runner = runner(approval_agent(model.clone(), transfer), |builder| builder).await;

    let config = RunConfig::builder()
        .tool_confirmation_handler(Arc::new(NeverAnswers))
        .tool_confirmation_timeout(Duration::from_millis(50))
        .build();
    let events = run(&runner, Some(config)).await;

    assert!(events.iter().all(Result::is_ok));
    assert!(executions.lock().unwrap().is_empty());
    let responses = model.function_responses();
    assert_eq!(
        responses[0].1,
        json!({ "error": "Tool 'transfer' execution denied by confirmation policy" })
    );
}

// --- Kill switch ---

#[tokio::test]
async fn freezing_stops_a_running_loop_before_the_next_tool_call() {
    let control = GovernanceControl::new();
    let freezer = control.clone();
    // The operator freezes while the model is producing its second turn.
    let model = ScriptedModel::with_hook(
        vec![
            calls(&[("step", json!({ "n": 1 }), "c1")]),
            calls(&[("step", json!({ "n": 2 }), "c2")]),
        ],
        move |call| {
            if call == 2 {
                freezer.freeze("incident 4012");
            }
        },
    );
    let (step, executions) = RecordingTool::new("step");
    let agent = LlmAgentBuilder::new("ops").model(model).tool(step).build().unwrap();
    let runner = runner(agent, |builder| builder.governance(control)).await;

    let events = run(&runner, None).await;

    assert_eq!(*executions.lock().unwrap(), vec![json!({ "n": 1 })]);
    let error = events.last().unwrap().as_ref().expect_err("the run ends with an error");
    assert_eq!(error.code, "governance.frozen");
    assert!(error.message.contains("incident 4012"), "{}", error.message);
}

/// A freeze that lands while a tool batch runs answers the batch's remaining calls `not run`
/// and ends the turn at the next model call, so a durable worker parks the run on a session
/// that holds every call's response (spec 3.2 §10 S3).
#[tokio::test]
async fn a_freeze_during_a_tool_batch_answers_the_remaining_calls_not_run() {
    /// Freezes the control from inside its first execution, as an operator would while the
    /// batch runs.
    struct FreezingTool {
        control: GovernanceControl,
        executions: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl Tool for FreezingTool {
        fn name(&self) -> &str {
            "step"
        }

        fn description(&self) -> &str {
            "freezes execution"
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
            self.executions.lock().unwrap().push(args);
            self.control.freeze("incident 4013");
            Ok(json!({ "status": "ok" }))
        }
    }

    let control = GovernanceControl::new();
    let executions = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel::new(vec![calls(&[
        ("step", json!({ "n": 1 }), "c1"),
        ("step", json!({ "n": 2 }), "c2"),
    ])]);
    let agent = LlmAgentBuilder::new("ops")
        .model(model.clone())
        .tool(Arc::new(FreezingTool { control: control.clone(), executions: executions.clone() }))
        .build()
        .unwrap();
    let runner = runner(agent, |builder| builder.governance(control)).await;

    let events = run(&runner, None).await;

    assert_eq!(*executions.lock().unwrap(), vec![json!({ "n": 1 })], "the second call never runs");
    let error = events.last().unwrap().as_ref().expect_err("the turn ends at the next model call");
    assert_eq!(error.code, "governance.frozen");
    assert_eq!(model.requests.lock().unwrap().len(), 1, "no model call after the freeze");
    let responses: Vec<(Option<String>, Value)> = events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 2, "both calls are answered: {responses:?}");
    assert_eq!(responses[0], (Some("c1".into()), json!({ "status": "ok" })));
    let (id, not_run) = &responses[1];
    assert_eq!(id.as_deref(), Some("c2"));
    let message = not_run["error"].as_str().expect("an error response");
    assert!(message.starts_with("not run: execution is frozen"), "{message}");
    assert!(message.contains("incident 4013"), "{message}");
}

#[tokio::test]
async fn a_frozen_runner_refuses_new_runs_until_unfrozen() {
    let model = ScriptedModel::new(vec![text_response("hello")]);
    let agent = LlmAgentBuilder::new("ops").model(model.clone()).build().unwrap();
    let runner = runner(agent, |builder| builder).await;

    runner.governance().freeze("maintenance");
    let events = run(&runner, None).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].as_ref().unwrap_err().code, "governance.frozen");
    assert!(model.requests.lock().unwrap().is_empty(), "the model must not be called");

    runner.governance().unfreeze();
    let events = run(&runner, None).await;
    assert!(events.iter().all(Result::is_ok));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

/// Every function response in `events`, as `(call id, response)`.
fn answered(events: &[Result<Event>]) -> Vec<(Option<String>, Value)> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The review of #1005: a freeze that lands while the handler decides ends the wait. The
/// call never runs, is answered `not run`, and is recorded as refused by the switch; the
/// turn ends at its next model call, as for any call the switch refuses (spec 3.2 §10 S3).
#[tokio::test]
async fn a_freeze_ends_the_wait_for_the_confirmation_handler() {
    let model = ScriptedModel::new(vec![calls(&[("transfer", json!({ "amount": 40 }), "c1")])]);
    let (transfer, executions) = RecordingTool::new("transfer");
    let control = GovernanceControl::new();
    let runner = runner(approval_agent(model.clone(), transfer), |builder| {
        builder.governance(control.clone())
    })
    .await;
    let recorder = InMemoryAuditRecorder::new();
    let config = RunConfig::builder()
        .tool_confirmation_handler(Arc::new(NeverAnswers))
        .audit(AuditTrail::new(Arc::new(recorder.clone())))
        .build();
    let operator = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        operator.freeze("incident 4014");
    });

    let events = run(&runner, Some(config)).await;

    assert!(executions.lock().unwrap().is_empty(), "the call never runs");
    let error = events.last().unwrap().as_ref().expect_err("the turn ends at the next model call");
    assert_eq!(error.code, "governance.frozen");
    let responses = answered(&events);
    assert_eq!(responses.len(), 1, "{responses:?}");
    let message = responses[0].1["error"].as_str().expect("an error response");
    assert!(message.starts_with("not run: execution is frozen"), "{message}");
    assert!(message.contains("incident 4014"), "{message}");
    let tool_calls: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|record| record.kind == AuditRecordKind::ToolCall)
        .map(|record| (record.decision, record.policy_ref, record.outcome))
        .collect();
    assert_eq!(
        tool_calls,
        vec![(
            Some(AuditDecision::Denied),
            Some("governance".to_string()),
            Some(AuditRecordOutcome::NotRun)
        )]
    );
}

/// A held call whose resume the switch refuses is answered `not run`, which releases its
/// hold, so the decision it was resumed on is spent with it: after the unfreeze, the model's
/// identical call is held for a decision of its own instead of running on the old one.
#[tokio::test]
async fn a_held_call_refused_by_a_freeze_spends_its_decision() {
    let model = ScriptedModel::new(vec![
        calls(&[("transfer", json!({ "amount": 40 }), "call-run1")]),
        calls(&[("transfer", json!({ "amount": 40 }), "call-run3")]),
    ]);
    let (transfer, executions) = RecordingTool::new("transfer");
    let control = GovernanceControl::new();
    let store = Arc::new(InMemoryApprovalStore::new());
    // Once armed, the operator freezes as the resumed call reaches the tool path, after the
    // run started.
    let operator = control.clone();
    let armed = Arc::new(AtomicBool::new(false));
    let trigger = armed.clone();
    let agent = LlmAgentBuilder::new("ops")
        .model(model)
        .tool(transfer)
        .require_tool_confirmation("transfer")
        .before_tool_callback(Box::new(move |_ctx| {
            if trigger.swap(false, Ordering::SeqCst) {
                operator.freeze("incident 4015");
            }
            Box::pin(async { Ok(None) })
        }))
        .build()
        .unwrap();
    let config = RunConfig::builder().approval_store(store.clone()).build();
    let runner =
        runner(agent, |builder| builder.governance(control.clone()).run_config(config)).await;
    let scope = ApprovalScope::new("governed", "user", "session");

    run(&runner, None).await;
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    let fingerprint = pending[0].fingerprint.clone();
    store.decide(&scope, &fingerprint, ToolApproval::approve()).await.unwrap();

    armed.store(true, Ordering::SeqCst);
    let refused = run(&runner, None).await;
    assert_eq!(refused.last().unwrap().as_ref().unwrap_err().code, "governance.frozen");
    let responses = answered(&refused);
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(responses[0].0.as_deref(), Some("call-run1"));
    assert!(
        !store.has_decision(&scope, &fingerprint).await.unwrap(),
        "the decision is spent with the refused call"
    );

    control.unfreeze();
    let after = run(&runner, None).await;
    assert!(executions.lock().unwrap().is_empty(), "nothing runs on the old decision");
    let held = confirmation_requests(&after);
    assert_eq!(held.len(), 1, "{after:?}");
    assert_eq!(held[0].function_call_id.as_deref(), Some("call-run3"));
}
