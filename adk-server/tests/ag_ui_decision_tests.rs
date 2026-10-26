//! AG-UI decisions (spec 3.3 §10 S2): a held call ends a protocol-native AG-UI run with an
//! interrupt naming its inbox entry, an AG-UI resume decides the entry through the approval
//! service, the call runs once, and a second answer is reported as a conflict.

#![cfg(feature = "approvals")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ApprovalAction, ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApprovalService,
    ApprovalState, ApprovalStore, ApproverSelector, InMemoryApprovalStore, SingleAgentLoader,
    StoreApprovalService, Tool, ToolContext,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "finance";

/// Sends an invoice; counts how often it runs.
struct SendInvoice(Arc<AtomicUsize>);

#[async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends an invoice to a client"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "sent": args["invoice"] }))
    }
}

/// `Bearer <user>` authenticates `<user>`.
struct UserExtractor;

#[async_trait]
impl RequestContextExtractor for UserExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let user = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(RequestContextError::MissingAuth)?;
        Ok(adk_core::RequestContext {
            user_id: user.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

struct Fixture {
    app: Router,
    sent: Arc<AtomicUsize>,
    service: Arc<StoreApprovalService>,
}

/// How a test server is built.
struct Setup {
    /// Mount the approval service.
    with_service: bool,
    /// Route held calls into inbox entries; without it the service leaves them store-only.
    routed: bool,
    /// Authenticate callers with `UserExtractor`.
    extractor: bool,
    /// The approval policy's escalation delay, if any.
    escalate_after: Option<std::time::Duration>,
    /// The server's kill switch.
    governance: adk_core::GovernanceControl,
    /// Who decides; by default alice, who approves her own agent's calls.
    policy: ApprovalPolicy,
    /// The risk the router gives every held call.
    risk: Option<ApprovalRisk>,
    /// What the router lets approvers do, when it restricts it.
    allowed: Option<Vec<ApprovalAction>>,
    /// The approval store; in memory by default.
    store: Option<Arc<dyn ApprovalStore>>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            with_service: true,
            routed: true,
            extractor: true,
            escalate_after: None,
            governance: adk_core::GovernanceControl::new(),
            policy: ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval(),
            risk: None,
            allowed: None,
            store: None,
        }
    }
}

/// A finance agent whose `send_invoice` needs confirmation, served with an inbox the
/// session's owner decides (a personal agent: the person approves their own agent's calls).
fn fixture(script: Vec<ScriptedResponse>, with_service: bool) -> Fixture {
    fixture_with(script, Setup { with_service, ..Setup::default() })
}

fn fixture_with(script: Vec<ScriptedResponse>, setup: Setup) -> Fixture {
    let sent = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new(APP)
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .tool(Arc::new(SendInvoice(sent.clone())))
        .require_tool_confirmation("send_invoice")
        .build()
        .unwrap();
    let mut policy = setup.policy;
    if let Some(after) = setup.escalate_after {
        policy = policy.escalate_after(after, [ApproverSelector::human("bo")]);
    }
    let (risk, allowed) = (setup.risk, setup.allowed);
    let router = move |_: &ApprovalCall| {
        let mut route = ApprovalRoute::new(policy.clone());
        if let Some(risk) = risk {
            route = route.with_risk(risk);
        }
        if let Some(allowed) = &allowed {
            route = route.with_allowed_actions(allowed.iter().copied());
        }
        Some(route)
    };
    let store = setup.store.unwrap_or_else(|| Arc::new(InMemoryApprovalStore::new()));
    let service = StoreApprovalService::new(store);
    let service = Arc::new(if setup.routed { service.with_router(router) } else { service });
    let sessions = Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>;
    let mut config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(agent))), sessions)
        .with_governance(setup.governance.clone());
    if setup.extractor {
        config = config.with_request_context(Arc::new(UserExtractor));
    }
    let builder = ServerBuilder::new(config);
    let app = if setup.with_service {
        builder.with_approval_service(service.clone()).build()
    } else {
        builder.build()
    };
    Fixture { app, sent, service }
}

fn invoice_script() -> Vec<ScriptedResponse> {
    vec![
        ScriptedResponse::function_call("send_invoice", json!({ "invoice": "INV-10" })),
        ScriptedResponse::text("The October invoice is sent."),
    ]
}

/// The AG-UI run input of a protocol-native `/run_sse` request.
fn run_body(input: Value) -> Value {
    json!({
        "appName": APP,
        "userId": "alice",
        "sessionId": "thread-1",
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": input,
    })
}

fn first_turn() -> Value {
    run_body(json!({
        "threadId": "thread-1",
        "runId": "run-1",
        "messages": [{ "id": "m1", "role": "user", "content": "Send the October invoice" }],
    }))
}

fn resume_turn(run_id: &str, resume: Value) -> Value {
    run_body(json!({
        "threadId": "thread-1",
        "runId": run_id,
        "messages": [{ "id": "m1", "role": "user", "content": "Send the October invoice" }],
        "resume": resume,
    }))
}

