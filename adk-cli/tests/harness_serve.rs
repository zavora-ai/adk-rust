//! The served harness (spec 3.1 §8.2, §11 adk-cli row): the approval inbox needs an
//! authenticated caller, loopback peers are the operator, a held call decided over HTTP
//! runs exactly once when the session resumes, and standing rules are created and revoked
//! over authenticated REST (§7.5).

#![cfg(feature = "harness")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use adk_cli::harness::{BearerTokenExtractor, HarnessLauncher, OPERATOR_SCOPE};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore, RequestContext,
    StoreApprovalService,
};
use adk_harness::{EphemeralAuditLog, HarnessAgent, HarnessMode};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::{RequestContextError, RequestContextExtractor};
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
    let extractor = token.map(|token| {
        Arc::new(BearerTokenExtractor::new(token, "ana")) as Arc<dyn RequestContextExtractor>
    });
    served_with(script, mode, extractor, None).await
}

/// A served harness as `adk-rust serve` builds it: a turn runs as the operator only when its
/// caller is authenticated, else Auto asks and standing rules are skipped (spec 3.1 §8.2).
async fn served_gated(script: Vec<ScriptedResponse>, mode: HarnessMode) -> Served {
    served_with_gate(script, mode, None, None, true).await
}

/// A served harness with sessions `s1` and `s2` of `ana`, authenticating with `extractor`
/// (loopback peers when `None`) and recording into `audit`.
async fn served_with(
    script: Vec<ScriptedResponse>,
    mode: HarnessMode,
    extractor: Option<Arc<dyn RequestContextExtractor>>,
    audit: Option<Arc<EphemeralAuditLog>>,
) -> Served {
    served_with_gate(script, mode, extractor, audit, false).await
}

