//! What a run's policy and confirmation handler see and decide on the governed path
//! (spec 3.1 §10, C1–C3).
//!
//! Every test drives a real `LlmAgent` through a `Runner` with `adk_model::ScriptedLlm`, and
//! asserts on what the tools received and what the model was told.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::audit_log::{AuditDecision, InMemoryAuditRecorder};
use adk_core::{
    AdkError, ArgPredicate, Content, DeclarativePolicy, Event, Llm, Part, PolicyCall,
    PolicyDecision, PolicyRule, Result, RunConfig, STATIC_TOOL_ORIGIN, SessionId, StrictestPolicy,
    Tool, ToolApproval, ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest,
    ToolContext, ToolOrigin, ToolPolicy, ToolPolicyRequest, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// --- Harness ---

/// Records the arguments of every execution. `source` is what it reports as its
/// [`Tool::origin`], so a test can stand in for a tool an MCP server publishes.
struct RecordingTool {
    name: &'static str,
    source: &'static str,
    executions: Arc<Mutex<Vec<Value>>>,
}

impl RecordingTool {
    fn new(name: &'static str, source: &'static str) -> (Arc<Self>, Arc<Mutex<Vec<Value>>>) {
        let executions = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Self { name, source, executions: executions.clone() }), executions)
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

    fn origin(&self) -> &str {
        self.source
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        Ok(json!({ "status": "ok" }))
    }
}

/// Records the facts each `evaluate_call` receives and denies every tool `mcp:evil`
/// publishes. `evaluate` alone requires approval, so a path that skips `evaluate_call`
/// holds the call instead of deciding it.
#[derive(Debug, Default)]
struct OriginGuard {
    seen: Mutex<Vec<SeenCall>>,
}

/// The tool name, origin, and call ID one `evaluate_call` received.
type SeenCall = (String, Option<ToolOrigin>, Option<String>);

#[async_trait]
impl ToolPolicy for OriginGuard {
    async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
        PolicyDecision::require_approval("evaluate_call was not used")
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        self.seen.lock().unwrap().push((
            request.tool_name.clone(),
            call.origin.cloned(),
            call.function_call_id.map(str::to_string),
        ));
        match call.origin {
            Some(origin) if origin.source == "mcp:evil" => {
                PolicyDecision::deny("tools from mcp:evil are blocked")
            }
            _ => PolicyDecision::Allow,
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some("origin-guard".to_string())
    }
}

async fn runner(agent: LlmAgent, configure: impl FnOnce(RunnerBuilder) -> RunnerBuilder) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let builder = Runner::builder()
        .app_name("app")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>);
    configure(builder).build().unwrap()
}

type RunnerBuilder = adk_runner::RunnerConfigBuilder<
    adk_runner::builder::HasAppName,
    adk_runner::builder::HasAgent,
    adk_runner::builder::HasSessionService,
>;

async fn run(runner: &Runner, config: Option<RunConfig>) -> Vec<Event> {
    let stream = runner
        .run_with_config(
            UserId::new("alice").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
            config,
        )
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(30), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    events.into_iter().map(|event| event.expect("the run must not fail")).collect()
}

/// Every function response in the model's last request, as `(call id, response)`.
fn told(model: &ScriptedLlm) -> Vec<(Option<String>, Value)> {
    let last = model.last_request().expect("the model was called");
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

// --- C1: the policy sees the tool's origin and the call's ID ---

#[tokio::test]
async fn evaluate_call_receives_the_origin_and_call_id_from_the_governed_path() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_calls([
                ("search", json!({ "q": "invoices" })),
                ("lookup", json!({ "id": 7 })),
            ]),
            ScriptedResponse::text("done"),
        ],
    ));
    let (search, searches) = RecordingTool::new("search", "mcp:evil");
    let (lookup, lookups) = RecordingTool::new("lookup", STATIC_TOOL_ORIGIN);
    let (search_origin, lookup_origin) =
        (ToolOrigin::of(search.as_ref()), ToolOrigin::of(lookup.as_ref()));
    let agent = LlmAgentBuilder::new("clerk")
        .model(model.clone() as Arc<dyn Llm>)
        .tool(search)
        .tool(lookup)
        .tool_execution_strategy(adk_core::ToolExecutionStrategy::Sequential)
        .build()
        .unwrap();
    let guard = Arc::new(OriginGuard::default());
    let policy = guard.clone();
    let runner = runner(agent, move |builder| builder.tool_policy(policy)).await;

    run(&runner, None).await;

    assert!(searches.lock().unwrap().is_empty(), "the call to mcp:evil must not run");
    assert_eq!(*lookups.lock().unwrap(), vec![json!({ "id": 7 })]);
    assert_eq!(
        *guard.seen.lock().unwrap(),
        vec![
            ("search".to_string(), Some(search_origin), Some("scripted_tc_0_0".to_string())),
            ("lookup".to_string(), Some(lookup_origin), Some("scripted_tc_0_1".to_string())),
        ]
    );
    assert_eq!(
        told(&model),
        vec![
            (
                Some("scripted_tc_0_0".to_string()),
                json!({ "error": "Tool 'search' denied by policy: tools from mcp:evil are blocked" })
            ),
            (Some("scripted_tc_0_1".to_string()), json!({ "status": "ok" })),
        ],
        "the model is told why the call was refused"
    );
}

