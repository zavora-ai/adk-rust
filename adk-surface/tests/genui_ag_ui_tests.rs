//! Agent-drawn surfaces over AG-UI (spec 3.3 §6.1 `genui`, §10 S2): a drawing reaches an
//! AG-UI client as an `a2ui-surface` activity, in the shape `genui_ag_ui_activity.json` pins;
//! an echoed activity is kept only when it passes the validator again; and a form a client
//! sends through `forwardedProps.a2uiAction` ends the run with a `tool_call` interrupt with
//! nothing run, its constants taken from the stored surface.

#![cfg(feature = "genui")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ApprovalPolicy, ApprovalRoute, ApprovalService, ApproverSelector, InMemoryApprovalStore,
    SingleAgentLoader, StoreApprovalService, Tool, ToolContext,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::rest::controllers::AgUiExtension;
use adk_server::rest::controllers::runtime::AgUiInputMessage;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::genui::{DrawToolset, GenuiAgUi};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "finance";
const FIXTURE: &str = "tests/genui_ag_ui_activity.json";

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

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "sent": true }))
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

fn invoice_form() -> Value {
    json!({
        "surfaceId": "invoice-10",
        "title": "Invoice INV-10",
        "fields": [
            { "name": "to", "label": "To", "type": "email", "required": true },
            { "name": "amount", "label": "Amount", "type": "number", "value": 4200 }
        ],
        "actions": [{
            "id": "send",
            "label": "Send",
            "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "amount": { "path": "/amount" }, "invoice": "INV-10" }
        }]
    })
}

struct Fixture {
    app: Router,
    sent: Arc<AtomicUsize>,
    service: Arc<StoreApprovalService>,
}

/// Finance draws the invoice form on its first turn; the server carries [`GenuiAgUi`].
fn fixture() -> Fixture {
    let sent = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("render_form", invoice_form()),
            ScriptedResponse::text("Here is the October invoice."),
        ],
    ));
    let agent = LlmAgentBuilder::new(APP)
        .model(model)
        .tool(Arc::new(SendInvoice(sent.clone())))
        .toolset(Arc::new(DrawToolset::forms()))
        .build()
        .unwrap();
    let policy = ApprovalPolicy::new([ApproverSelector::human("ana")]).allow_self_approval();
    let service =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            move |_: &adk_core::ApprovalCall| Some(ApprovalRoute::new(policy.clone())),
        ));
    let sessions = Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>;
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(agent))), sessions)
        .with_request_context(Arc::new(UserExtractor));
    let app = ServerBuilder::new(config)
        .with_approval_service(service.clone())
        .with_ag_ui_extension(Arc::new(GenuiAgUi::new()))
        .build();
    Fixture { app, sent, service }
}

fn run_body(input: Value) -> Value {
    json!({
        "appName": APP,
        "userId": "ana",
        "sessionId": "thread-1",
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": input,
    })
}

fn history() -> Value {
    json!([{ "id": "m1", "role": "user", "content": "Prepare the October invoice for ACME" }])
}

fn submit(version: u64, action: &str, values: Value) -> Value {
    run_body(json!({
        "threadId": "thread-1",
        "runId": "run-2",
        "messages": history(),
        "forwardedProps": {
            "a2uiAction": { "surfaceId": "invoice-10", "version": version, "actionId": action, "values": values },
        },
    }))
}

async fn post(app: &Router, body: Value) -> (StatusCode, Vec<Value>) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/run_sse")
        .header("content-type", "application/json")
        .header("authorization", "Bearer ana")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
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

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|event| event["type"] == kind).collect()
}