/// Posts `body` to `/api/run_sse` as `user` and returns the status and the AG-UI events.
async fn post(app: &Router, user: Option<&str>, body: Value) -> (StatusCode, Vec<Value>) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/run_sse")
        .header("content-type", "application/json");
    if let Some(user) = user {
        request = request.header("authorization", format!("Bearer {user}"));
    }
    let response =
        app.clone().oneshot(request.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let events = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect();
    (status, events)
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().filter_map(|event| event["type"].as_str()).collect()
}

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|event| event["type"] == kind).collect()
}

#[tokio::test]
async fn a_held_call_ends_the_run_with_an_interrupt_naming_its_inbox_entry() {
    let fx = fixture(invoice_script(), true);
    let (status, events) = post(&fx.app, Some("alice"), first_turn()).await;
    assert_eq!(status, StatusCode::OK);

    let pending = fx
        .service
        .inbox(adk_core::InboxQuery::for_approver(
            adk_core::principal::SubjectRef::human("alice"),
            Vec::<String>::new(),
        ))
        .await
        .unwrap()
        .items;
    assert_eq!(pending.len(), 1, "the held call is in the inbox");
    let entry = &pending[0];

    // The call is shown, but neither its placeholder result nor the hold's text is.
    assert_eq!(
        kinds(&events),
        [
            "RUN_STARTED",
            "MESSAGES_SNAPSHOT",
            "TOOL_CALL_START",
            "TOOL_CALL_ARGS",
            "TOOL_CALL_END",
            "RUN_FINISHED"
        ]
    );
    let call_id =
        of_type(&events, "TOOL_CALL_START")[0]["toolCallId"].as_str().unwrap().to_string();
    let finished = of_type(&events, "RUN_FINISHED")[0];
    assert_eq!(
        finished["outcome"],
        json!({
            "type": "interrupt",
            "interrupts": [{
                "id": entry.id,
                "reason": "tool_call",
                "message": entry.summary,
                "toolCallId": call_id,
                "responseSchema": {
                    "type": "object",
                    "properties": {
                        "approved": { "type": "boolean" },
                        "editedArgs": { "type": "object" },
                        "reason": { "type": "string" },
                    },
                    "required": ["approved"],
                },
                "metadata": {
                    "summary": entry.summary,
                    // The acting agent the inbox entry records, by name.
                    "agent": APP,
                    "tool": "send_invoice",
                    "answerable": true,
                    "risk": "medium",
                    "version": entry.version,
                    // The authenticated caller is the session's owner.
                    "arguments": { "invoice": "INV-10" },
                },
            }],
        })
    );
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0, "nothing runs before the decision");
}

#[tokio::test]
async fn allow_once_runs_the_call_once_and_a_second_answer_is_a_conflict() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt = of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0].clone();
    let interrupt_id = interrupt["id"].as_str().unwrap().to_string();
    let call_id = interrupt["toolCallId"].as_str().unwrap().to_string();
    let allow_once = json!([{ "interruptId": interrupt_id, "status": "resolved", "payload": { "approved": true } }]);

    let (status, resumed) =
        post(&fx.app, Some("alice"), resume_turn("run-2", allow_once.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let decided = of_type(&resumed, "CUSTOM");
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0]["name"], "adk.decision");
    assert_eq!(decided[0]["value"]["interruptId"], interrupt_id.as_str());
    assert_eq!(decided[0]["value"]["state"], "approved");
    assert_eq!(decided[0]["value"]["decidedBy"], "alice");
    // The resumed call reports its result against the original tool call, without being
    // issued again.
    assert!(of_type(&resumed, "TOOL_CALL_START").is_empty());
    let results = of_type(&resumed, "TOOL_CALL_RESULT");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["toolCallId"], call_id.as_str());
    assert_eq!(results[0]["content"], json!({ "sent": "INV-10" }).to_string());
    let finished = of_type(&resumed, "RUN_FINISHED")[0];
    assert!(finished.get("outcome").is_none(), "nothing waits any more: {finished}");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);

    // The same answer again is reported, not retried, and nothing runs.
    let (status, again) = post(&fx.app, Some("alice"), resume_turn("run-3", allow_once)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kinds(&again), ["RUN_STARTED", "CUSTOM", "RUN_ERROR"]);
    assert_eq!(of_type(&again, "CUSTOM")[0]["value"]["state"], "approved");
    let error = of_type(&again, "RUN_ERROR")[0];
    assert_eq!(error["code"], "approval.already_resolved");
    assert_eq!(error["message"], "Already decided by alice");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1, "the call ran exactly once");
}