// --- C2: an origin-aware layer composes with an operator policy ---

#[tokio::test]
async fn a_strictest_policy_keeps_the_operator_policy_and_adds_the_origin_layer() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_calls([
                ("search", json!({ "q": "invoices" })),
                ("delete", json!({ "path": "/" })),
                ("lookup", json!({ "id": 7 })),
            ]),
            ScriptedResponse::text("done"),
        ],
    ));
    let (search, searches) = RecordingTool::new("search", "mcp:evil");
    let (delete, deletes) = RecordingTool::new("delete", STATIC_TOOL_ORIGIN);
    let (lookup, lookups) = RecordingTool::new("lookup", STATIC_TOOL_ORIGIN);
    let agent = LlmAgentBuilder::new("clerk")
        .model(model.clone() as Arc<dyn Llm>)
        .tool(search)
        .tool(delete)
        .tool(lookup)
        .tool_execution_strategy(adk_core::ToolExecutionStrategy::Sequential)
        .build()
        .unwrap();
    // The operator allows search and lookup; the origin layer refuses mcp:evil on top.
    let operator = DeclarativePolicy::builder().name("ops-v3").allow("search").allow("lookup");
    let policy = StrictestPolicy::new(vec![Arc::new(operator.build())])
        .layer(Arc::new(OriginGuard::default()));
    let recorder = InMemoryAuditRecorder::new();
    let audit = recorder.clone();
    let runner = runner(agent, move |builder| {
        builder.tool_policy(Arc::new(policy)).audit_log(Arc::new(audit))
    })
    .await;

    run(&runner, None).await;

    assert!(searches.lock().unwrap().is_empty(), "the origin layer denies mcp:evil");
    assert!(deletes.lock().unwrap().is_empty(), "the operator's default deny still applies");
    assert_eq!(lookups.lock().unwrap().len(), 1);
    let decisions: Vec<_> = recorder
        .records()
        .into_iter()
        .map(|record| {
            (
                record.ext["toolSource"].clone(),
                record.decision,
                record.policy_ref,
                record.ext.get("reason").cloned(),
            )
        })
        .collect();
    let policy_ref = Some("tool_policy:ops-v3+origin-guard".to_string());
    assert_eq!(
        decisions,
        vec![
            (
                json!("mcp:evil"),
                Some(AuditDecision::Denied),
                policy_ref.clone(),
                Some(json!("tools from mcp:evil are blocked")),
            ),
            (
                json!(STATIC_TOOL_ORIGIN),
                Some(AuditDecision::Denied),
                policy_ref.clone(),
                Some(json!("no policy rule permits tool 'delete'; it is denied by default")),
            ),
            (json!(STATIC_TOOL_ORIGIN), Some(AuditDecision::Allowed), policy_ref, None),
        ]
    );
}

// --- C3: a handler's edited arguments are checked again, then executed ---

/// Approves every call with `edit` in place of its arguments; `decide` must not be used.
#[derive(Debug)]
struct EditingHandler {
    edit: Value,
    shown: Mutex<Vec<Value>>,
}

