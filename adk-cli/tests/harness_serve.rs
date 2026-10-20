//! The served harness (spec 3.1 §8.2, §11 adk-cli row): the approval inbox needs an
//! authenticated caller, loopback peers are the operator, and a held call decided over HTTP
//! runs exactly once when the session resumes.

#![cfg(feature = "harness")]

use std::net::SocketAddr;
use std::sync::Arc;

use adk_cli::harness::{BearerTokenExtractor, HarnessLauncher, OPERATOR_SCOPE};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore, StoreApprovalService,
};
use adk_harness::{HarnessAgent, HarnessMode};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "harness";

fn approval_service() -> Arc<dyn ApprovalService> {
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(routing))
}

struct Served {
    app: Router,
    workspace: tempfile::TempDir,
}

async fn served(script: Vec<ScriptedResponse>, mode: HarnessMode, token: Option<&str>) -> Served {
    let workspace = tempfile::tempdir().unwrap();
    let service = approval_service();
    let agent = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .workspace(adk_devtools::Workspace::new(workspace.path()).allow_bash(true))
        .mode(mode)
        .approval_service(service.clone())
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut launcher = HarnessLauncher::new(agent)
        .with_session_service(sessions)
        .with_user_id("ana")
        .with_approval_service(service);
    if let Some(token) = token {
        launcher = launcher
            .with_request_context_extractor(Arc::new(BearerTokenExtractor::new(token, "ana")));
    }
    Served { app: launcher.build_app().unwrap(), workspace }
}

fn request(method: &str, uri: &str, body: Option<Value>, peer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let mut request =
        builder.body(body.map_or_else(Body::empty, |body| Body::from(body.to_string()))).unwrap();
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    }
    request
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn run_body(text: Option<&str>) -> Value {
    let parts: Vec<Value> = text.map(|text| json!({ "text": text })).into_iter().collect();
    json!({
        "appName": APP,
        "userId": "ana",
        "sessionId": "s1",
        "newMessage": { "role": "user", "parts": parts },
    })
}

const LOOPBACK: Option<&str> = Some("127.0.0.1:40000");

#[tokio::test]
async fn the_inbox_admits_loopback_operators_and_refuses_everyone_else() {
    let served = served(vec![], HarnessMode::Plan, None).await;
    let (status, _) = call(&served.app, request("GET", "/api/approvals", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) =
        call(&served.app, request("GET", "/api/approvals", None, Some("10.1.2.3:40000"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = call(&served.app, request("GET", "/api/approvals", None, LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"], json!([]));
    // A client cannot claim to be on loopback with the marker header.
    let mut forged = request("GET", "/api/approvals", None, Some("10.1.2.3:40000"));
    forged.headers_mut().insert("x-adk-loopback-peer", "anything".parse().unwrap());
    assert_eq!(call(&served.app, forged).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_bearer_token_is_required_when_configured() {
    let served = served(vec![], HarnessMode::Plan, Some("t0ken")).await;
    let (status, _) = call(&served.app, request("GET", "/api/approvals", None, LOOPBACK)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the token replaces loopback admission");
    let mut authorized = request("GET", "/api/approvals", None, None);
    authorized.headers_mut().insert("authorization", "Bearer t0ken".parse().unwrap());
    assert_eq!(call(&served.app, authorized).await.0, StatusCode::OK);
}

#[test]
fn a_harness_beyond_loopback_needs_an_extractor() {
    let agent = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(std::env::temp_dir()))
        .build()
        .unwrap();
    let error = HarnessLauncher::new(agent)
        .with_bind_address("0.0.0.0".parse().unwrap())
        .build_app()
        .unwrap_err();
    assert!(error.to_string().contains("--auth-token"), "{error}");
}

#[tokio::test]
async fn plan_mode_writes_nothing_over_http() {
    let served = served(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "notes.txt", "content": "hi" }),
            ),
            ScriptedResponse::text("Planned."),
        ],
        HarnessMode::Plan,
        None,
    )
    .await;
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("write"))), LOOPBACK))
            .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert!(events.to_string().contains("denied by policy"), "{events}");
    assert!(!served.workspace.path().join("notes.txt").exists());
}

#[tokio::test]
async fn a_held_call_decided_over_http_runs_once_on_resume() {
    let served = served(
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "echo run >> count.txt" })),
            ScriptedResponse::text("Ran it."),
            ScriptedResponse::text("Nothing else to do."),
        ],
        HarnessMode::Ask,
        None,
    )
    .await;
    let count = served.workspace.path().join("count.txt");

    // The run holds the call and ends; nothing ran.
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("count"))), LOOPBACK))
            .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert!(events.to_string().contains("\"tool_confirmation\""), "{events}");
    assert!(!count.exists());

    // The operator finds it in the inbox and approves it.
    let (_, inbox) = call(&served.app, request("GET", "/api/approvals", None, LOOPBACK)).await;
    let item = &inbox["items"][0];
    assert_eq!(item["toolName"], "bash", "{inbox}");
    let id = item["id"].as_str().unwrap();
    let decide = json!({ "expectedVersion": item["version"], "action": "approve" });
    let (status, decided) = call(
        &served.app,
        request("POST", &format!("/api/approvals/{id}/decide"), Some(decide), LOOPBACK),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{decided}");

    // An empty-message run resumes the held call; it runs exactly once.
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(None)), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n");
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(None)), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n", "the approval was reused");
}
