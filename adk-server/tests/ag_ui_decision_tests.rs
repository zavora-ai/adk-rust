//! AG-UI decisions (spec 3.3 §10 S2): a held call ends a protocol-native AG-UI run with an
//! interrupt naming its inbox entry, an AG-UI resume decides the entry through the approval
//! service, the call runs once, and a second answer is reported as a conflict.

#![cfg(feature = "approvals")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ApprovalPolicy, ApprovalService, ApprovalState, ApproverSelector, InMemoryApprovalStore,
    SingleAgentLoader, StoreApprovalService, Tool, ToolContext,
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

/// A finance agent whose `send_invoice` needs confirmation, served with an inbox the
/// session's owner decides (a personal agent: the person approves their own agent's calls).
fn fixture(script: Vec<ScriptedResponse>, with_service: bool) -> Fixture {
    let sent = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new(APP)
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .tool(Arc::new(SendInvoice(sent.clone())))
        .require_tool_confirmation("send_invoice")
        .build()
        .unwrap();
    let service =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval(),
        ));
    let sessions = Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>;
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(agent))), sessions)
        .with_request_context(Arc::new(UserExtractor));
    let builder = ServerBuilder::new(config);
    let app = if with_service {
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
                    "agent": APP,
                    "tool": "send_invoice",
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