#[async_trait]
impl ToolConfirmationHandler for EditingHandler {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        Err(AdkError::tool("decide must not be called when decide_approval is overridden"))
    }

    async fn decide_approval(&self, request: &ToolConfirmationRequest) -> Result<ToolApproval> {
        self.shown.lock().unwrap().push(request.args.clone());
        Ok(ToolApproval::approve()
            .with_edited_args(self.edit.clone())
            .with_message("capped by the approver"))
    }
}

/// Transfers up to 1000 need approval; larger ones are denied.
fn transfer_policy() -> Arc<DeclarativePolicy> {
    Arc::new(
        DeclarativePolicy::builder()
            .rule(
                PolicyRule::require_approval("transfer", "transfers need a person")
                    .when(ArgPredicate::at_most("/amount", 1000.0)),
            )
            .deny("transfer", "over the transfer limit")
            .build(),
    )
}

async fn transfer_with_edit(edit: Value) -> (Arc<ScriptedLlm>, Vec<Value>, Vec<Value>) {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call("transfer", json!({ "amount": 900 })),
            ScriptedResponse::text("done"),
        ],
    ));
    let (transfer, executions) = RecordingTool::new("transfer", STATIC_TOOL_ORIGIN);
    let agent = LlmAgentBuilder::new("treasurer")
        .model(model.clone() as Arc<dyn Llm>)
        .tool(transfer)
        .build()
        .unwrap();
    let runner = runner(agent, |builder| builder.tool_policy(transfer_policy())).await;
    let handler = Arc::new(EditingHandler { edit, shown: Mutex::new(Vec::new()) });
    let config = RunConfig::builder().tool_confirmation_handler(handler.clone()).build();

    run(&runner, Some(config)).await;

    let executed = executions.lock().unwrap().clone();
    let shown = handler.shown.lock().unwrap().clone();
    (model, executed, shown)
}

#[tokio::test]
async fn a_handlers_edited_arguments_are_rechecked_and_executed() {
    let (model, executed, shown) = transfer_with_edit(json!({ "amount": 400 })).await;

    assert_eq!(shown, vec![json!({ "amount": 900 })], "the approver sees the model's call");
    assert_eq!(executed, vec![json!({ "amount": 400 })], "the tool runs with the edit");
    assert_eq!(
        told(&model),
        vec![(Some("scripted_tc_0_0".to_string()), json!({ "status": "ok" }))]
    );
}

#[tokio::test]
async fn a_handlers_edit_the_policy_denies_is_not_executed() {
    let (model, executed, _) = transfer_with_edit(json!({ "amount": 5000 })).await;

    assert!(executed.is_empty(), "an edit past the limit must not run");
    assert_eq!(
        told(&model),
        vec![(
            Some("scripted_tc_0_0".to_string()),
            json!({
                "error": "Tool 'transfer' with the approver's edited arguments denied by policy: \
                          over the transfer limit"
            })
        )]
    );
}

#[tokio::test]
async fn a_handlers_denial_message_reaches_the_model() {
    #[derive(Debug)]
    struct Decline;

    #[async_trait]
    impl ToolConfirmationHandler for Decline {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolConfirmationDecision> {
            Ok(ToolConfirmationDecision::Deny)
        }

        async fn decide_approval(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> Result<ToolApproval> {
            Ok(ToolApproval::deny().with_message("pay from the treasury account instead"))
        }
    }

    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call("transfer", json!({ "amount": 900 })),
            ScriptedResponse::text("done"),
        ],
    ));
    let (transfer, executions) = RecordingTool::new("transfer", STATIC_TOOL_ORIGIN);
    let agent = LlmAgentBuilder::new("treasurer")
        .model(model.clone() as Arc<dyn Llm>)
        .tool(transfer)
        .build()
        .unwrap();
    let runner = runner(agent, |builder| builder.tool_policy(transfer_policy())).await;
    let config = RunConfig::builder().tool_confirmation_handler(Arc::new(Decline)).build();

    run(&runner, Some(config)).await;

    assert!(executions.lock().unwrap().is_empty());
    assert_eq!(
        told(&model),
        vec![(
            Some("scripted_tc_0_0".to_string()),
            json!({
                "error": "Tool 'transfer' was not run; the approver declined it: pay from the \
                          treasury account instead"
            })
        )]
    );
}
