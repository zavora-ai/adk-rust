//! What `LlmAgent` tells the model about a tool call that did not return its result normally.
//!
//! - An after-tool stage that fails withholds the result but tells the model the call ran, so a
//!   model that reads an error does not repeat a completed side effect under a new call ID.
//! - A tool error reaches the model as its component, category, and code; the message, which
//!   can quote credentials, stays local unless the agent exposes error messages.
//! - A call whose arguments the provider could not decode is answered without executing.
//! - A sub-second timeout is reported in milliseconds.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ActionLedger, ActionOutcome, AdkError, BackpressurePolicy, Content, ErrorCategory,
    ErrorComponent, Event, InMemoryActionLedger, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    Part, RESULT_WITHHELD_STATUS, Result, RunConfig, SessionId, TOOL_ERROR_MESSAGE_LIMIT,
    TOOL_INVALID_ARGUMENTS_CODE, TOOL_RESULT_ERROR_CATEGORY_KEY, TOOL_RESULT_ERROR_CODE_KEY,
    TOOL_RESULT_STATUS_KEY, Tool, ToolConcurrencyConfig, ToolContext, ToolEffect, ToolResultStatus,
    UserId,
};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SECRET: &str = "postgres://admin:hunter2@db.internal:5432/prod";

/// Replays scripted responses and records each request.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()), requests: Mutex::new(Vec::new()) })
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
        self.requests.lock().unwrap().push(req);
        let response = self.responses.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn call(tool: &str, args: Value) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: tool.to_string(),
                args,
                id: Some("call-1".to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// A tool that counts its executions and returns `result`.
struct CountingTool {
    name: &'static str,
    effect: ToolEffect,
    runs: Arc<AtomicUsize>,
    result: std::result::Result<Value, (ErrorCategory, &'static str, String)>,
    delay: Option<Duration>,
    hold: bool,
}

impl CountingTool {
    fn ok(name: &'static str, result: Value) -> (Self, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let tool = Self {
            name,
            effect: ToolEffect::NonIdempotent,
            runs: runs.clone(),
            result: Ok(result),
            delay: None,
            hold: false,
        };
        (tool, runs)
    }

    fn failing(
        name: &'static str,
        category: ErrorCategory,
        code: &'static str,
        message: &str,
    ) -> (Self, Arc<AtomicUsize>) {
        let (mut tool, runs) = Self::ok(name, Value::Null);
        tool.result = Err((category, code, message.to_string()));
        (tool, runs)
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "A test tool"
    }

    fn effect(&self) -> ToolEffect {
        self.effect
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if self.hold {
            ctx.set_actions(adk_core::EventActions {
                tool_confirmation: Some(
                    adk_core::ToolConfirmationRequest::new("delegate", json!({}))
                        .with_function_call_id("child-1"),
                ),
                ..Default::default()
            });
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        match &self.result {
            Ok(value) => Ok(value.clone()),
            Err((category, code, message)) => {
                Err(AdkError::new(ErrorComponent::Tool, *category, code, message.clone()))
            }
        }
    }
}

struct Run {
    session_events: Vec<Event>,
    error: Option<AdkError>,
}

impl Run {
    fn response(&self) -> &Part {
        self.session_events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| &content.parts)
            .find(
                |part| matches!(part, Part::FunctionResponse { id: Some(id), .. } if id == "call-1"),
            )
            .expect("call-1 is answered")
    }

    /// The response that answers `call-1`, as the session holds it.
    fn answer(&self) -> Value {
        self.session_events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .find_map(|part| match part {
                Part::FunctionResponse { function_response, id: Some(id), .. }
                    if id == "call-1" =>
                {
                    Some(function_response.response.clone())
                }
                _ => None,
            })
            .expect("call-1 is answered")
    }

    fn answer_with(&self, status: ToolResultStatus) -> Value {
        assert_eq!(self.status(), Some(status));
        self.answer()
    }

    fn status(&self) -> Option<ToolResultStatus> {
        self.session_events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| &content.parts)
            .find(
                |part| matches!(part, Part::FunctionResponse {id: Some(id), ..} if id == "call-1"),
            )
            .and_then(Part::tool_result_status)
    }

    fn invocation_id(&self) -> String {
        self.session_events
            .iter()
            .find(|event| event.author != "user")
            .map(|event| event.invocation_id.clone())
            .expect("an agent event")
    }
}