async fn served_with_gate(
    script: Vec<ScriptedResponse>,
    mode: HarnessMode,
    extractor: Option<Arc<dyn RequestContextExtractor>>,
    audit: Option<Arc<EphemeralAuditLog>>,
    identity_gate: bool,
) -> Served {
    let workspace = tempfile::tempdir().unwrap();
    let service = approval_service();
    let mut builder = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .workspace(adk_devtools::Workspace::new(workspace.path()).allow_bash(true))
        .mode(mode)
        .approval_service(service.clone());
    if let Some(audit) = audit {
        builder = builder.audit_log(audit);
    }
    if identity_gate {
        builder = builder.require_authenticated_identity();
    }
    let agent = builder.build().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    for session in ["s1", "s2"] {
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: "ana".into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await
            .unwrap();
    }
    let mut launcher = HarnessLauncher::new(agent)
        .with_session_service(sessions)
        .with_user_id("ana")
        .with_approval_service(service);
    if let Some(extractor) = extractor {
        launcher = launcher.with_request_context_extractor(extractor);
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
    run_body_in("s1", text)
}

fn run_body_in(session: &str, text: Option<&str>) -> Value {
    let parts: Vec<Value> = text.map(|text| json!({ "text": text })).into_iter().collect();
    json!({
        "appName": APP,
        "userId": "ana",
        "sessionId": session,
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

/// The served harness mounts the web home (spec 3.3 §5.1, §10 S1): the page at `/ui/`, its
/// API for the loopback operator only (§11 S-I8); a held call is a card the home allows once
/// with the version it saw, a second answer is "already decided", and the call runs once.
#[cfg(feature = "surface")]
#[tokio::test]
async fn the_web_home_allows_a_held_call_once() {
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

    let page = served.app.clone().oneshot(request("GET", "/ui/", None, LOOPBACK)).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
    let (status, _) = call(
        &served.app,
        request("GET", "/api/surface/status?surface=web", None, Some("203.0.113.9:40000")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the home's API is the operator's only");

    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("count"))), LOOPBACK))
            .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    let (status, page) =
        call(&served.app, request("GET", "/api/surface/decisions?surface=web", None, LOOPBACK))
            .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let card = &page["decisions"][0];
    assert_eq!(card["call"]["tool"], "bash", "{page}");
    assert_eq!(card["statusText"], "Waiting for you", "{page}");
    let decide = |answer: &str| {
        json!({
            "surface": "web",
            "origin": { "kind": "pointer" },
            "command": {
                "type": "decide",
                "decisionId": card["id"],
                "version": card["version"],
                "answer": answer,
                "expanded": true,
            },
        })
    };
    let (status, outcome) = call(
        &served.app,
        request("POST", "/api/surface/commands", Some(decide("allowOnce")), LOOPBACK),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["type"], "answered", "{outcome}");
    let (_, again) =
        call(&served.app, request("POST", "/api/surface/commands", Some(decide("deny")), LOOPBACK))
            .await;
    assert_eq!(again["type"], "alreadyDecided", "{again}");

    // The engine resumes the session; the call runs once.
    for _ in 0..200 {
        if count.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n");
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(None)), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n", "the answer was reused");
}

#[cfg(feature = "surface")]
fn home_command(command: Value) -> Value {
    json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": command })
}

/// The home's Stop all freezes what the server runs, not only the home's own turns (spec 3.3
/// §11 S-I10): a run started over `POST /api/run` afterwards does not run, until Resume all.
/// Auto mode runs the edit without approval, so only the stop can keep it from running.
#[cfg(feature = "surface")]
#[tokio::test]
async fn the_home_s_stop_all_stops_the_server_s_runs() {
    let served = served(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "count.txt", "content": "run\n" }),
            ),
            ScriptedResponse::text("Ran it."),
        ],
        HarnessMode::Auto,
        None,
    )
    .await;
    let count = served.workspace.path().join("count.txt");
    let stop = home_command(json!({ "type": "stopAll" }));
    let (status, outcome) =
        call(&served.app, request("POST", "/api/surface/commands", Some(stop), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["type"], "stopped", "{outcome}");

    let (_, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("count"))), LOOPBACK))
            .await;
    assert!(events.to_string().contains("governance.frozen"), "{events}");
    assert!(!count.exists(), "nothing ran while stopped");

    // Resume all is the home's from its own page, a browser's same-origin request (S-I10).
    let resume = home_command(json!({ "type": "resumeAll" }));
    let mut resume = request("POST", "/api/surface/commands", Some(resume), LOOPBACK);
    resume.headers_mut().insert("sec-fetch-site", "same-origin".parse().unwrap());
    let (_, outcome) = call(&served.app, resume).await;
    assert_eq!(outcome["type"], "resumed", "{outcome}");
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("count"))), LOOPBACK))
            .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n");
}

/// A message typed in the home runs as the operator the server admitted (spec 3.3 §5.1,
/// spec 3.1 §8.2): in Auto mode its edit runs without a decision, as the same message over
/// `POST /api/run` does; an unauthenticated turn would be held as in Ask.
#[cfg(feature = "surface")]
#[tokio::test]
async fn the_home_s_turns_run_as_the_operator() {
    let served = served_gated(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "count.txt", "content": "run\n" }),
            ),
            ScriptedResponse::text("Ran it."),
        ],
        HarnessMode::Auto,
    )
    .await;
    let count = served.workspace.path().join("count.txt");
    let send = home_command(json!({ "type": "send", "sessionId": "s1", "text": "count" }));
    let (status, outcome) =
        call(&served.app, request("POST", "/api/surface/commands", Some(send), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    for _ in 0..200 {
        if count.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let (_, page) =
        call(&served.app, request("GET", "/api/surface/decisions?surface=web", None, LOOPBACK))
            .await;
    assert_eq!(page["decisions"], json!([]), "Auto asked nothing: {page}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n");
}

/// Callers named by the `x-user` header with the scopes in `x-scopes`; no header, no caller.
struct HeaderIdentity;

#[async_trait::async_trait]
impl RequestContextExtractor for HeaderIdentity {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let header = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());
        let user = header("x-user").ok_or(RequestContextError::MissingAuth)?;
        Ok(RequestContext {
            user_id: user.to_string(),
            scopes: header("x-scopes")
                .map(|scopes| scopes.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            metadata: HashMap::new(),
        })
    }
}

fn as_caller(mut request: Request<Body>, user: &str, scopes: &str) -> Request<Body> {
    request.headers_mut().insert("x-user", user.parse().unwrap());
    request.headers_mut().insert("x-scopes", scopes.parse().unwrap());
    request
}

fn rule_records(records: &[adk_core::audit_log::AuditRecord], action: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|record| record.action == action)
        .map(|record| serde_json::to_value(&record.ext).unwrap())
        .collect()
}