async fn draw(fx: &Fixture) -> Vec<Value> {
    let (status, events) = post(
        &fx.app,
        run_body(json!({ "threadId": "thread-1", "runId": "run-1", "messages": history() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    events
}

async fn waiting(fx: &Fixture) -> Vec<adk_core::ApprovalRequest> {
    fx.service
        .inbox(adk_core::InboxQuery::for_approver(
            adk_core::principal::SubjectRef::human("ana"),
            Vec::<String>::new(),
        ))
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn a_drawing_reaches_an_ag_ui_client_as_the_fixture_pins() {
    let fx = fixture();
    let events = draw(&fx).await;
    let activities = of_type(&events, "ACTIVITY_SNAPSHOT");
    assert_eq!(activities.len(), 1, "{events:?}");
    let activity = activities[0].clone();
    if std::env::var("ADK_SURFACE_WRITE_API").is_ok() {
        std::fs::write(FIXTURE, serde_json::to_string_pretty(&activity).unwrap() + "\n").unwrap();
    }
    let pinned: Value = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    assert_eq!(activity, pinned);
    // It follows the render tool's result.
    let kinds: Vec<&str> = events.iter().filter_map(|event| event["type"].as_str()).collect();
    let result = kinds.iter().position(|kind| *kind == "TOOL_CALL_RESULT").unwrap();
    assert_eq!(kinds[result + 1], "ACTIVITY_SNAPSHOT");
}

#[tokio::test]
async fn a_forwarded_send_ends_the_run_with_a_tool_call_interrupt_and_nothing_run() {
    let fx = fixture();
    draw(&fx).await;
    let (status, events) =
        post(&fx.app, submit(1, "send", json!({ "/to": "ana@example.com" }))).await;
    assert_eq!(status, StatusCode::OK, "{events:?}");
    let entries = waiting(&fx).await;
    assert_eq!(entries.len(), 1);
    let interrupt = &of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0];
    assert_eq!(interrupt["reason"], "tool_call");
    assert_eq!(interrupt["id"], json!(entries[0].id));
    // The typed address, and the amount and invoice the stored form fixed.
    assert_eq!(
        interrupt["metadata"]["arguments"],
        json!({ "to": "ana@example.com", "amount": 4200, "invoice": "INV-10" })
    );
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_tampered_stale_or_undeclared_send_is_refused() {
    let fx = fixture();
    draw(&fx).await;
    for (body, status) in [
        // A constant the form fixed is not a field the client may set.
        (submit(1, "send", json!({ "/invoice": "INV-99" })), StatusCode::BAD_REQUEST),
        (submit(0, "send", json!({ "/to": "ana@example.com" })), StatusCode::CONFLICT),
        (submit(1, "approve", json!({ "/to": "ana@example.com" })), StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(post(&fx.app, body).await.0, status);
    }
    assert!(waiting(&fx).await.is_empty());
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_echoed_activity_is_kept_only_when_it_passes_the_validator() {
    let fx = fixture();
    let events = draw(&fx).await;
    let mut ours = of_type(&events, "ACTIVITY_SNAPSHOT")[0].clone();
    let extension = GenuiAgUi::new();
    let echoed = |content: Value, activity_type: &str| AgUiInputMessage {
        id: Some("echo".into()),
        role: Some("activity".into()),
        activity_type: Some(activity_type.into()),
        content: Some(content),
        ..AgUiInputMessage::default()
    };
    assert!(extension.keeps_activity(&echoed(ours["content"].clone(), "a2ui-surface")));

    // The same form with its button relabelled "Approve".
    let operations = ours["content"]["a2ui_operations"].as_array_mut().unwrap();
    let components =
        operations.iter_mut().find_map(|operation| operation.get_mut("updateComponents")).unwrap()
            ["components"]
            .as_array_mut()
            .unwrap();
    for component in components.iter_mut() {
        if component["text"] == "Send" {
            component["text"] = json!("Approve");
        }
    }
    assert!(!extension.keeps_activity(&echoed(ours["content"].clone(), "a2ui-surface")));
    assert!(!extension.keeps_activity(&echoed(json!({ "anything": 1 }), "a2ui-surface")));
    assert!(extension.keeps_activity(&echoed(json!({ "anything": 1 }), "progress")));
}