async fn run(
    agent: impl adk_core::Agent + 'static,
    plugin: Option<Plugin>,
    run_config: RunConfig,
) -> Run {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "outcomes".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name("outcomes")
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(run_config);
    if let Some(plugin) = plugin {
        builder = builder.plugin_manager(Arc::new(PluginManager::new(vec![plugin])));
    }
    let runner = builder.build().unwrap();
    let mut stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap();
    let mut error = None;
    while let Some(item) = stream.next().await {
        if let Err(err) = item {
            error = Some(err);
            break;
        }
    }
    let session = sessions
        .get(GetRequest {
            app_name: "outcomes".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    Run { session_events: session.events().all(), error }
}

fn audit_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "audit.sink_unavailable",
        format!("audit sink {SECRET} unavailable"),
    )
}

#[derive(Clone, Copy, Debug)]
enum AfterStage {
    Callback,
    CallbackFull,
    RunnerHook,
    #[cfg(feature = "enhanced-plugins")]
    EnhancedPlugin,
}

#[cfg(feature = "enhanced-plugins")]
struct FailingAfterPlugin(fn() -> AdkError);

#[cfg(feature = "enhanced-plugins")]
#[async_trait]
impl adk_plugin::EnhancedPlugin for FailingAfterPlugin {
    fn name(&self) -> &str {
        "audit"
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        _result: Value,
        _ctx: Arc<dyn adk_core::CallbackContext>,
        _plugin_ctx: &adk_plugin::PluginContext,
    ) -> Result<adk_plugin::AfterToolCallResult> {
        Err((self.0)())
    }
}

fn stages() -> Vec<AfterStage> {
    vec![
        AfterStage::Callback,
        AfterStage::CallbackFull,
        AfterStage::RunnerHook,
        #[cfg(feature = "enhanced-plugins")]
        AfterStage::EnhancedPlugin,
    ]
}

#[tokio::test]
async fn a_failed_after_tool_stage_withholds_the_result_and_says_the_call_ran() {
    for stage in stages() {
        let model = ScriptedModel::new(vec![call("charge_card", json!({"amount": 5}))]);
        let (tool, runs) = CountingTool::ok("charge_card", json!({"charge_id": "ch_raw_result"}));
        let mut builder =
            LlmAgentBuilder::new("payments").model(model.clone()).tool(Arc::new(tool));
        let mut plugin = None;
        match stage {
            AfterStage::Callback => {
                builder = builder
                    .after_tool_callback(Box::new(|_| Box::pin(async { Err(audit_error()) })));
            }
            AfterStage::CallbackFull => {
                builder = builder.after_tool_callback_full(Box::new(|_, _, _, _| {
                    Box::pin(async { Err(audit_error()) })
                }));
            }
            AfterStage::RunnerHook => {
                plugin = Some(Plugin::new(PluginConfig {
                    name: "audit".into(),
                    after_tool: Some(Box::new(|_| Box::pin(async { Err(audit_error()) }))),
                    ..Default::default()
                }));
            }
            #[cfg(feature = "enhanced-plugins")]
            AfterStage::EnhancedPlugin => {
                builder = builder.enhanced_plugin(Arc::new(FailingAfterPlugin(audit_error)));
            }
        }
        let ledger = Arc::new(InMemoryActionLedger::new());
        let config = RunConfig::builder().action_ledger(ledger.clone()).build();
        let outcome = run(builder.build().unwrap(), plugin, config).await;

        assert!(outcome.error.is_none(), "{stage:?}: {:?}", outcome.error);
        assert_eq!(runs.load(Ordering::SeqCst), 1, "{stage:?}");
        let answer = outcome.answer_with(ToolResultStatus::Withheld);
        assert_eq!(
            answer,
            adk_core::result_withheld_response("charge_card", &audit_error(), false),
            "{stage:?}"
        );
        assert_eq!(answer["status"], RESULT_WITHHELD_STATUS);
        let text = answer.to_string();
        assert!(!text.contains("ch_raw_result") && !text.contains("hunter2"), "{stage:?}: {text}");
        // The next model request carries the same answer, so the model is told the call ran.
        let second = &model.requests()[1];
        let sent = serde_json::to_string(&second.contents).unwrap();
        assert!(sent.contains("do not call it again") && !sent.contains("hunter2"), "{stage:?}");

        let key = format!("outcomes/user/session/{}/call-1", outcome.invocation_id());
        let record = ledger.get(&key).await.unwrap().expect("ledger record");
        assert!(
            matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })),
            "{stage:?}: {record:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_after_tool_stage_ends_the_pipeline() {
    let model = ScriptedModel::new(vec![call("charge_card", json!({}))]);
    let (tool, _) = CountingTool::ok("charge_card", json!({"charge_id": "ch_raw_result"}));
    let later = Arc::new(AtomicUsize::new(0));
    let seen = later.clone();
    let agent = LlmAgentBuilder::new("payments")
        .model(model)
        .tool(Arc::new(tool))
        .after_tool_callback(Box::new(|_| Box::pin(async { Err(audit_error()) })))
        // A later stage is handed the unscreened result, so it must not run.
        .after_tool_callback_full(Box::new(move |_, _, _, response| {
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(Some(response)) })
        }))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(later.load(Ordering::SeqCst), 0);
    assert_eq!(outcome.answer_with(ToolResultStatus::Withheld)["status"], RESULT_WITHHELD_STATUS);
}