#[tokio::test]
async fn deny_refuses_the_call_with_the_reason() {
    let script = vec![
        ScriptedResponse::function_call("send_invoice", json!({ "invoice": "INV-10" })),
        ScriptedResponse::text("I did not send it."),
    ];
    let fx = fixture(script, true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt_id =
        of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0]["id"].clone();
    let deny = json!([{ "interruptId": interrupt_id, "status": "resolved", "payload": { "approved": false, "reason": "wrong client" } }]);

    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-2", deny)).await;
    assert_eq!(of_type(&resumed, "CUSTOM")[0]["value"]["state"], "rejected");
    let result = of_type(&resumed, "TOOL_CALL_RESULT")[0]["content"].as_str().unwrap().to_string();
    assert!(result.contains("wrong client"), "{result}");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_abandoned_interrupt_keeps_waiting_in_the_inbox() {
    let script = vec![
        ScriptedResponse::function_call("send_invoice", json!({ "invoice": "INV-10" })),
        ScriptedResponse::text("It still needs your decision."),
    ];
    let fx = fixture(script, true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt_id = of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let abandon = json!([{ "interruptId": interrupt_id, "status": "cancelled" }]);

    let (status, _) = post(&fx.app, Some("alice"), resume_turn("run-2", abandon)).await;
    assert_eq!(status, StatusCode::OK);
    let entry = fx.service.get(&interrupt_id).await.unwrap().unwrap();
    assert_eq!(entry.state, ApprovalState::Pending, "nothing is decided by silence");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_resume_needs_an_authenticated_caller_and_a_valid_answer() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt_id =
        of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0]["id"].clone();

    let (status, _) = post(
        &fx.app,
        Some("alice"),
        resume_turn("run-2", json!([{ "interruptId": interrupt_id, "payload": { "yes": 1 } }])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = post(
        &fx.app,
        None,
        resume_turn(
            "run-2",
            json!([{ "interruptId": interrupt_id, "payload": { "approved": true } }]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn another_users_resume_cannot_decide_the_entry() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt_id = of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut body = resume_turn(
        "run-2",
        json!([{ "interruptId": interrupt_id, "payload": { "approved": true } }]),
    );
    body["userId"] = json!("mallory");

    // Mallory's run is in Mallory's session, which holds no such decision.
    let (_, resumed) = post(&fx.app, Some("mallory"), body).await;
    assert_eq!(
        of_type(&resumed, "CUSTOM")[0]["value"],
        json!({ "interruptId": interrupt_id, "state": "unknown" })
    );
    let entry = fx.service.get(&interrupt_id).await.unwrap().unwrap();
    assert_eq!(entry.state, ApprovalState::Pending);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn without_an_approval_service_the_interrupt_names_the_call() {
    let fx = fixture(invoice_script(), false);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let call_id = of_type(&events, "TOOL_CALL_START")[0]["toolCallId"].clone();
    let interrupt = &of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0];
    assert_eq!(interrupt["id"], call_id);
    assert_eq!(interrupt["toolCallId"], call_id);
    assert_eq!(interrupt["metadata"]["summary"], "Approve a call to 'send_invoice'");
    assert!(interrupt["metadata"].get("risk").is_none());

    let (status, _) = post(
        &fx.app,
        Some("alice"),
        resume_turn("run-2", json!([{ "interruptId": call_id, "payload": { "approved": true } }])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "only an inbox entry can be decided");
}

#[tokio::test]
async fn edit_runs_the_call_with_the_replacement_arguments() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt_id =
        of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0]["id"].clone();
    let edit = json!([{
        "interruptId": interrupt_id,
        "status": "resolved",
        "payload": { "approved": true, "editedArgs": { "invoice": "INV-11" } },
    }]);

    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-2", edit)).await;
    assert_eq!(of_type(&resumed, "CUSTOM")[0]["value"]["state"], "approved");
    assert_eq!(
        of_type(&resumed, "TOOL_CALL_RESULT")[0]["content"],
        json!({ "sent": "INV-11" }).to_string()
    );
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_resume_outside_the_native_ag_ui_transport_is_refused() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt_id =
        of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0]["id"].clone();
    let mut body = resume_turn(
        "run-2",
        json!([{ "interruptId": interrupt_id, "payload": { "approved": true } }]),
    );
    body["uiTransport"] = json!("legacy_wrapper");

    let (status, _) = post(&fx.app, Some("alice"), body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

// ----- Review of #1029 (spec 3.3 §10 S2) -----

/// The interrupts a run ended with.
fn interrupts(events: &[Value]) -> Vec<Value> {
    of_type(events, "RUN_FINISHED")[0]["outcome"]["interrupts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn two_invoices_script() -> Vec<ScriptedResponse> {
    vec![
        ScriptedResponse::function_calls([
            ("send_invoice", json!({ "invoice": "INV-10" })),
            ("send_invoice", json!({ "invoice": "INV-11" })),
        ]),
        ScriptedResponse::text("Both invoices are sent."),
    ]
}

async fn state_of(fx: &Fixture, id: &str) -> ApprovalState {
    fx.service.get(id).await.unwrap().unwrap().state
}

/// Finding 1: one refused entry decides nothing else in the resume, and the corrected resume
/// decides and runs every call.
#[tokio::test]
async fn a_refused_entry_leaves_the_other_entries_of_the_resume_undecided() {
    let fx = fixture(two_invoices_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let held = interrupts(&events);
    assert_eq!(held.len(), 2);
    let (a, b) = (held[0]["id"].as_str().unwrap(), held[1]["id"].as_str().unwrap());

    let stale = json!([
        { "interruptId": a, "payload": { "approved": true } },
        { "interruptId": b, "payload": { "approved": true }, "metadata": { "version": 99 } },
    ]);
    let (_, refused) = post(&fx.app, Some("alice"), resume_turn("run-2", stale)).await;
    assert_eq!(kinds(&refused).last(), Some(&"RUN_ERROR"));
    assert_eq!(of_type(&refused, "RUN_ERROR")[0]["code"], "approval.version_conflict");
    assert_eq!(state_of(&fx, a).await, ApprovalState::Pending, "a resume is all or nothing");
    assert_eq!(state_of(&fx, b).await, ApprovalState::Pending);

    let both = json!([
        { "interruptId": a, "payload": { "approved": true } },
        { "interruptId": b, "payload": { "approved": true } },
    ]);
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-3", both)).await;
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 2, "{resumed:#?}");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 2);
}

/// Finding 1: an answer the inbox already holds, from this surface or another, lets the run
/// start and dispatch the call instead of wedging it.
#[tokio::test]
async fn an_entry_already_decided_lets_the_run_start() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    // The person allows it in the web home before the AG-UI client answers.
    let entry = fx.service.get(&id).await.unwrap().unwrap();
    fx.service
        .decide(
            &id,
            entry.version,
            adk_core::ApprovalDecision::approve(adk_core::principal::SubjectRef::human("alice")),
        )
        .await
        .unwrap();

    let allow_once = json!([{ "interruptId": id, "payload": { "approved": true } }]);
    let (status, resumed) =
        post(&fx.app, Some("alice"), resume_turn("run-2", allow_once.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(of_type(&resumed, "RUN_ERROR").is_empty(), "{resumed:#?}");
    assert_eq!(of_type(&resumed, "CUSTOM")[0]["value"]["state"], "approved");
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 1);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);

    // Once the call ran, the same answer is a conflict and nothing runs again.
    let (_, again) = post(&fx.app, Some("alice"), resume_turn("run-3", allow_once)).await;
    assert_eq!(of_type(&again, "RUN_ERROR")[0]["code"], "approval.already_resolved");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);
}

/// Finding 1: one interrupt answered twice in one resume is not an answer.
#[tokio::test]
async fn a_resume_answering_one_interrupt_twice_is_refused() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].clone();
    let twice = json!([
        { "interruptId": id, "payload": { "approved": true } },
        { "interruptId": id, "payload": { "approved": false } },
    ]);
    let (status, _) = post(&fx.app, Some("alice"), resume_turn("run-2", twice)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(state_of(&fx, id.as_str().unwrap()).await, ApprovalState::Pending);
}

/// Finding 2: a resume whose request fails after its answers were read decides nothing, so
/// the corrected request decides and runs the call.
#[tokio::test]
async fn a_resume_whose_request_fails_decides_nothing() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    let allow_once = json!([{ "interruptId": id, "payload": { "approved": true } }]);

    let mut invalid = resume_turn("run-2", allow_once.clone());
    invalid["stateDelta"] = json!([1]);
    let (status, _) = post(&fx.app, Some("alice"), invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Pending);

    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-3", allow_once)).await;
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 1, "{resumed:#?}");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);
}