#[tokio::test]
async fn rule_routes_need_an_authenticated_operator() {
    let served = served_with(vec![], HarnessMode::Ask, Some(Arc::new(HeaderIdentity)), None).await;
    let create = || json!({ "tool": "bash", "prefix": "echo" });
    // No caller: 401 on every route.
    for request in [
        request("GET", "/api/rules", None, None),
        request("POST", "/api/rules", Some(create()), None),
        request("DELETE", "/api/rules/r1", None, None),
        request("GET", "/api/rules/suggestions/apr_1", None, None),
    ] {
        assert_eq!(call(&served.app, request).await.0, StatusCode::UNAUTHORIZED);
    }
    // A caller without the operator scope: 403, and nothing is stored.
    for request in [
        request("GET", "/api/rules", None, None),
        request("POST", "/api/rules", Some(create()), None),
        request("DELETE", "/api/rules/r1", None, None),
        request("GET", "/api/rules/suggestions/apr_1", None, None),
    ] {
        let (status, body) = call(&served.app, as_caller(request, "ana", "viewer")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let (status, body) = call(
        &served.app,
        as_caller(request("GET", "/api/rules", None, None), "ana", OPERATOR_SCOPE),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rules"], json!([]));
}

#[tokio::test]
async fn a_rule_body_cannot_name_its_subject_or_cover_what_no_rule_may() {
    let served = served(vec![], HarnessMode::Ask, None).await;
    let post = |body: Value| request("POST", "/api/rules", Some(body), LOOPBACK);
    // The subject is the caller (spec 3.1 §9.2 I2, I5): a body naming one is refused.
    let (status, _) =
        call(&served.app, post(json!({ "tool": "bash", "prefix": "echo", "userId": "mallory" })))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    for (body, why) in [
        (json!({ "tool": "bash" }), "a bash allow rule needs `prefix`"),
        (json!({ "tool": "bash", "prefix": "sudo ls" }), "cannot start with `sudo`"),
        (json!({ "tool": "bash", "prefix": "ls; rm -rf ~" }), "plain command words"),
        (json!({ "tool": "exit_plan_mode" }), "never covered by a standing rule"),
        (json!({ "tool": "write_file", "pathWithin": "../etc" }), "inside the workspace"),
        (json!({ "tool": "rm_rf" }), "is not a tool this harness declares"),
        (json!({ "approvalId": "apr_1" }), "`suggestion`"),
    ] {
        let (status, error) = call(&served.app, post(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {error}");
        assert!(error.to_string().contains(why), "{body}: {error}");
    }
    let (status, error) =
        call(&served.app, post(json!({ "approvalId": "apr_1", "suggestion": 0 }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    let (_, listed) = call(&served.app, request("GET", "/api/rules", None, LOOPBACK)).await;
    assert_eq!(listed["rules"], json!([]), "nothing was stored");
}

#[tokio::test]
async fn a_rest_rule_covers_the_next_call_until_it_is_revoked() {
    let audit = Arc::new(EphemeralAuditLog::new().unwrap());
    // A shell-prefix rule matches only a command of plain words (spec 3.1 §7.5).
    let served = served_with(
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "touch one.txt" })),
            ScriptedResponse::text("Done."),
            ScriptedResponse::function_call("bash", json!({ "command": "touch two.txt" })),
            ScriptedResponse::text("Waiting."),
        ],
        HarnessMode::Ask,
        None,
        Some(audit.clone()),
    )
    .await;
    let path = |name: &str| served.workspace.path().join(name);

    let (status, created) = call(
        &served.app,
        request("POST", "/api/rules", Some(json!({ "tool": "bash", "prefix": "touch" })), LOOPBACK),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let rule = &created["rule"];
    assert_eq!(rule["scope"]["userId"], "ana", "the subject is the authenticated caller");
    assert_eq!(rule["createdBy"], json!({ "kind": "human", "id": "ana" }));
    let id = rule["id"].as_str().unwrap().to_string();
    let (_, listed) = call(&served.app, request("GET", "/api/rules", None, LOOPBACK)).await;
    assert_eq!(listed["rules"][0]["id"], id.as_str());

    // The next call matches the rule: it runs with no hold.
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("one"))), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert!(!events.to_string().contains("\"tool_confirmation\""), "{events}");
    assert!(path("one.txt").exists(), "{events}");

    // Revoked, the rule no longer applies: the next call is held and does not run.
    let (status, revoked) =
        call(&served.app, request("DELETE", &format!("/api/rules/{id}"), None, LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let (status, _) =
        call(&served.app, request("DELETE", &format!("/api/rules/{id}"), None, LOOPBACK)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a rule is revoked once");
    let (status, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("two"))), LOOPBACK)).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert!(events.to_string().contains("\"tool_confirmation\""), "{events}");
    assert!(!path("two.txt").exists());

    let records = audit.records().await.unwrap();
    let created = rule_records(&records, "harness.rule.created");
    let revoked = rule_records(&records, "harness.rule.revoked");
    assert_eq!(created.len(), 1, "{records:?}");
    assert_eq!(revoked.len(), 1, "{records:?}");
    for ext in [&created[0], &revoked[0]] {
        assert_eq!(ext["ruleId"], id.as_str());
        assert_eq!(ext["surface"], "rest");
        assert_eq!(ext["subject"], "ana");
        assert_eq!(ext["by"], "ana");
    }
}

#[tokio::test]
async fn a_rule_from_a_held_calls_suggestion_covers_the_next_call() {
    let audit = Arc::new(EphemeralAuditLog::new().unwrap());
    let served = served_with(
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "touch held.txt" })),
            ScriptedResponse::function_call("bash", json!({ "command": "touch next.txt" })),
            ScriptedResponse::text("Done."),
        ],
        HarnessMode::Ask,
        None,
        Some(audit.clone()),
    )
    .await;
    let path = |name: &str| served.workspace.path().join(name);

    let (_, events) =
        call(&served.app, request("POST", "/api/run", Some(run_body(Some("held"))), LOOPBACK))
            .await;
    assert!(events.to_string().contains("\"tool_confirmation\""), "{events}");
    let (_, inbox) = call(&served.app, request("GET", "/api/approvals", None, LOOPBACK)).await;
    let approval = inbox["items"][0]["id"].as_str().unwrap().to_string();

    let (status, offered) = call(
        &served.app,
        request("GET", &format!("/api/rules/suggestions/{approval}"), None, LOOPBACK),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{offered}");
    let labels: Vec<&str> = offered["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|suggestion| suggestion["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["bash: touch held.txt …", "bash: touch …"]);

    let (status, created) = call(
        &served.app,
        request(
            "POST",
            "/api/rules",
            Some(json!({ "approvalId": approval, "suggestion": 1 })),
            LOOPBACK,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["rule"]["scope"]["userId"], "ana");
    assert!(created["rule"]["sourceFingerprint"].is_string(), "{created}");

    // The rule covers the next matching call, here in another session of the same user.
    let (status, events) = call(
        &served.app,
        request("POST", "/api/run", Some(run_body_in("s2", Some("next"))), LOOPBACK),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert!(!events.to_string().contains("\"tool_confirmation\""), "{events}");
    assert!(path("next.txt").exists(), "{events}");
    assert!(!path("held.txt").exists(), "creating the rule decides no held call");

    let records = audit.records().await.unwrap();
    let created = rule_records(&records, "harness.rule.created");
    assert_eq!(created.len(), 1, "{records:?}");
    assert_eq!(created[0]["surface"], "rest");
    assert!(created[0]["createdFromCallId"].is_string(), "{:?}", created[0]);
}

/// "Allowed without asking — Extend" (spec 3.3 §8.1): a rule with an end date is extended
/// to a later one, recorded with its old and new end; a rule without one, an earlier end, a
/// subject in the body, another person and a rule stopped meanwhile are refused, and an
/// extension never brings a stopped rule back.
#[tokio::test]
async fn a_rule_is_extended_to_a_later_end_and_never_brought_back_once_stopped() {
    let audit = Arc::new(EphemeralAuditLog::new().unwrap());
    let served =
        served_with(vec![], HarnessMode::Ask, Some(Arc::new(HeaderIdentity)), Some(audit.clone()))
            .await;
    let as_ana = |request| as_caller(request, "ana", OPERATOR_SCOPE);
    let create = |body: Value| as_ana(request("POST", "/api/rules", Some(body), None));
    let (status, created) =
        call(&served.app, create(json!({ "tool": "bash", "prefix": "ls", "expiresInHours": 24 })))
            .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["rule"]["id"].as_str().unwrap().to_string();
    let end: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(created["rule"]["expiresAt"].clone()).unwrap();
    let extend = |id: &str, body: Value| {
        request("POST", &format!("/api/rules/{id}/extend"), Some(body), None)
    };
    let later = end + chrono::Duration::days(30);

    let (status, extended) =
        call(&served.app, as_ana(extend(&id, json!({ "expiresAt": later })))).await;
    assert_eq!(status, StatusCode::OK, "{extended}");
    assert_eq!(
        serde_json::from_value::<chrono::DateTime<chrono::Utc>>(
            extended["rule"]["expiresAt"].clone()
        )
        .unwrap(),
        later
    );
    let (_, listed) = call(&served.app, as_ana(request("GET", "/api/rules", None, None))).await;
    assert_eq!(listed["rules"][0], extended["rule"], "the rule is stored with its new end");

    // An earlier end, a rule without one, or a body naming a subject is refused.
    let (status, error) = call(&served.app, as_ana(extend(&id, json!({ "expiresAt": end })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(error.to_string().contains("later end"), "{error}");
    let (_, endless) = call(&served.app, create(json!({ "tool": "bash", "prefix": "pwd" }))).await;
    let endless = endless["rule"]["id"].as_str().unwrap();
    let (status, error) =
        call(&served.app, as_ana(extend(endless, json!({ "expiresAt": later })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(error.to_string().contains("no end date"), "{error}");
    let (status, _) =
        call(&served.app, as_ana(extend(&id, json!({ "expiresAt": later, "userId": "bo" })))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Another person, or no operator scope, extends nothing.
    let (status, _) = call(
        &served.app,
        as_caller(extend(&id, json!({ "expiresAt": later })), "bo", OPERATOR_SCOPE),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) =
        call(&served.app, as_caller(extend(&id, json!({ "expiresAt": later })), "ana", "viewer"))
            .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Stopped, the rule stays stopped.
    let (status, _) =
        call(&served.app, as_ana(request("DELETE", &format!("/api/rules/{id}"), None, None))).await;
    assert_eq!(status, StatusCode::OK);
    let much_later = later + chrono::Duration::days(1);
    let (status, _) =
        call(&served.app, as_ana(extend(&id, json!({ "expiresAt": much_later })))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, listed) = call(&served.app, as_ana(request("GET", "/api/rules", None, None))).await;
    assert!(listed["rules"].as_array().unwrap().iter().all(|rule| rule["id"] != id.as_str()));

    let records = audit.records().await.unwrap();
    let extended = rule_records(&records, "harness.rule.extended");
    assert_eq!(extended.len(), 1, "only the extension that took effect is recorded: {records:?}");
    assert_eq!(extended[0]["ruleId"], id.as_str());
    assert_eq!(extended[0]["surface"], "rest");
    assert_eq!(extended[0]["by"], "ana");
    assert_eq!(extended[0]["from"], json!(end));
    assert_eq!(extended[0]["to"], json!(later));
}

#[tokio::test]
async fn a_caller_lists_and_revokes_only_their_own_rules() {
    let served = served_with(vec![], HarnessMode::Ask, Some(Arc::new(HeaderIdentity)), None).await;
    let (status, created) = call(
        &served.app,
        as_caller(
            request("POST", "/api/rules", Some(json!({ "tool": "bash", "prefix": "ls" })), None),
            "ana",
            OPERATOR_SCOPE,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["rule"]["id"].as_str().unwrap();
    let as_bo = |request| as_caller(request, "bo", OPERATOR_SCOPE);
    let (_, listed) = call(&served.app, as_bo(request("GET", "/api/rules", None, None))).await;
    assert_eq!(listed["rules"], json!([]));
    let (status, _) =
        call(&served.app, as_bo(request("DELETE", &format!("/api/rules/{id}"), None, None))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, listed) = call(
        &served.app,
        as_caller(request("GET", "/api/rules", None, None), "ana", OPERATOR_SCOPE),
    )
    .await;
    assert_eq!(listed["rules"][0]["id"], id, "bo could not revoke ana's rule");
}

/// Stop all and Resume all freeze and unfreeze every run the server makes, so behind a
/// custom extractor only the operator gives them (spec 3.3 §11 S-I10); another caller the
/// server admits gets `403`, and the server's runs carry on.
#[cfg(feature = "surface")]
#[tokio::test]
async fn only_the_operator_stops_or_resumes_every_agent() {
    let served = served_with(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "count.txt", "content": "run\n" }),
            ),
            ScriptedResponse::text("Ran it."),
        ],
        HarnessMode::Auto,
        Some(Arc::new(HeaderIdentity)),
        None,
    )
    .await;
    let count = served.workspace.path().join("count.txt");
    let command = |kind: &str| Some(home_command(json!({ "type": kind })));
    for kind in ["stopAll", "resumeAll"] {
        let viewer = as_caller(
            request("POST", "/api/surface/commands", command(kind), None),
            "ana",
            "viewer",
        );
        let (status, body) = call(&served.app, viewer).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind}: {body}");
        assert_eq!(body["error"]["code"], "surface.operator_only", "{body}");
    }
    let run = as_caller(
        request("POST", "/api/run", Some(run_body(Some("count"))), None),
        "ana",
        "viewer",
    );
    let (status, events) = call(&served.app, run).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n", "the viewer stopped nothing");

    let operator = as_caller(
        request("POST", "/api/surface/commands", command("stopAll"), None),
        "ana",
        OPERATOR_SCOPE,
    );
    let (status, outcome) = call(&served.app, operator).await;
    assert_eq!((status, outcome["type"].clone()), (StatusCode::OK, json!("stopped")), "{outcome}");
}

/// Behind a custom extractor too, a message typed in the home runs as the caller the server
/// admitted (spec 3.3 §5.1, spec 3.1 §8.2): in Auto mode the edit runs without a decision.
#[cfg(feature = "surface")]
#[tokio::test]
async fn the_home_s_turns_run_as_the_caller_behind_any_extractor() {
    let served = served_with_gate(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "count.txt", "content": "run\n" }),
            ),
            ScriptedResponse::text("Ran it."),
        ],
        HarnessMode::Auto,
        Some(Arc::new(HeaderIdentity)),
        None,
        true,
    )
    .await;
    let count = served.workspace.path().join("count.txt");
    let send = home_command(json!({ "type": "send", "sessionId": "s1", "text": "count" }));
    let (status, outcome) = call(
        &served.app,
        as_caller(
            request("POST", "/api/surface/commands", Some(send), None),
            "ana",
            OPERATOR_SCOPE,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    for _ in 0..200 {
        if count.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let (_, page) = call(
        &served.app,
        as_caller(
            request("GET", "/api/surface/decisions?surface=web", None, None),
            "ana",
            OPERATOR_SCOPE,
        ),
    )
    .await;
    assert_eq!(page["decisions"], json!([]), "Auto asked nothing: {page}");
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n");
}