#[tokio::test]
async fn exposing_error_messages_puts_the_callback_message_in_the_withheld_answer() {
    let model = ScriptedModel::new(vec![call("charge_card", json!({}))]);
    let (tool, _) = CountingTool::ok("charge_card", json!({}));
    let agent = LlmAgentBuilder::new("payments")
        .model(model)
        .tool(Arc::new(tool))
        .after_tool_callback(Box::new(|_| Box::pin(async { Err(audit_error()) })))
        .expose_tool_error_messages(true)
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(
        outcome.answer_with(ToolResultStatus::Withheld),
        adk_core::result_withheld_response("charge_card", &audit_error(), true)
    );
}

#[tokio::test]
async fn a_tool_error_message_stays_out_of_the_model_request_and_the_session() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({"id": "ORD-7"}))]);
    let message = format!("connect {SECRET} failed: connection refused");
    let (tool, runs) = CountingTool::failing(
        "lookup_order",
        ErrorCategory::Unavailable,
        "tool.orders_db.unavailable",
        &message,
    );
    let structured = Arc::new(Mutex::new(None));
    let seen = structured.clone();
    let agent = LlmAgentBuilder::new("support")
        .model(model.clone())
        .tool(Arc::new(tool))
        .on_tool_error(Box::new(move |ctx, _tool, _args, error| {
            let seen = seen.clone();
            Box::pin(async move {
                let outcome = ctx.tool_outcome().expect("the outcome reaches on_tool_error");
                let code = outcome.error.as_ref().map(|error| error.code);
                *seen.lock().unwrap() = Some((code, error));
                Ok(None)
            })
        }))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;

    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        outcome.answer_with(ToolResultStatus::Failed),
        json!({"error": "tool 'lookup_order' failed: tool.unavailable (tool.orders_db.unavailable)"})
    );
    let sent = serde_json::to_string(&model.requests()[1].contents).unwrap();
    assert!(!sent.contains("hunter2") && !sent.contains("db.internal"), "{sent}");
    let stored = serde_json::to_string(&outcome.session_events).unwrap();
    assert!(!stored.contains("hunter2"), "{stored}");
    // The hook still sees the structured error and the full message.
    let (code, full) = structured.lock().unwrap().clone().expect("on_tool_error ran");
    assert_eq!(code, Some("tool.orders_db.unavailable"));
    assert!(full.contains("hunter2"));
}

/// Records the tool outcome an enhanced plugin's `after_tool_call` is handed.
#[cfg(feature = "enhanced-plugins")]
struct OutcomeRecordingPlugin(Arc<Mutex<Option<adk_core::ToolOutcome>>>);

