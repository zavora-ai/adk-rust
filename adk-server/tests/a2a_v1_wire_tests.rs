//! The A2A v1 wire format over HTTP, as another SDK sees it (zavora-ai/adk-rust#1343).
//!
//! Each test serves adk-server's v1 handlers the way the A2A deployment docs mount them (the
//! agent card, JSON-RPC at `/a2a` and HTTP+JSON at `/rest` behind version negotiation) and
//! checks the bytes a client receives against the A2A v1.0.1 specification, or checks that
//! adk-server's own v1 client reads what a conformant server sends.
#![cfg(feature = "a2a-v1")]

use std::sync::Arc;

use a2a_protocol_types::responses::SendMessageResponse;
use a2a_protocol_types::task::TaskState;
use a2a_protocol_types::{
    AgentCapabilities, AgentInterface, Message, MessageId, MessageRole, Part,
};
use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Result as AdkResult};
use adk_server::a2a::client::v1_client::{A2aV1Client, V1ClientError};
use adk_server::a2a::remote_agent::v1_remote::{RemoteA2aV1Agent, RemoteA2aV1Config};
use adk_server::a2a::v1::V1Executor;
use adk_server::a2a::v1::card::{CachedAgentCard, build_v1_agent_card};
use adk_server::a2a::v1::jsonrpc_handler::jsonrpc_handler;
use adk_server::a2a::v1::push::NoOpPushNotificationSender;
use adk_server::a2a::v1::request_handler::RequestHandler;
use adk_server::a2a::v1::rest_handler::rest_router;
use adk_server::a2a::v1::task_store::{InMemoryTaskStore, TaskStore};
use adk_server::a2a::v1::version::version_negotiation;
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::Router;
use axum::routing::{get, post};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::RwLock;

const ANSWER: &str = "Hello from the scripted agent!";

/// Answers every message with [`ANSWER`], or fails with "upstream exploded" when `fail` is set.
struct ScriptedAgent {
    fail: bool,
}

#[async_trait]
impl Agent for ScriptedAgent {
    fn name(&self) -> &str {
        "scripted"
    }

