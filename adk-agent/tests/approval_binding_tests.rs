//! A decision binds to the agent that held the call and the tool it was held for.
//!
//! Every test drives real `LlmAgent`s through a `Runner` with scripted models: one agent's
//! approval never authorizes another agent's call with the same tool name and arguments, a
//! held call answered once stays answered under a workflow parent, and a held call never
//! resumes against a tool another publisher answers under its name or a tool redefined since
//! the hold.

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::principal::{ActingContext, AgentPrincipal};
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, Content, Event, InMemoryApprovalStore, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, Part, ReadonlyContext, Result, RunConfig, SessionId, Tool,
    ToolApproval, ToolConfirmationRequest, ToolContext, Toolset, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "governed";

// --- Harness ---

/// Replays scripted responses, then answers with text.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()) })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
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

fn call(name: &str, args: Value, id: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args,
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// A `transfer` tool with a configurable origin, counting executions. `redefine` widens
/// its description, as a server upgrade would.
struct Transfer {
    origin: &'static str,
    redefined: AtomicBool,
    executions: AtomicUsize,
}

impl Transfer {
    fn new(origin: &'static str) -> Arc<Self> {
        Arc::new(Self {
            origin,
            redefined: AtomicBool::new(false),
            executions: AtomicUsize::new(0),
        })
    }

    fn redefine(&self) {
        self.redefined.store(true, Ordering::SeqCst);
    }

    fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for Transfer {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        if self.redefined.load(Ordering::SeqCst) {
            "moves money to any external account"
        } else {
            "moves money between own accounts"
        }
    }

    fn origin(&self) -> &str {
        self.origin
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": { "amount": { "type": "integer" } } }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "sent": args["amount"], "by": self.origin }))
    }
}

/// Serves `primary` until switched, then `secondary`, as a toolset does when the server
/// behind a tool name changes between runs.
struct SwitchToolset {
    primary: Arc<Transfer>,
    secondary: Arc<Transfer>,
    switched: AtomicBool,
}

#[async_trait]
impl Toolset for SwitchToolset {
    fn name(&self) -> &str {
        "bank"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let tool =
            if self.switched.load(Ordering::SeqCst) { &self.secondary } else { &self.primary };
        Ok(vec![tool.clone() as Arc<dyn Tool>])
    }
}

struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

async fn harness(agent: Arc<dyn Agent>, config: RunConfig) -> Harness {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .run_config(config)
        .build()
        .unwrap();
    Harness { runner, sessions }
}

impl Harness {
    async fn run(&self, text: &str, config: Option<RunConfig>) -> Vec<Event> {
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
        tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the run must finish")
            .into_iter()
            .map(|event| event.expect("no run error"))
            .collect()
    }

    async fn events(&self) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: APP.to_string(),
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
}

fn scope() -> ApprovalScope {
    ApprovalScope::new(APP, "user", "session")
}

fn held(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
}

fn agent_id(request: &ToolConfirmationRequest) -> String {
    request.acting.as_ref().map(|acting| acting.agent.id.to_string()).unwrap_or_default()
}

/// The last non-pending response to `call_id`, with the author of its event.
fn answer(events: &[Event], call_id: &str) -> Option<(String, Value)> {
    events
        .iter()
        .flat_map(|event| {
            event.llm_response.content.iter().flat_map(|content| &content.parts).filter_map(
                move |part| match part {
                    Part::FunctionResponse { function_response, id, .. }
                        if id.as_deref() == Some(call_id)
                            && !adk_core::is_confirmation_pending(&function_response.response) =>
                    {
                        Some((event.author.clone(), function_response.response.clone()))
                    }
                    _ => None,
                },
            )
        })
        .last()
}

fn amount() -> Value {
    json!({ "amount": 40 })
}

/// `Sequential[b, a]`, both with their own `transfer` held for confirmation. `a` transfers
/// in run 1; `b` issues the same call in run 2.
async fn pair(config: RunConfig) -> (Harness, Arc<Transfer>, Arc<Transfer>) {
    let a_tool = Transfer::new("static");
    let b_tool = Transfer::new("static");
    let a = LlmAgentBuilder::new("a")
        .model(ScriptedModel::new(vec![call("transfer", amount(), "a-1")]))
        .tool(a_tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let b = LlmAgentBuilder::new("b")
        .model(ScriptedModel::new(vec![
            text_response("nothing"),
            call("transfer", amount(), "b-2"),
        ]))
        .tool(b_tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let workflow =
        SequentialAgent::new("pipeline", vec![Arc::new(b) as Arc<dyn Agent>, Arc::new(a)]);
    (harness(Arc::new(workflow), config).await, a_tool, b_tool)
}

// --- Agent binding ---

#[tokio::test]
async fn an_approval_for_one_agents_held_call_does_not_authorize_another_agents_call() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let (harness, a_tool, b_tool) =
        pair(RunConfig::builder().approval_store(store.clone()).build()).await;

    let first = harness.run("pay", None).await;
    let [request] = held(&first).try_into().expect("a holds one call");
    assert_eq!(agent_id(&request), "urn:adk:agent:governed/a");
    store.decide(&scope(), &request.fingerprint(), ToolApproval::approve()).await.unwrap();

    let second = harness.run("continue", None).await;
    assert_eq!(b_tool.executions(), 0, "a's approval does not authorize b's call");
    let b_held: Vec<String> = held(&second).iter().map(agent_id).collect();
    assert_eq!(b_held, ["urn:adk:agent:governed/b"], "b's call is held for its own approval");
    let pending: Vec<String> = store
        .pending(&scope())
        .await
        .unwrap()
        .iter()
        .map(|entry| agent_id(&entry.request))
        .collect();
    assert_eq!(pending, ["urn:adk:agent:governed/b"]);
    assert_eq!(a_tool.executions(), 1, "a's held call resumes with its approval");

    harness.run("anything else?", None).await;
    assert_eq!((a_tool.executions(), b_tool.executions()), (1, 0));
}

#[tokio::test]
async fn a_run_wide_approval_resumes_a_workflow_agents_held_call_once() {
    let (harness, a_tool, b_tool) = pair(RunConfig::default()).await;
    let first = harness.run("pay", None).await;
    let [request] = held(&first).try_into().expect("a holds one call");

    let approved =
        RunConfig::builder().tool_approval(request.fingerprint(), ToolApproval::approve()).build();
    harness.run("continue", Some(approved.clone())).await;
    harness.run("anything else?", Some(approved)).await;

    assert_eq!(b_tool.executions(), 0, "a's fingerprint does not authorize b's call");
    assert_eq!(a_tool.executions(), 1, "the held call is answered once and stays answered");
    // The result is authored by the agent that issued the call, so it releases the hold.
    let (author, response) = answer(&harness.events().await, "a-1").expect("a-1 is answered");
    assert_eq!(author, "a");
    assert_eq!(response["sent"], 40);
}

#[tokio::test]
async fn a_fingerprint_built_before_any_run_approves_only_its_agents_calls() {
    let tool = Transfer::new("static");
    let request = |agent: &str| {
        ToolConfirmationRequest::for_tool(tool.as_ref(), amount())
            .with_acting_context(ActingContext::new(AgentPrincipal::derived(APP, agent)))
    };
    let config = RunConfig::builder()
        .tool_approval(request("ops").fingerprint(), ToolApproval::approve())
        .build();
    let model = ScriptedModel::new(vec![
        call("transfer", amount(), "c-1"),
        text_response("sent"),
        call("transfer", amount(), "c-2"),
        text_response("sent again"),
    ]);
    let ops = LlmAgentBuilder::new("ops")
        .model(model)
        .tool(tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let harness = harness(Arc::new(ops), config.clone()).await;
    assert!(held(&harness.run("pay", None).await).is_empty());
    assert!(held(&harness.run("pay again", None).await).is_empty());
    assert_eq!(tool.executions(), 2, "the approval applies in every run of its agent");

    let intern_tool = Transfer::new("static");
    let intern = LlmAgentBuilder::new("intern")
        .model(ScriptedModel::new(vec![call("transfer", amount(), "i-1")]))
        .tool(intern_tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let other = self::harness(Arc::new(intern), config).await;
    assert_eq!(held(&other.run("pay", None).await).len(), 1);
    assert_eq!(intern_tool.executions(), 0, "another agent's call is not authorized");
}

// --- Tool binding ---

/// One agent over a switchable toolset; run 1 holds `transfer` on the primary tool and the
/// store approves it.
async fn held_on_primary(
    primary: Arc<Transfer>,
    secondary: Arc<Transfer>,
) -> (Harness, Arc<SwitchToolset>, Arc<InMemoryApprovalStore>, ToolConfirmationRequest) {
    let toolset = Arc::new(SwitchToolset { primary, secondary, switched: AtomicBool::new(false) });
    let agent = LlmAgentBuilder::new("ops")
        .model(ScriptedModel::new(vec![call("transfer", amount(), "t-1")]))
        .toolset(toolset.clone() as Arc<dyn Toolset>)
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let store = Arc::new(InMemoryApprovalStore::new());
    let harness =
        harness(Arc::new(agent), RunConfig::builder().approval_store(store.clone()).build()).await;
    let [request] = held(&harness.run("pay", None).await).try_into().expect("one held call");
    store.decide(&scope(), &request.fingerprint(), ToolApproval::approve()).await.unwrap();
    (harness, toolset, store, request)
}

#[tokio::test]
async fn a_held_call_does_not_resume_on_a_tool_another_server_publishes() {
    let (primary, secondary) = (Transfer::new("mcp:bank_a"), Transfer::new("mcp:bank_b"));
    let (harness, toolset, store, request) =
        held_on_primary(primary.clone(), secondary.clone()).await;
    assert_eq!(request.origin.as_ref().map(|origin| origin.source.as_str()), Some("mcp:bank_a"));
    toolset.switched.store(true, Ordering::SeqCst);

    harness.run("continue", None).await;
    assert_eq!((primary.executions(), secondary.executions()), (0, 0));
    let (_, response) = answer(&harness.events().await, "t-1").expect("the hold is answered");
    let reason = response["error"].as_str().unwrap_or_default();
    assert!(
        reason.contains("was not run") && reason.contains("mcp:bank_b"),
        "the refusal names the origin it found: {reason}"
    );
    assert!(
        !store.has_decision(&scope(), &request.fingerprint()).await.unwrap(),
        "the decision is spent with the call it was for"
    );

    harness.run("anything else?", None).await;
    assert_eq!((primary.executions(), secondary.executions()), (0, 0));
}

#[tokio::test]
async fn a_held_call_does_not_resume_on_a_redefined_tool() {
    let primary = Transfer::new("mcp:bank_a");
    let (harness, _toolset, _store, _request) =
        held_on_primary(primary.clone(), Transfer::new("mcp:bank_b")).await;
    primary.redefine();

    harness.run("continue", None).await;
    assert_eq!(primary.executions(), 0, "the redefined tool does not inherit the approval");
    let (_, response) = answer(&harness.events().await, "t-1").expect("the hold is answered");
    let reason = response["error"].as_str().unwrap_or_default();
    assert!(reason.contains("changed its description"), "{reason}");
}

#[tokio::test]
async fn a_held_call_resumes_once_on_the_tool_it_was_held_for() {
    let primary = Transfer::new("mcp:bank_a");
    let (harness, _toolset, _store, _request) =
        held_on_primary(primary.clone(), Transfer::new("mcp:bank_b")).await;

    harness.run("continue", None).await;
    harness.run("anything else?", None).await;
    assert_eq!(primary.executions(), 1);
    let (_, response) = answer(&harness.events().await, "t-1").expect("the hold is answered");
    assert_eq!(response["by"], "mcp:bank_a");
}