#[cfg(feature = "enhanced-plugins")]
#[async_trait]
impl adk_plugin::EnhancedPlugin for OutcomeRecordingPlugin {
    fn name(&self) -> &str {
        "outcome-recorder"
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        result: Value,
        ctx: Arc<dyn adk_core::CallbackContext>,
        _plugin_ctx: &adk_plugin::PluginContext,
    ) -> Result<adk_plugin::AfterToolCallResult> {
        *self.0.lock().unwrap() = ctx.tool_outcome();
        Ok(adk_plugin::AfterToolCallResult::Continue(result))
    }
}

#[cfg(feature = "enhanced-plugins")]
#[tokio::test]
async fn an_enhanced_plugin_sees_the_full_error_the_response_redacts() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({"id": "ORD-7"}))]);
    let (tool, _) = CountingTool::failing(
        "lookup_order",
        ErrorCategory::Unavailable,
        "tool.orders_db.unavailable",
        &format!("connect {SECRET} failed"),
    );
    let seen = Arc::new(Mutex::new(None));
    let agent = LlmAgentBuilder::new("support")
        .model(model)
        .tool(Arc::new(tool))
        .enhanced_plugin(Arc::new(OutcomeRecordingPlugin(seen.clone())))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;

    assert!(!serde_json::to_string(&outcome.answer()).unwrap().contains("hunter2"));
    let recorded = seen.lock().unwrap().clone().expect("the plugin is handed the outcome");
    assert!(!recorded.success);
    assert_eq!(recorded.error.as_ref().map(|error| error.code), Some("tool.orders_db.unavailable"));
    assert!(recorded.error_message.is_some_and(|message| message.contains("hunter2")));
}

#[tokio::test]
async fn exposing_error_messages_sends_the_tool_error_message() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
    let (tool, _) = CountingTool::failing(
        "lookup_order",
        ErrorCategory::NotFound,
        "tool.not_found",
        "no ORD-7",
    );
    let agent = LlmAgentBuilder::new("support")
        .model(model)
        .tool(Arc::new(tool))
        .expose_tool_error_messages(true)
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(
        outcome.answer_with(ToolResultStatus::Failed),
        json!({"error": "tool.not_found: no ORD-7"})
    );
}

#[tokio::test]
async fn a_tool_rejection_keeps_its_message_bounded() {
    let model = ScriptedModel::new(vec![call("bash", json!({"command": "rm -rf .git/hooks"}))]);
    let (tool, runs) = CountingTool::failing(
        "bash",
        ErrorCategory::Forbidden,
        "harness.shell.protected",
        ".git/hooks is a protected path; ask for approval",
    );
    let agent = LlmAgentBuilder::new("harness").model(model).tool(Arc::new(tool)).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        outcome.answer(),
        json!({"error": "tool 'bash' failed: tool.forbidden (harness.shell.protected): \
                         .git/hooks is a protected path; ask for approval"})
    );

    let model = ScriptedModel::new(vec![call("read_file", json!({"path": "../x"}))]);
    let (tool, _) = CountingTool::failing(
        "read_file",
        ErrorCategory::InvalidInput,
        "devtools.path.outside_workspace",
        &"x".repeat(TOOL_ERROR_MESSAGE_LIMIT + 1),
    );
    let agent = LlmAgentBuilder::new("harness").model(model).tool(Arc::new(tool)).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    let text = outcome.answer()["error"].as_str().unwrap().to_string();
    assert_eq!(
        text,
        format!(
            "tool 'read_file' failed: tool.invalid_input (devtools.path.outside_workspace): {}… \
             (truncated)",
            "x".repeat(TOOL_ERROR_MESSAGE_LIMIT)
        )
    );
}

#[tokio::test]
async fn an_invalid_arguments_error_keeps_its_message() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
    let (tool, _) = CountingTool::failing(
        "lookup_order",
        ErrorCategory::InvalidInput,
        TOOL_INVALID_ARGUMENTS_CODE,
        "missing field `id`",
    );
    let agent = LlmAgentBuilder::new("support").model(model).tool(Arc::new(tool)).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(
        outcome.answer_with(ToolResultStatus::Failed),
        json!({"error": "tool.invalid_input: missing field `id`"})
    );
}