/// Finding 2: a frozen server decides nothing.
#[tokio::test]
async fn a_resume_while_frozen_decides_nothing() {
    let governance = adk_core::GovernanceControl::new();
    let fx = fixture_with(
        invoice_script(),
        Setup { governance: governance.clone(), ..Setup::default() },
    );
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    let allow_once = json!([{ "interruptId": id, "payload": { "approved": true } }]);

    governance.freeze("incident 4012");
    let (status, frozen) =
        post(&fx.app, Some("alice"), resume_turn("run-2", allow_once.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{frozen:?}");
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Pending, "a freeze decides nothing");

    governance.unfreeze();
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-3", allow_once)).await;
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 1, "{resumed:#?}");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);
}

/// Finding 3: a call the service leaves store-only has no inbox entry, so its interrupt offers
/// no answer.
#[tokio::test]
async fn a_store_only_call_is_not_offered_as_answerable() {
    let fx = fixture_with(invoice_script(), Setup { routed: false, ..Setup::default() });
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt = &interrupts(&events)[0];
    assert_eq!(interrupt["id"], interrupt["toolCallId"]);
    assert!(interrupt.get("responseSchema").is_none(), "{interrupt}");
    assert_eq!(interrupt["metadata"]["answerable"], false);
}

/// Finding 4: a conflict reports the version to answer at, and answering at it decides.
#[tokio::test]
async fn a_version_conflict_reports_the_current_version() {
    let fx = fixture_with(
        invoice_script(),
        Setup { escalate_after: Some(std::time::Duration::ZERO), ..Setup::default() },
    );
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt = &interrupts(&events)[0];
    let id = interrupt["id"].as_str().unwrap().to_string();
    let shown = interrupt["metadata"]["version"].as_u64().unwrap();
    // An escalation step fires while the person reads the decision.
    fx.service.tick(chrono::Utc::now() + chrono::Duration::seconds(1)).await.unwrap();
    let current = fx.service.get(&id).await.unwrap().unwrap().version;
    assert!(current > shown);

    let at = |version: u64| json!([{ "interruptId": id, "payload": { "approved": true }, "metadata": { "version": version } }]);
    let (_, refused) = post(&fx.app, Some("alice"), resume_turn("run-2", at(shown))).await;
    assert_eq!(of_type(&refused, "RUN_ERROR")[0]["code"], "approval.version_conflict");
    assert_eq!(of_type(&refused, "CUSTOM")[0]["value"]["version"], current);

    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-3", at(current))).await;
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 1, "{resumed:#?}");
}