    fn description(&self) -> &str {
        "answers with a fixed text, or fails"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        if self.fail {
            let error = adk_core::AdkError::model("upstream exploded");
            return Ok(Box::pin(futures::stream::iter(vec![Err(error)])));
        }
        let mut event = Event::new("scripted");
        event.llm_response.content = Some(Content::new("model").with_text(ANSWER));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

async fn listen(build: impl FnOnce(&str) -> Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = build(&base);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Serves `agent` over adk-server's v1 handlers and returns the base URL.
async fn serve_v1(fail: bool) -> String {
    listen(|base| {
        let mut card = build_v1_agent_card(
            "scripted",
            "scripted agent",
            &format!("{base}/a2a"),
            "1.0.0",
            vec![],
            AgentCapabilities::none().with_streaming(true),
        );
        card.supported_interfaces.push(AgentInterface {
            url: format!("{base}/rest"),
            protocol_binding: "HTTP+JSON".to_string(),
            protocol_version: "1.0".to_string(),
            tenant: None,
        });
        let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name("wire")
                .agent(Arc::new(ScriptedAgent { fail }))
                .session_service(Arc::new(InMemorySessionService::new()))
                .build_config(),
        );
        let handler = Arc::new(RequestHandler::with_runner(
            Arc::new(V1Executor::new(store.clone())),
            store,
            Arc::new(NoOpPushNotificationSender),
            Arc::new(RwLock::new(CachedAgentCard::new(card.clone()))),
            runner_config,
        ));
        let rpc = Router::new()
            .route("/a2a", post(jsonrpc_handler))
            .with_state(handler.clone())
            .nest("/rest", rest_router(handler))
            .layer(axum::middleware::from_fn(version_negotiation));
        Router::new()
            .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
            .merge(rpc)
    })
    .await
}

/// A scripted JSON-RPC peer that answers every call with `reply` as HTTP 200.
async fn serve_reply(reply: Value) -> String {
    listen(|base| {
        let card = build_v1_agent_card(
            "peer",
            "scripted peer",
            &format!("{base}/a2a"),
            "1.0.0",
            vec![],
            AgentCapabilities::none(),
        );
        Router::new()
            .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
            .route("/a2a", post(move || async move { axum::Json(reply) }))
    })
    .await
}

struct Reply {
    status: u16,
    content_type: String,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

async fn post_raw(url: &str, version: Option<&str>, body: impl Into<reqwest::Body>) -> Reply {
    let mut request =
        reqwest::Client::new().post(url).header("content-type", "application/json").body(body);
    if let Some(version) = version {
        request = request.header("A2A-Version", version);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    Reply { status, content_type, body: response.text().await.unwrap() }
}

fn send_message_request(id: Value, method: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": { "message": { "messageId": "m-1", "role": "ROLE_USER", "parts": [{"text": "Say hello"}] } }
    })
    .to_string()
}

fn user_message(text: &str) -> Message {
    Message {
        id: MessageId::new(uuid::Uuid::new_v4().to_string()),
        role: MessageRole::User,
        parts: vec![Part::text(text)],
        task_id: None,
        context_id: None,
        reference_task_ids: None,
        extensions: None,
        metadata: None,
    }
}

#[tokio::test]
async fn send_message_answers_with_the_task_oneof_on_both_bindings() {
    let base = serve_v1(false).await;

    // JSON-RPC (A2A §9.4.1): `result` is SendMessageResponse, `{"task": …}`; §9.1: application/json.
    let reply = post_raw(
        &format!("{base}/a2a"),
        Some("1.0"),
        send_message_request(json!(1), "SendMessage"),
    )
    .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type, "application/json");
    let body = reply.json();
    assert_eq!(body["id"], 1);
    let keys: Vec<_> = body["result"].as_object().expect("result object").keys().collect();
    assert_eq!(keys, ["task"], "{body}");
    let task = &body["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], ANSWER);
    assert!(task.get("kind").is_none());

    // HTTP+JSON (A2A §6.1, §11.4): the body is the same oneof, as application/a2a+json.
    let rest_body = json!({
        "message": { "messageId": "m-2", "role": "ROLE_USER", "parts": [{"text": "Say hello"}] }
    });
    let reply =
        post_raw(&format!("{base}/rest/message:send"), Some("1.0"), rest_body.to_string()).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type, "application/a2a+json");
    let body = reply.json();
    let keys: Vec<_> = body.as_object().expect("object body").keys().collect();
    assert_eq!(keys, ["task"], "{body}");
    assert_eq!(body["task"]["status"]["state"], "TASK_STATE_COMPLETED");
}

#[tokio::test]
async fn envelope_errors_are_json_rpc_errors() {
    let base = serve_v1(false).await;
    let url = format!("{base}/a2a");

    // A2A §9.5: -32700 for invalid JSON, -32600 for JSON that is not a request object.
    let cases = [
        ("{not json".to_string(), -32700, Value::Null),
        (json!({"jsonrpc": "2.0", "id": 1, "params": {}}).to_string(), -32600, json!(1)),
        (
            json!({"jsonrpc": "2.0", "id": 2, "method": "NoSuchMethod", "params": {}}).to_string(),
            -32601,
            json!(2),
        ),
    ];
    for (body, code, id) in cases {
        let reply = post_raw(&url, Some("1.0"), body.clone()).await;
        assert_eq!(reply.status, 200, "{body}");
        assert_eq!(reply.content_type, "application/json", "{body}");
        let reply = reply.json();
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["error"]["code"], code, "{body} -> {reply}");
        assert_eq!(reply["id"], id, "{body} -> {reply}");
    }
}

#[tokio::test]
async fn an_unsupported_or_missing_version_gets_the_binding_s_version_error() {
    let base = serve_v1(false).await;

    // JSON-RPC: -32009 in a JSON-RPC error that answers the request id (§3.6.2, §5.4, §9.5). No
    // header means 0.3 (§3.6.2), which the v1 handlers do not serve.
    for version in [Some("99.0"), None] {
        let reply = post_raw(
            &format!("{base}/a2a"),
            version,
            send_message_request(json!("v-1"), "SendMessage"),
        )
        .await;
        assert_eq!(reply.status, 200, "{version:?}");
        assert_eq!(reply.content_type, "application/json", "{version:?}");
        let body = reply.json();
        assert_eq!(body["id"], "v-1");
        assert_eq!(body["error"]["code"], -32009, "{version:?} -> {body}");
    }

    // A patch number does not take part in negotiation (§3.6).
    let reply = post_raw(
        &format!("{base}/a2a"),
        Some("1.0.1"),
        send_message_request(json!(3), "SendMessage"),
    )
    .await;
    assert!(reply.json()["result"]["task"].is_object(), "{}", reply.body);

    // HTTP+JSON: HTTP 400 with an AIP-193 body (§11.6).
    let reply =
        post_raw(&format!("{base}/rest/message:send"), Some("99.0"), r#"{"message":{}}"#).await;
    assert_eq!(reply.status, 400);
    assert_eq!(reply.json()["error"]["status"], "FAILED_PRECONDITION");
}

#[tokio::test]
async fn a_failed_stream_ends_on_a_failed_status_that_carries_the_error() {
    let base = serve_v1(true).await;
    let reply = post_raw(
        &format!("{base}/a2a"),
        Some("1.0"),
        send_message_request(json!(9), "SendStreamingMessage"),
    )
    .await;
    assert_eq!(reply.status, 200);
    assert!(reply.content_type.starts_with("text/event-stream"), "{}", reply.content_type);

    let frames: Vec<Value> = reply
        .body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| serde_json::from_str(data.trim()).unwrap())
        .collect();
    assert!(
        frames.iter().all(|frame| frame.get("error").is_none()),
        "no error frame: the error rides on the terminal status: {frames:#?}"
    );
    // A2A §3.1.2: the stream closes at the terminal state, so the Failed status is the last frame.
    let last = &frames.last().expect("frames")["result"]["statusUpdate"]["status"];
    assert_eq!(last["state"], "TASK_STATE_FAILED", "{frames:#?}");
    let text = last["message"]["parts"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("upstream exploded"), "{text}");
}