#[tokio::test]
async fn a_before_tool_callback_error_message_stays_local() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
    let (tool, runs) = CountingTool::ok("lookup_order", json!({}));
    let agent = LlmAgentBuilder::new("support")
        .model(model)
        .tool(Arc::new(tool))
        .before_tool_callback(Box::new(|_| Box::pin(async { Err(audit_error()) })))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        outcome.answer_with(ToolResultStatus::NotRun),
        json!({"error": "tool 'lookup_order' was not run: a before-tool callback failed with agent.unavailable (audit.sink_unavailable)"})
    );
}

#[tokio::test]
async fn a_ledgered_failure_records_what_the_model_was_told() {
    let model = ScriptedModel::new(vec![call("charge_card", json!({}))]);
    let (tool, _) = CountingTool::failing(
        "charge_card",
        ErrorCategory::Unavailable,
        "tool.payments.unavailable",
        &format!("gateway {SECRET} down"),
    );
    let ledger = Arc::new(InMemoryActionLedger::new());
    let agent = LlmAgentBuilder::new("payments").model(model).tool(Arc::new(tool)).build().unwrap();
    let outcome =
        run(agent, None, RunConfig::builder().action_ledger(ledger.clone()).build()).await;
    let key = format!("outcomes/user/session/{}/call-1", outcome.invocation_id());
    let record = ledger.get(&key).await.unwrap().expect("ledger record");
    assert_eq!(
        record.outcome,
        Some(ActionOutcome::Failed {
            error: "tool 'charge_card' failed: tool.unavailable (tool.payments.unavailable)"
                .to_string()
        })
    );
}

#[tokio::test]
async fn a_sub_second_timeout_is_reported_in_milliseconds() {
    let model = ScriptedModel::new(vec![call("slow_report", json!({}))]);
    let (mut tool, _) = CountingTool::ok("slow_report", json!({}));
    tool.effect = ToolEffect::ReadOnly;
    tool.delay = Some(Duration::from_secs(5));
    let agent = LlmAgentBuilder::new("reports")
        .model(model)
        .tool(Arc::new(tool))
        .tool_timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(
        outcome.answer_with(ToolResultStatus::Failed),
        json!({"error": "Tool 'slow_report' timed out after 50ms"})
    );
}

#[tokio::test]
async fn interrupted_effects_keep_unknown_outcomes() {
    let model = ScriptedModel::new(vec![call("charge_card", json!({}))]);
    let (mut tool, runs) = CountingTool::ok("charge_card", json!({"charged":true}));
    tool.delay = Some(Duration::from_secs(5));
    let agent = LlmAgentBuilder::new("payments")
        .model(model)
        .tool(Arc::new(tool))
        .tool_timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status(), Some(ToolResultStatus::OutcomeUnknown));
    assert_eq!(
        outcome.answer(),
        adk_core::outcome_unknown_response(
            "Tool 'charge_card' timed out after 50ms. The call may have taken effect; check its result before calling it again."
        )
    );
}

#[tokio::test]
async fn interrupted_holds_keep_unknown_outcomes() {
    let model = ScriptedModel::new(vec![call("charge_card", json!({}))]);
    let (mut tool, runs) = CountingTool::ok("charge_card", json!({"charged": true}));
    tool.hold = true;
    tool.delay = Some(Duration::from_secs(5));
    let agent = LlmAgentBuilder::new("payments")
        .model(model)
        .tool(Arc::new(tool))
        .tool_timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status(), Some(ToolResultStatus::OutcomeUnknown));
    assert_eq!(outcome.response().tool_result_error_code(), Some("tool.timeout"));
    assert_eq!(outcome.response().tool_result_error_category(), Some(ErrorCategory::Timeout));
    assert_eq!(
        outcome.answer(),
        adk_core::outcome_unknown_response(
            "Tool 'charge_card' timed out after 50ms. The call may have taken effect; check its result before calling it again."
        )
    );
    assert!(outcome.session_events.iter().all(|event| event.actions.tool_confirmation.is_none()));
}