async fn ag_ui_features(app: &Router) -> Vec<String> {
    let request = Request::builder()
        .uri("/api/ui/capabilities")
        .header("authorization", "Bearer alice")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    json["protocols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|protocol| protocol["protocol"] == "ag_ui")
        .unwrap()["features"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|feature| feature.as_str().map(str::to_string))
        .collect()
}

/// Finding 5: resume is advertised only where it is wired.
#[tokio::test]
async fn interrupt_resume_is_advertised_only_where_it_is_wired() {
    let wired = fixture(invoice_script(), true);
    assert!(ag_ui_features(&wired.app).await.contains(&"interrupt_resume".to_string()));
    let unwired = fixture(invoice_script(), false);
    let features = ag_ui_features(&unwired.app).await;
    assert!(features.contains(&"tool_call_interrupts".to_string()));
    assert!(!features.contains(&"interrupt_resume".to_string()), "{features:?}");
    let anonymous = fixture_with(invoice_script(), Setup { extractor: false, ..Setup::default() });
    assert!(!ag_ui_features(&anonymous.app).await.contains(&"interrupt_resume".to_string()));
}

/// Finding 10: without a `RequestContextExtractor` a resume reaches its own check and is
/// refused, and the entry stays undecided.
#[tokio::test]
async fn a_resume_without_a_request_context_extractor_is_unauthorized() {
    let fx = fixture_with(invoice_script(), Setup { extractor: false, ..Setup::default() });
    let (status, events) = post(&fx.app, None, first_turn()).await;
    assert_eq!(status, StatusCode::OK);
    let interrupt = &interrupts(&events)[0];
    // Wave 2 findings 5, 14 and 19: no resume can decide it here, so none is offered.
    assert_eq!(interrupt["metadata"]["answerable"], false, "{interrupt}");
    assert!(interrupt.get("responseSchema").is_none(), "{interrupt}");
    let id = interrupt["id"].as_str().unwrap().to_string();
    let (status, _) = post(
        &fx.app,
        None,
        resume_turn("run-2", json!([{ "interruptId": id, "payload": { "approved": true } }])),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Pending);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

/// Finding 12 of #1029, and wave 2 findings 7 and 24: the interrupt names the acting agent the
/// inbox entry records by its name, never by its identity URN (spec 3.3 §7.1, §8).
#[tokio::test]
async fn the_interrupt_names_the_agent_from_the_inbox_entry() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt = &interrupts(&events)[0];
    let entry = fx.service.get(interrupt["id"].as_str().unwrap()).await.unwrap().unwrap();
    let acting = entry.acting.expect("the governed path records the acting agent");
    assert_eq!(acting.agent.id.as_str(), "urn:adk:agent:finance/finance");
    assert_eq!(interrupt["metadata"]["agent"], "finance");
}

// ----- Wave 2 review (spec 3.3 §10 S2) -----

/// Finding 2: a second resume of a thread while the first one's run is still going is
/// refused, so two runs never resume the same held call; once the first run ends, the same
/// answer is reported as already decided and nothing runs again.
#[tokio::test]
async fn a_second_resume_while_the_first_runs_is_refused() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    let allow_once = json!([{ "interruptId": id, "payload": { "approved": true } }]);

    // The first resume's answer is recorded and its run starts once its stream is read.
    let request = Request::builder()
        .method("POST")
        .uri("/api/run_sse")
        .header("content-type", "application/json")
        .header("authorization", "Bearer alice")
        .body(Body::from(resume_turn("run-2", allow_once.clone()).to_string()))
        .unwrap();
    let first = fx.app.clone().oneshot(request).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Approved);

    // A double click, or a retry, while it runs.
    let (status, again) =
        post(&fx.app, Some("alice"), resume_turn("run-3", allow_once.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kinds(&again), ["RUN_STARTED", "RUN_ERROR"], "{again:#?}");
    assert_eq!(of_type(&again, "RUN_ERROR")[0]["code"], "approval.resume_in_progress");

    let bytes = axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("TOOL_CALL_RESULT"));
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1);

    let (_, after) = post(&fx.app, Some("alice"), resume_turn("run-4", allow_once)).await;
    assert_eq!(of_type(&after, "RUN_ERROR")[0]["code"], "approval.already_resolved");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1, "the call ran exactly once");
}

/// Finding 3: a high-risk call is offered and accepted only as a denial from AG-UI, which
/// cannot confirm it is the person (spec 3.3 §7.2).
#[tokio::test]
async fn a_high_risk_call_is_only_denied_from_ag_ui() {
    for risk in [ApprovalRisk::High, ApprovalRisk::Critical] {
        let fx = fixture_with(invoice_script(), Setup { risk: Some(risk), ..Setup::default() });
        let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
        let interrupt = &interrupts(&events)[0];
        assert_eq!(
            interrupt["responseSchema"],
            json!({
                "type": "object",
                "properties": {
                    "approved": { "type": "boolean", "const": false },
                    "reason": { "type": "string" },
                },
                "required": ["approved"],
            }),
            "{risk:?}"
        );
        let id = interrupt["id"].as_str().unwrap().to_string();
        for yes in [json!({ "approved": true }), json!({ "approved": true, "editedArgs": {} })] {
            let resume = json!([{ "interruptId": id, "payload": yes }]);
            let (_, refused) = post(&fx.app, Some("alice"), resume_turn("run-2", resume)).await;
            let error = of_type(&refused, "RUN_ERROR")[0];
            assert_eq!(error["code"], "surface.answer_not_allowed", "{risk:?}: {refused:#?}");
            assert_eq!(
                error["message"],
                "This call is high risk or spends money; approve it where you can confirm it's you"
            );
        }
        assert_eq!(state_of(&fx, &id).await, ApprovalState::Pending);
        let deny = json!([{ "interruptId": id, "payload": { "approved": false } }]);
        let (_, denied) = post(&fx.app, Some("alice"), resume_turn("run-3", deny)).await;
        assert_eq!(of_type(&denied, "CUSTOM")[0]["value"]["state"], "rejected");
        assert_eq!(fx.sent.load(Ordering::SeqCst), 0, "{risk:?}");
    }
}