#[tokio::test]
async fn the_v1_client_reads_the_oneof() {
    let base = serve_v1(false).await;
    let card = A2aV1Client::resolve_agent_card(&base).await.unwrap();
    let client = A2aV1Client::new(card);

    for reply in [
        client.send_message(user_message("Say hello")).await.unwrap(),
        client.send_message_rest(user_message("Say hello")).await.unwrap(),
    ] {
        match reply {
            SendMessageResponse::Task(task) => assert_eq!(task.status.state, TaskState::Completed),
            other => panic!("expected a task, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_v1_client_reads_a_message_reply_and_an_older_bare_task() {
    let message =
        json!({ "messageId": "m-9", "role": "ROLE_AGENT", "parts": [{"text": "direct"}] });
    let base =
        serve_reply(json!({"jsonrpc": "2.0", "id": 1, "result": {"message": message}})).await;
    let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
    match client.send_message(user_message("hi")).await.unwrap() {
        SendMessageResponse::Message(reply) => assert_eq!(reply.id.0, "m-9"),
        other => panic!("expected a message, got {other:?}"),
    }

    // adk-server before 3.0 put the Task itself in `result`.
    let bare =
        json!({"id": "t-1", "contextId": "c-1", "status": {"state": "TASK_STATE_COMPLETED"}});
    let base = serve_reply(json!({"jsonrpc": "2.0", "id": 1, "result": bare})).await;
    let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
    match client.send_message(user_message("hi")).await.unwrap() {
        SendMessageResponse::Task(task) => assert_eq!(task.id.0, "t-1"),
        other => panic!("expected a task, got {other:?}"),
    }
}

#[tokio::test]
async fn the_v1_client_reads_a_version_error_sent_as_http_200() {
    let error = json!({
        "code": -32009,
        "message": "Version not supported: 1.0",
        "data": [{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": "VERSION_NOT_SUPPORTED",
            "domain": "a2a-protocol.org",
            "metadata": {"requested": "1.0", "supported": "2.0"}
        }]
    });
    let base = serve_reply(json!({"jsonrpc": "2.0", "id": 1, "error": error})).await;
    let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
    match client.get_task("t-1", None).await.unwrap_err() {
        V1ClientError::VersionNotSupported { supported, .. } => assert_eq!(supported, ["2.0"]),
        other => panic!("expected VersionNotSupported, got {other}"),
    }
}

/// Runs a `RemoteA2aV1Agent` for the server at `base` once and returns its events.
async fn run_remote(base: &str, streaming: bool) -> Vec<Event> {
    let card = A2aV1Client::resolve_agent_card(base).await.unwrap();
    let agent = RemoteA2aV1Agent::new(RemoteA2aV1Config {
        name: "remote".to_string(),
        description: "the scripted agent over A2A".to_string(),
        agent_card: card,
        streaming: Some(streaming),
        credential_broker: None,
        credential_scopes: vec![],
        send_configuration: None,
    });
    let sessions = Arc::new(InMemorySessionService::new());
    adk_session::SessionService::create(
        sessions.as_ref(),
        adk_session::CreateRequest {
            app_name: "client".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: Default::default(),
        },
    )
    .await
    .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("client")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()
        .unwrap();
    runner
        .run_str("user", "session", Content::new("user").with_text("Say hello"))
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await
}

fn texts(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(adk_core::Part::text)
        .collect()
}

#[tokio::test]
async fn the_non_streaming_remote_agent_reads_the_oneof() {
    let base = serve_v1(false).await;
    let events = run_remote(&base, false).await;
    assert_eq!(texts(&events), ANSWER);
    assert!(events.iter().all(|event| event.llm_response.error_message.is_none()), "{events:#?}");
}

#[tokio::test]
async fn the_remote_agent_reports_a_remote_failure_once_as_an_error() {
    let base = serve_v1(true).await;
    for streaming in [true, false] {
        let events = run_remote(&base, streaming).await;
        let errors: Vec<_> =
            events.iter().filter_map(|event| event.llm_response.error_message.clone()).collect();
        assert_eq!(errors.len(), 1, "streaming={streaming}: {errors:?}");
        assert!(errors[0].contains("upstream exploded"), "{errors:?}");
        assert_eq!(texts(&events), "", "an error is not model output");
    }
}