#[tokio::test]
async fn a_call_with_undecodable_arguments_is_answered_without_executing() {
    let model = ScriptedModel::new(vec![call(
        "get_weather",
        adk_core::invalid_tool_arguments("arguments are not valid JSON", r#"{"city": "Nair"#),
    )]);
    let (tool, runs) = CountingTool::ok("get_weather", json!({"forecast": "sunny"}));
    let agent =
        LlmAgentBuilder::new("weather").model(model.clone()).tool(Arc::new(tool)).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;

    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let answer =
        outcome.answer_with(ToolResultStatus::NotRun)["error"].as_str().unwrap().to_string();
    assert!(answer.starts_with("tool 'get_weather' was not run: arguments are not valid JSON"));
    // The run continues: the model is asked again with the call answered.
    assert_eq!(model.requests().len(), 2);
}

#[tokio::test]
async fn a_scope_guard_denial_reaches_the_model_as_a_denial_naming_the_scope() {
    // spec 3.1 §7.14: an adk-auth denial is reported as a refusal naming the missing scope,
    // not as `tool.internal (tool.legacy)`.
    struct Transfer;

    #[async_trait]
    impl Tool for Transfer {
        fn name(&self) -> &str {
            "transfer"
        }
        fn description(&self) -> &str {
            "moves money"
        }
        fn required_scopes(&self) -> &[&str] {
            &["finance:write"]
        }
        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(json!({ "status": "sent" }))
        }
    }

    let model = ScriptedModel::new(vec![call("transfer", json!({ "amount": 5 }))]);
    let guarded = adk_auth::ScopeGuard::new(adk_auth::StaticScopeResolver::new(vec![
        "finance:read".to_string(),
    ]))
    .protect(Transfer);
    let agent =
        LlmAgentBuilder::new("support").model(model).tool(Arc::new(guarded)).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(
        outcome.answer(),
        json!({
            "error": "tool 'transfer' failed: tool.forbidden (tool.permission_denied): missing \
                      required scopes: [finance:write] (tool requires: [finance:write])"
        })
    );
}

#[tokio::test]
async fn successful_business_errors_stay_unchanged() {
    let result = json!({"isError":true, "error":"business value", "status":"available"});
    let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
    let (tool, runs) = CountingTool::ok("lookup_order", result.clone());
    let agent =
        LlmAgentBuilder::new("support").model(model.clone()).tool(Arc::new(tool)).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status(), None);
    assert_eq!(outcome.answer(), result);
    let responses: Vec<_> = model.requests()[1]
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
    assert_eq!(responses, vec![result]);
}