/// Findings 9 and 19: an entry the session's owner may not decide — the policy forbids
/// approving one's own call, or names other approvers — is not offered as answerable.
#[tokio::test]
async fn an_entry_the_owner_may_not_decide_is_not_answerable() {
    for policy in [
        ApprovalPolicy::new([ApproverSelector::human("alice")]),
        ApprovalPolicy::new([ApproverSelector::human("bob")]).allow_self_approval(),
    ] {
        let fx =
            fixture_with(invoice_script(), Setup { policy: policy.clone(), ..Setup::default() });
        let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
        let interrupt = &interrupts(&events)[0];
        assert_eq!(interrupt["metadata"]["answerable"], false, "{policy:?}");
        assert!(interrupt.get("responseSchema").is_none(), "{policy:?}");
    }
}

/// Findings 8, 15 and 16: the schema offers only the answers the entry allows, and an answer
/// it does not allow is refused in sentence case, in the words the surfaces use.
#[tokio::test]
async fn an_interrupt_offers_only_the_answers_its_entry_allows() {
    let fx = fixture_with(
        invoice_script(),
        Setup {
            allowed: Some(vec![ApprovalAction::Approve, ApprovalAction::Edit]),
            ..Setup::default()
        },
    );
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt = &interrupts(&events)[0];
    assert_eq!(
        interrupt["responseSchema"],
        json!({
            "type": "object",
            "properties": {
                "approved": { "type": "boolean", "const": true },
                "editedArgs": { "type": "object" },
            },
            "required": ["approved"],
        })
    );
    let id = interrupt["id"].as_str().unwrap().to_string();
    let deny = json!([{ "interruptId": id, "payload": { "approved": false } }]);
    let (_, refused) = post(&fx.app, Some("alice"), resume_turn("run-2", deny)).await;
    let error = of_type(&refused, "RUN_ERROR")[0];
    assert_eq!(error["code"], "approval.action_not_allowed");
    assert_eq!(error["message"], "This decision does not offer Deny");
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Pending);
}

/// Finding 10: a yes with replacement arguments other than the ones the entry was approved
/// with is reported as not recorded, and the call runs as it was approved.
#[tokio::test]
async fn a_different_edit_to_a_decided_entry_is_reported_as_not_recorded() {
    let fx = fixture(invoice_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    let entry = fx.service.get(&id).await.unwrap().unwrap();
    fx.service
        .decide(
            &id,
            entry.version,
            adk_core::ApprovalDecision::approve(adk_core::principal::SubjectRef::human("alice")),
        )
        .await
        .unwrap();

    let edit = json!([{ "interruptId": id, "payload": { "approved": true, "editedArgs": { "invoice": "INV-99" } } }]);
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-2", edit)).await;
    let update = &of_type(&resumed, "CUSTOM")[0]["value"];
    assert_eq!(update["state"], "approved");
    assert_eq!(update["code"], "approval.already_resolved", "{update}");
    assert_eq!(update["message"], "Already decided by alice");
    assert_eq!(
        of_type(&resumed, "TOOL_CALL_RESULT")[0]["content"],
        json!({ "sent": "INV-10" }).to_string(),
        "the call runs as it was approved"
    );
}

/// Findings 12 and 17: without an echoed version, another approver's edit since the interrupt
/// was shown is a conflict that shows the new arguments, never approved unseen; answering at
/// the reported version then completes the quorum.
#[tokio::test]
async fn without_an_echoed_version_another_approvers_edit_is_a_conflict() {
    let quorum =
        ApprovalPolicy::new([ApproverSelector::human("alice"), ApproverSelector::human("bob")])
            .with_quorum(2)
            .allow_self_approval();
    let fx = fixture_with(invoice_script(), Setup { policy: quorum, ..Setup::default() });
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let interrupt = &interrupts(&events)[0];
    assert_eq!(interrupt["metadata"]["arguments"], json!({ "invoice": "INV-10" }));
    let id = interrupt["id"].as_str().unwrap().to_string();
    let entry = fx.service.get(&id).await.unwrap().unwrap();
    let edited = json!({ "invoice": "INV-99" });
    fx.service
        .decide(
            &id,
            entry.version,
            adk_core::ApprovalDecision::edit(
                adk_core::principal::SubjectRef::human("bob"),
                edited.clone(),
            ),
        )
        .await
        .unwrap();

    let yes = json!([{ "interruptId": id, "payload": { "approved": true } }]);
    let (_, refused) = post(&fx.app, Some("alice"), resume_turn("run-2", yes)).await;
    assert_eq!(of_type(&refused, "RUN_ERROR")[0]["code"], "approval.version_conflict");
    let update = &of_type(&refused, "CUSTOM")[0]["value"];
    assert_eq!(update["editedArgs"], edited, "the update shows what changed");
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Pending);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);

    let seen = json!([{ "interruptId": id, "payload": { "approved": true }, "metadata": { "version": update["version"] } }]);
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-3", seen)).await;
    assert_eq!(
        of_type(&resumed, "TOOL_CALL_RESULT")[0]["content"],
        json!({ "sent": "INV-99" }).to_string()
    );
}

