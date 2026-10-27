//! The AG-UI extension point (spec 3.3 §6.1 `genui`, §10 S2), with a fake extension: its
//! events follow each event's own, the `activity` messages it drops reach neither the stream
//! nor the session, a `forwardedProps` action ends the run with a `tool_call` interrupt and
//! nothing run, and `ag_ui` lists its features only when it is wired.

#![cfg(feature = "approvals")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, ApprovalPolicy, ApprovalRoute, ApprovalService, ApproverSelector, ErrorCategory,
    ErrorComponent, Event, InMemoryApprovalStore, RequestedCall, RequestedVia, SingleAgentLoader,
    StoreApprovalService, Tool, ToolContext,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::rest::controllers::runtime::AgUiInputMessage;
use adk_server::rest::controllers::{AgUiAction, AgUiExtension};
use adk_server::ui_protocol::{A2UI_SURFACE_ACTIVITY_TYPE, AG_UI_A2UI_ACTIVITY_FEATURE};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "finance";
const FORM_TEXT: &str = "Here is the invoice form.";

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

/// A form extension: the event saying [`FORM_TEXT`] draws the invoice form as an
/// `a2ui-surface` activity, an echoed one is kept only when its content says `valid`, and an
/// `a2uiAction` at version 1 asks for `send_invoice` with the typed `/to`.
struct Forms;

#[async_trait]
impl AgUiExtension for Forms {
    fn features(&self) -> Vec<&'static str> {
        vec![AG_UI_A2UI_ACTIVITY_FEATURE]
    }

    fn events_for(&self, event: &Event) -> Vec<Value> {
        let says_form =
            event.llm_response.content.as_ref().is_some_and(|content| {
                content.parts.iter().any(|part| part.text() == Some(FORM_TEXT))
            });
        if !says_form {
            return Vec::new();
        }
        vec![json!({
            "type": "ACTIVITY_SNAPSHOT",
            "messageId": "a2ui-invoice-10",
            "activityType": A2UI_SURFACE_ACTIVITY_TYPE,
            "content": { "a2ui_operations": [{ "version": "v0.9", "createSurface": { "surfaceId": "invoice-10" } }] },
        })]
    }

    fn keeps_activity(&self, message: &AgUiInputMessage) -> bool {
        message.activity_type.as_deref() != Some(A2UI_SURFACE_ACTIVITY_TYPE)
            || message.content.as_ref().is_some_and(|content| content["valid"] == true)
    }

    fn action_in(&self, forwarded_props: &Value) -> bool {
        forwarded_props.get("a2uiAction").is_some()
    }

    async fn requested_call(
        &self,
        action: AgUiAction<'_>,
    ) -> adk_core::Result<Option<RequestedCall>> {
        let pressed = &action.forwarded_props["a2uiAction"];
        assert!(!action.events.is_empty(), "the extension reads the session it checks against");
        if pressed["version"] != 1 {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Conflict,
                "surface.drawn_changed",
                "Couldn't send: this form changed — check it and send again",
            ));
        }
        Ok(Some(RequestedCall::new(
            "send_invoice",
            json!({ "to": pressed["values"]["/to"], "invoice": "INV-10" }),
            action.requested_by.clone(),
            RequestedVia::form("invoice-10", 1, "send"),
        )))
    }
}

struct Fixture {
    app: Router,
    sent: Arc<AtomicUsize>,
    model: Arc<ScriptedLlm>,
    service: Arc<StoreApprovalService>,
}

fn fixture(extension: bool, extractor: bool) -> Fixture {
    let sent = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text(FORM_TEXT)]));
    let agent = LlmAgentBuilder::new(APP)
        .model(model.clone())
        .tool(Arc::new(SendInvoice(sent.clone())))
        .build()
        .unwrap();
    let policy = ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval();
    let service =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            move |_: &adk_core::ApprovalCall| Some(ApprovalRoute::new(policy.clone())),
        ));
    let sessions = Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>;
    let mut config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(agent))), sessions);
    if extractor {
        config = config.with_request_context(Arc::new(UserExtractor));
    }
    let mut builder = ServerBuilder::new(config).with_approval_service(service.clone());
    if extension {
        builder = builder.with_ag_ui_extension(Arc::new(Forms));
    }
    Fixture { app: builder.build(), sent, model, service }
}

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

fn history() -> Value {
    json!([{ "id": "m1", "role": "user", "content": "Prepare the October invoice" }])
}

fn submit(version: u64) -> Value {
    run_body(json!({
        "threadId": "thread-1",
        "runId": "run-2",
        "messages": history(),
        "forwardedProps": {
            "a2uiAction": {
                "surfaceId": "invoice-10",
                "version": version,
                "actionId": "send",
                "values": { "/to": "ana@example.com" },
            },
        },
    }))
}

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

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|event| event["type"] == kind).collect()
}