#[tokio::test]
async fn successful_callback_payloads_stay_unchanged() {
    let value = json!({"isError": true, "error": "domain row"});
    for stage in ["before", "after", "error"] {
        let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
        let (tool, runs) = if stage == "error" {
            CountingTool::failing(
                "lookup_order",
                ErrorCategory::Unavailable,
                "tool.unavailable",
                "offline",
            )
        } else {
            CountingTool::ok("lookup_order", json!({"original": true}))
        };
        let mut builder = LlmAgentBuilder::new("support").model(model.clone()).tool(Arc::new(tool));
        let result = value.clone();
        builder = match stage {
            "before" => builder.before_tool_callback(Box::new(move |_| {
                let result = result.clone();
                Box::pin(async move {
                    Ok(Some(Content {
                        role: "function".to_string(),
                        parts: vec![Part::FunctionResponse {
                            function_response: adk_core::FunctionResponseData::new(
                                "cached", result,
                            ),
                            id: None,
                            annotations: None,
                        }],
                    }))
                })
            })),
            "after" => builder.after_tool_callback_full(Box::new(move |_, _, _, _| {
                let result = result.clone();
                Box::pin(async move { Ok(Some(result)) })
            })),
            _ => builder.on_tool_error(Box::new(move |_, _, _, _| {
                let result = result.clone();
                Box::pin(async move { Ok(Some(result)) })
            })),
        };
        let outcome = run(builder.build().unwrap(), None, RunConfig::default()).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(runs.load(Ordering::SeqCst), usize::from(stage != "before"));
        assert_eq!(outcome.answer(), value);
        assert_eq!(outcome.status(), None);
        let requests = model.requests();
        let request = &requests[1];
        let sent: Vec<_> = request
            .contents
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(|part| {
                if let Part::FunctionResponse { function_response, .. } = part {
                    Some(function_response.response.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sent, vec![value.clone()]);
    }
}

#[tokio::test]
async fn cancellation_retains_execution_facts() {
    fn cancelled() -> AdkError {
        AdkError::new(ErrorComponent::Tool, ErrorCategory::Cancelled, "tool.cancelled", "stopped")
    }
    for (stage, status) in [
        ("before", ToolResultStatus::NotRun),
        ("execute", ToolResultStatus::Failed),
        ("after", ToolResultStatus::Withheld),
    ] {
        let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
        let (tool, runs) = if stage == "execute" {
            CountingTool::failing(
                "lookup_order",
                ErrorCategory::Cancelled,
                "tool.cancelled",
                "stopped",
            )
        } else {
            CountingTool::ok("lookup_order", json!({"row": 1}))
        };
        let mut builder = LlmAgentBuilder::new("support").model(model).tool(Arc::new(tool));
        if stage == "before" {
            builder =
                builder.before_tool_callback(Box::new(|_| Box::pin(async { Err(cancelled()) })));
        } else if stage == "after" {
            builder =
                builder.after_tool_callback(Box::new(|_| Box::pin(async { Err(cancelled()) })));
        }
        let ledger = Arc::new(InMemoryActionLedger::new());
        let config = RunConfig::builder().action_ledger(ledger.clone()).build();
        let outcome = run(builder.build().unwrap(), None, config).await;
        assert_eq!(runs.load(Ordering::SeqCst), usize::from(stage != "before"));
        let expected = match stage {
            "before" => {
                json!({"error": "tool 'lookup_order' was not run: a before-tool callback failed with tool.cancelled (tool.cancelled)"})
            }
            "after" => adk_core::result_withheld_response("lookup_order", &cancelled(), false),
            _ => adk_core::tool_error_response("lookup_order", &cancelled(), false),
        };
        assert_eq!(
            outcome.response(),
            &Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new("lookup_order", expected),
                id: Some("call-1".to_string()),
                annotations: Some(json!({
                    TOOL_RESULT_STATUS_KEY: status,
                    TOOL_RESULT_ERROR_CODE_KEY: "tool.cancelled",
                    TOOL_RESULT_ERROR_CATEGORY_KEY: "cancelled",
                })),
            }
        );
        if stage == "after" {
            let key = format!("outcomes/user/session/{}/call-1", outcome.invocation_id());
            let record = ledger.get(&key).await.unwrap().expect("ledger record");
            assert!(
                matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })),
                "the ledger and the status agree that the call ran: {:?}",
                record.outcome
            );
        }
    }
}

#[cfg(feature = "enhanced-plugins")]
struct ReplacingPlugin(Value);

#[cfg(feature = "enhanced-plugins")]
#[async_trait]
impl adk_plugin::EnhancedPlugin for ReplacingPlugin {
    fn name(&self) -> &str {
        "replacing"
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        _result: Value,
        _ctx: Arc<dyn adk_core::CallbackContext>,
        _plugin_ctx: &adk_plugin::PluginContext,
    ) -> Result<adk_plugin::AfterToolCallResult> {
        Ok(adk_plugin::AfterToolCallResult::Continue(self.0.clone()))
    }
}