/// Findings 6 and 23: an entry withdrawn or expired before the answer, whose denial waits to
/// run, is reported with a code while the run starts and the resume's other answers stand.
#[tokio::test]
async fn an_expired_or_withdrawn_entry_is_reported_and_the_run_starts() {
    let fx = fixture(two_invoices_script(), true);
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let held = interrupts(&events);
    let (a, b) = (held[0]["id"].as_str().unwrap(), held[1]["id"].as_str().unwrap());
    fx.service.cancel(a, Some("no longer needed".into())).await.unwrap();

    let both = json!([
        { "interruptId": a, "payload": { "approved": true } },
        { "interruptId": b, "payload": { "approved": true } },
    ]);
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-2", both)).await;
    assert!(of_type(&resumed, "RUN_ERROR").is_empty(), "{resumed:#?}");
    let updates: Vec<&Value> =
        of_type(&resumed, "CUSTOM").iter().map(|event| &event["value"]).collect();
    assert_eq!(updates[0]["state"], "cancelled");
    assert_eq!(updates[0]["code"], "approval.already_resolved");
    assert_eq!(updates[0]["message"], "This decision was withdrawn");
    assert_eq!(updates[1]["state"], "approved");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 1, "only the call still decided yes runs");

    let expiring = ApprovalPolicy::new([ApproverSelector::human("alice")])
        .allow_self_approval()
        .expires_after(std::time::Duration::from_secs(600));
    let fx = fixture_with(invoice_script(), Setup { policy: expiring, ..Setup::default() });
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    fx.service.tick(chrono::Utc::now() + chrono::Duration::minutes(11)).await.unwrap();
    assert_eq!(state_of(&fx, &id).await, ApprovalState::Expired);
    let late = json!([{ "interruptId": id, "payload": { "approved": true } }]);
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-2", late)).await;
    assert!(of_type(&resumed, "RUN_ERROR").is_empty(), "{resumed:#?}");
    let update = &of_type(&resumed, "CUSTOM")[0]["value"];
    assert_eq!(
        (&update["state"], &update["code"], &update["message"]),
        (&json!("expired"), &json!("approval.already_resolved"), &json!("This decision expired"))
    );
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 1, "the call is answered as denied");
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

/// Findings 13 and 18: every AG-UI answer is on the server's audit trail with the surface and
/// how it was given, so "What your agents did" can say where the yes came from (spec 3.3 §11).
#[tokio::test]
async fn an_ag_ui_answer_is_on_record_with_its_surface() {
    let recorder = Arc::new(adk_core::audit_log::InMemoryAuditRecorder::new());
    let governance = adk_core::GovernanceControl::new();
    governance.attach_audit(adk_core::audit_log::AuditTrail::new(recorder.clone()));
    let fx = fixture_with(invoice_script(), Setup { governance, ..Setup::default() });
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    let allow_once = json!([{ "interruptId": id, "payload": { "approved": true } }]);
    post(&fx.app, Some("alice"), resume_turn("run-2", allow_once)).await;

    let answers: Vec<_> =
        recorder.records().into_iter().filter(|record| record.action == "surface.decide").collect();
    assert_eq!(answers.len(), 1, "{answers:#?}");
    let record = &answers[0];
    assert_eq!(record.decision, Some(adk_core::audit_log::AuditDecision::Approved));
    assert_eq!(record.actor.on_behalf_of, Some(adk_core::principal::SubjectRef::human("alice")));
    for (field, value) in [
        ("surface", json!("ag_ui")),
        ("origin", json!({ "kind": "app", "channel": "another app" })),
        ("approvalId", json!(id)),
        ("answer", json!("allowOnce")),
        ("outcome", json!("decided")),
        ("tool", json!("send_invoice")),
    ] {
        assert_eq!(record.ext.get(field), Some(&value), "{field}");
    }
}

/// An approval store whose reads fail on demand: `pending` once a request was created, and
/// `has_decision` while `fail_has_decision` is set.
#[derive(Debug, Default)]
struct FlakyStore {
    inner: InMemoryApprovalStore,
    fail_pending_after_create: std::sync::atomic::AtomicBool,
    created: std::sync::atomic::AtomicBool,
    fail_has_decision: std::sync::atomic::AtomicBool,
}

fn store_down() -> adk_core::AdkError {
    adk_core::AdkError::new(
        adk_core::ErrorComponent::Server,
        adk_core::ErrorCategory::Unavailable,
        "test.store_down",
        "the approval store is down",
    )
}