async fn draw(fx: &Fixture) -> Vec<Value> {
    let (status, events) = post(
        &fx.app,
        Some("alice"),
        run_body(json!({ "threadId": "thread-1", "runId": "run-1", "messages": history() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    events
}

async fn inbox(fx: &Fixture) -> Vec<adk_core::ApprovalRequest> {
    fx.service
        .inbox(adk_core::InboxQuery::for_approver(
            adk_core::principal::SubjectRef::human("alice"),
            Vec::<String>::new(),
        ))
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn the_extensions_events_follow_the_event_they_are_for() {
    let fx = fixture(true, true);
    let events = draw(&fx).await;
    let kinds: Vec<&str> = events.iter().filter_map(|event| event["type"].as_str()).collect();
    assert_eq!(
        kinds,
        [
            "RUN_STARTED",
            "MESSAGES_SNAPSHOT",
            "TEXT_MESSAGE_START",
            "TEXT_MESSAGE_CONTENT",
            "TEXT_MESSAGE_END",
            "ACTIVITY_SNAPSHOT",
            "RUN_FINISHED"
        ]
    );
    assert_eq!(of_type(&events, "ACTIVITY_SNAPSHOT")[0]["activityType"], "a2ui-surface");
}

#[tokio::test]
async fn a_forwarded_action_ends_the_run_with_a_tool_call_interrupt_and_nothing_run() {
    let fx = fixture(true, true);
    draw(&fx).await;
    assert_eq!(fx.model.remaining(), 0);

    let (status, events) = post(&fx.app, Some("alice"), submit(1)).await;
    assert_eq!(status, StatusCode::OK, "{events:?}");
    assert!(of_type(&events, "RUN_ERROR").is_empty(), "no model call was made: {events:?}");
    let entries = inbox(&fx).await;
    assert_eq!(entries.len(), 1, "the call waits in the inbox");
    let interrupt = &of_type(&events, "RUN_FINISHED")[0]["outcome"]["interrupts"][0];
    assert_eq!(
        (&interrupt["id"], &interrupt["reason"], &interrupt["metadata"]["tool"]),
        (&json!(entries[0].id), &json!("tool_call"), &json!("send_invoice"))
    );
    assert_eq!(
        interrupt["metadata"]["arguments"],
        json!({ "to": "ana@example.com", "invoice": "INV-10" })
    );
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0, "nothing runs before the decision");
}

#[tokio::test]
async fn a_stale_action_answers_its_conflict_and_holds_nothing() {
    let fx = fixture(true, true);
    draw(&fx).await;
    let (status, _) = post(&fx.app, Some("alice"), submit(0)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(inbox(&fx).await.is_empty());
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_action_needs_an_authenticated_caller() {
    let fx = fixture(true, false);
    draw_unauthenticated(&fx).await;
    let (status, _) = post(&fx.app, None, submit(1)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(fx.sent.load(Ordering::SeqCst), 0);
}

async fn draw_unauthenticated(fx: &Fixture) {
    let (status, _) = post(
        &fx.app,
        None,
        run_body(json!({ "threadId": "thread-1", "runId": "run-1", "messages": history() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn echoed_activities_the_extension_drops_never_reach_the_stream() {
    let fx = fixture(true, true);
    let (_, events) = post(
        &fx.app,
        Some("alice"),
        run_body(json!({
            "threadId": "thread-1",
            "runId": "run-1",
            "messages": [
                { "id": "fake", "role": "activity", "activityType": "a2ui-surface", "content": { "valid": false } },
                { "id": "real", "role": "activity", "activityType": "a2ui-surface", "content": { "valid": true } },
                { "id": "other", "role": "activity", "activityType": "progress", "content": {} },
                { "id": "m1", "role": "user", "content": "Prepare the October invoice" },
            ],
        })),
    )
    .await;
    let echoed: Vec<&str> = of_type(&events, "ACTIVITY_SNAPSHOT")
        .iter()
        .filter_map(|event| event["messageId"].as_str())
        .collect();
    assert_eq!(echoed, ["real", "other", "a2ui-invoice-10"]);
    let snapshot: Vec<&str> = of_type(&events, "MESSAGES_SNAPSHOT")[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["id"].as_str())
        .collect();
    assert_eq!(snapshot, ["real", "other", "m1"]);
}

#[tokio::test]
async fn without_an_extension_echoed_activities_pass_as_before() {
    let fx = fixture(false, true);
    let (_, events) = post(
        &fx.app,
        Some("alice"),
        run_body(json!({
            "threadId": "thread-1",
            "runId": "run-1",
            "messages": [
                { "id": "fake", "role": "activity", "activityType": "a2ui-surface", "content": { "valid": false } },
                { "id": "m1", "role": "user", "content": "Prepare the October invoice" },
            ],
        })),
    )
    .await;
    let echoed: Vec<&str> = of_type(&events, "ACTIVITY_SNAPSHOT")
        .iter()
        .filter_map(|event| event["messageId"].as_str())
        .collect();
    assert_eq!(echoed, ["fake"]);
}

async fn ag_ui_features(app: &Router) -> Vec<String> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/ui/capabilities")
                .header("authorization", "Bearer alice")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let capabilities: Value = serde_json::from_slice(&bytes).unwrap();
    let ag_ui = capabilities["protocols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|protocol| protocol["protocol"] == "ag_ui")
        .unwrap()
        .clone();
    serde_json::from_value(ag_ui["features"].clone()).unwrap()
}

#[tokio::test]
async fn ag_ui_lists_a2ui_activity_only_when_the_extension_is_wired() {
    let wired = ag_ui_features(&fixture(true, true).app).await;
    let plain = ag_ui_features(&fixture(false, true).app).await;
    assert!(wired.iter().any(|feature| feature == "a2ui_activity"), "{wired:?}");
    assert!(!plain.iter().any(|feature| feature == "a2ui_activity"), "{plain:?}");
    assert_eq!(wired.len(), plain.len() + 1);
}