/// A hook that replaces a result owns what the model reads, so a known failure status does
/// not carry over; an unknown outcome does, with its code and category.
#[tokio::test]
async fn replacements_keep_only_unknown_outcomes() {
    let mut hooks = vec!["on_tool_error", "after_tool_callback", "after_tool_callback_full"];
    if cfg!(feature = "enhanced-plugins") {
        hooks.push("enhanced_plugin");
    }
    for hook in hooks {
        for unknown in [false, true] {
            let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
            let (mut tool, runs) = if unknown {
                CountingTool::ok("lookup_order", json!({"row": 1}))
            } else {
                CountingTool::failing(
                    "lookup_order",
                    ErrorCategory::Unavailable,
                    "tool.unavailable",
                    "offline",
                )
            };
            if unknown {
                tool.delay = Some(Duration::from_secs(5));
            }
            let replacement = json!({"cached": true});
            let value = replacement.clone();
            let builder = LlmAgentBuilder::new("support")
                .model(model)
                .tool(Arc::new(tool))
                .tool_timeout(Duration::from_millis(50));
            let builder = match hook {
                "on_tool_error" => builder.on_tool_error(Box::new(move |_, _, _, _| {
                    let value = value.clone();
                    Box::pin(async move { Ok(Some(value)) })
                })),
                "after_tool_callback" => builder.after_tool_callback(Box::new(move |_| {
                    let part = Part::FunctionResponse {
                        function_response: adk_core::FunctionResponseData::new(
                            "lookup_order",
                            value.clone(),
                        ),
                        id: None,
                        annotations: None,
                    };
                    Box::pin(async move {
                        Ok(Some(Content { role: "function".to_string(), parts: vec![part] }))
                    })
                })),
                "after_tool_callback_full" => {
                    builder.after_tool_callback_full(Box::new(move |_, _, _, _| {
                        let value = value.clone();
                        Box::pin(async move { Ok(Some(value)) })
                    }))
                }
                #[cfg(feature = "enhanced-plugins")]
                "enhanced_plugin" => builder.enhanced_plugin(Arc::new(ReplacingPlugin(value))),
                other => unreachable!("{other}"),
            };
            let outcome = run(builder.build().unwrap(), None, RunConfig::default()).await;
            assert!(outcome.error.is_none(), "{hook}: {:?}", outcome.error);
            assert_eq!(runs.load(Ordering::SeqCst), 1, "{hook}");
            let annotations = unknown.then(|| {
                json!({
                    TOOL_RESULT_STATUS_KEY: ToolResultStatus::OutcomeUnknown,
                    TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
                    TOOL_RESULT_ERROR_CATEGORY_KEY: "timeout",
                })
            });
            assert_eq!(
                outcome.response(),
                &Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(
                        "lookup_order",
                        replacement
                    ),
                    id: Some("call-1".to_string()),
                    annotations,
                },
                "{hook}, unknown: {unknown}"
            );
        }
    }
}

#[tokio::test]
async fn runner_hook_errors_skip_execution() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
    let (tool, runs) = CountingTool::ok("lookup_order", json!({}));
    let agent = LlmAgentBuilder::new("support").model(model).tool(Arc::new(tool)).build().unwrap();
    let plugin = Plugin::new(PluginConfig {
        name: "audit".into(),
        before_tool: Some(Box::new(|_| Box::pin(async { Err(audit_error()) }))),
        ..Default::default()
    });
    let outcome = run(agent, Some(plugin), RunConfig::default()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        outcome.answer_with(ToolResultStatus::NotRun),
        json!({"error": "tool 'lookup_order' was not run: a before-tool callback failed with agent.unavailable (audit.sink_unavailable)"})
    );
}

#[tokio::test]
async fn missing_tools_are_explicit_failures() {
    let model = ScriptedModel::new(vec![call("missing", json!({}))]);
    let agent = LlmAgentBuilder::new("support").model(model).build().unwrap();
    let outcome = run(agent, None, RunConfig::default()).await;
    assert_eq!(
        outcome.answer_with(ToolResultStatus::NotRun),
        json!({"error": "Tool missing not found"})
    );
}

#[tokio::test]
async fn concurrency_rejections_skip_execution() {
    let model = ScriptedModel::new(vec![call("lookup_order", json!({}))]);
    let (tool, runs) = CountingTool::ok("lookup_order", json!({}));
    let agent = LlmAgentBuilder::new("support").model(model).tool(Arc::new(tool)).build().unwrap();
    let config = RunConfig::builder()
        .tool_concurrency(ToolConcurrencyConfig {
            max_concurrency: Some(0),
            backpressure: BackpressurePolicy::Fail,
            ..Default::default()
        })
        .build();
    let outcome = run(agent, None, config).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(
        outcome.answer_with(ToolResultStatus::NotRun)["error"]
            .as_str()
            .unwrap()
            .contains("concurrency limit")
    );
}

#[path = "tool_outcome/uncertainty.rs"]
mod uncertainty;