#[async_trait]
impl ApprovalStore for FlakyStore {
    async fn record_pending(
        &self,
        scope: &adk_core::ApprovalScope,
        request: &adk_core::ToolConfirmationRequest,
    ) -> adk_core::Result<()> {
        self.inner.record_pending(scope, request).await
    }
    async fn pending(
        &self,
        scope: &adk_core::ApprovalScope,
    ) -> adk_core::Result<Vec<adk_core::PendingApproval>> {
        if self.fail_pending_after_create.load(Ordering::SeqCst)
            && self.created.load(Ordering::SeqCst)
        {
            return Err(store_down());
        }
        self.inner.pending(scope).await
    }
    async fn decide(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
        approval: adk_core::ToolApproval,
    ) -> adk_core::Result<()> {
        self.inner.decide(scope, fingerprint, approval).await
    }
    async fn take_decision(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<Option<adk_core::ToolConfirmationDecision>> {
        self.inner.take_decision(scope, fingerprint).await
    }
    async fn has_decision(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<bool> {
        if self.fail_has_decision.load(Ordering::SeqCst) {
            return Err(store_down());
        }
        self.inner.has_decision(scope, fingerprint).await
    }
    async fn record_pending_keyed(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
        request: &adk_core::ToolConfirmationRequest,
    ) -> adk_core::Result<()> {
        self.inner.record_pending_keyed(scope, fingerprint, request).await
    }
    async fn take_approval(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<Option<adk_core::ToolApproval>> {
        self.inner.take_approval(scope, fingerprint).await
    }
    async fn create_request(
        &self,
        request: &adk_core::ApprovalRequest,
    ) -> adk_core::Result<adk_core::ApprovalRequest> {
        let created = self.inner.create_request(request).await;
        self.created.store(true, Ordering::SeqCst);
        created
    }
    async fn get_request(&self, id: &str) -> adk_core::Result<Option<adk_core::ApprovalRequest>> {
        self.inner.get_request(id).await
    }
    async fn replace_request(
        &self,
        request: &adk_core::ApprovalRequest,
        expected_version: u64,
    ) -> adk_core::Result<bool> {
        self.inner.replace_request(request, expected_version).await
    }
    async fn list_requests(
        &self,
        query: &adk_core::ApprovalRequestQuery,
    ) -> adk_core::Result<Vec<adk_core::ApprovalRequest>> {
        self.inner.list_requests(query).await
    }
    async fn put_delegation(
        &self,
        delegation: &adk_core::ApprovalDelegation,
    ) -> adk_core::Result<()> {
        self.inner.put_delegation(delegation).await
    }
    async fn delegations_to(
        &self,
        to: &adk_core::principal::SubjectRef,
    ) -> adk_core::Result<Vec<adk_core::ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }
    async fn delete_delegation(&self, id: &str) -> adk_core::Result<bool> {
        self.inner.delete_delegation(id).await
    }
}

/// Findings 11 and 20: a failed read of the inbox while the run ends is reported, never
/// turned into an interrupt that says the call cannot be decided here.
#[tokio::test]
async fn a_failed_inbox_read_is_reported_not_turned_into_an_unanswerable_interrupt() {
    let store = Arc::new(FlakyStore::default());
    store.fail_pending_after_create.store(true, Ordering::SeqCst);
    let fx = fixture_with(
        invoice_script(),
        Setup { store: Some(store.clone() as Arc<dyn ApprovalStore>), ..Setup::default() },
    );
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    assert!(of_type(&events, "RUN_FINISHED").is_empty(), "{events:#?}");
    let error = of_type(&events, "RUN_ERROR")[0];
    assert_eq!(error["code"], "test.store_down");
    assert_eq!(
        error["message"],
        "This run is waiting for a decision that could not be loaded; answer it in your \
         decisions inbox"
    );
}

/// Findings 11 and 20: a failed read of whether a decision waits to run refuses the resume
/// as a failure to try again, never as "already decided".
#[tokio::test]
async fn a_failed_decision_read_is_reported_not_called_already_decided() {
    let store = Arc::new(FlakyStore::default());
    let fx = fixture_with(
        invoice_script(),
        Setup { store: Some(store.clone() as Arc<dyn ApprovalStore>), ..Setup::default() },
    );
    let (_, events) = post(&fx.app, Some("alice"), first_turn()).await;
    let id = interrupts(&events)[0]["id"].as_str().unwrap().to_string();
    let entry = fx.service.get(&id).await.unwrap().unwrap();
    fx.service
        .decide(
            &id,
            entry.version,
            adk_core::ApprovalDecision::approve(adk_core::principal::SubjectRef::human("alice")),
        )
        .await
        .unwrap();

    store.fail_has_decision.store(true, Ordering::SeqCst);
    let allow_once = json!([{ "interruptId": id, "payload": { "approved": true } }]);
    let (_, refused) = post(&fx.app, Some("alice"), resume_turn("run-2", allow_once.clone())).await;
    let error = of_type(&refused, "RUN_ERROR")[0];
    assert_eq!(error["code"], "test.store_down", "{refused:#?}");
    assert_eq!(error["message"], "The decision could not be recorded; try again");

    store.fail_has_decision.store(false, Ordering::SeqCst);
    let (_, resumed) = post(&fx.app, Some("alice"), resume_turn("run-3", allow_once)).await;
    assert_eq!(of_type(&resumed, "TOOL_CALL_RESULT").len(), 1, "{resumed:#?}");
}
