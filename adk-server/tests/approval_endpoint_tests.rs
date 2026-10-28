//! The approval inbox endpoints: authentication, inbox, quorum decisions, conflicts,
//! self-approval, delegation, cancellation, and the signed webhook notifier.

#![cfg(feature = "approvals")]

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    Agent, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService, ApprovalStore,
    ApproverSelector, EventStream, InMemoryApprovalStore, InvocationContext, Result as AdkResult,
    SingleAgentLoader, StoreApprovalService, ToolApproval, ToolConfirmationRequest,
};
use adk_server::approvals::WebhookApprovalNotifier;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Idle;

#[async_trait]
impl Agent for Idle {
    fn name(&self) -> &str {
        "idle"
    }
    fn description(&self) -> &str {
        "does nothing"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// `Bearer <user>` authenticates `<user>`; everyone but `eve` is in `finance`.
struct UserExtractor;

#[async_trait]
impl RequestContextExtractor for UserExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let header = parts.headers.get("authorization").and_then(|value| value.to_str().ok());
        let user = header
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(RequestContextError::MissingAuth)?;
        let scopes = if user == "eve" { vec![] } else { vec!["finance".to_string()] };
        Ok(adk_core::RequestContext {
            user_id: user.to_string(),
            scopes,
            metadata: Default::default(),
        })
    }
}

struct Fixture {
    app: Router,
    service: Arc<StoreApprovalService>,
    store: Arc<InMemoryApprovalStore>,
}

fn fixture() -> Fixture {
    let store = Arc::new(InMemoryApprovalStore::new());
    let service = Arc::new(StoreApprovalService::new(store.clone()));
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(Idle)));
    let sessions = Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>;
    let config = ServerConfig::new(loader, sessions).with_request_context(Arc::new(UserExtractor));
    let app = ServerBuilder::new(config).with_approval_service(service.clone()).build();
    Fixture { app, service, store }
}

async fn submit(fx: &Fixture, quorum: u32) -> ApprovalRequest {
    submit_in(fx, "carol", "s1", quorum).await
}

async fn submit_in(fx: &Fixture, user: &str, session: &str, quorum: u32) -> ApprovalRequest {
    let call = ToolConfirmationRequest {
        tool_name: "transfer".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "amount": 900, "iban": "DE00 secret" }),
        acting: None,
        origin: None,
    };
    let scope = ApprovalScope::new("bank", user, session);
    fx.store.record_pending(&scope, &call).await.unwrap();
    let request = ApprovalRequest::new(
        uuid::Uuid::new_v4().to_string(),
        scope,
        &call,
        ApprovalPolicy::new([ApproverSelector::group("finance")]).with_quorum(quorum),
    )
    .with_redacted_args(json!({ "amount": 900, "iban": "DE00 ****" }))
    .with_requester(SubjectRef::human(user));
    fx.service.submit(request).await.unwrap()
}

async fn call(
    fx: &Fixture,
    method: &str,
    uri: &str,
    user: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header("authorization", format!("Bearer {user}"));
    }
    let request = if body.is_null() {
        builder.body(Body::empty()).unwrap()
    } else {
        builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = fx.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn every_route_needs_an_authenticated_caller() {
    let fx = fixture();
    let (status, _) = call(&fx, "GET", "/api/approvals", None, Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_quorum_of_two_is_reached_through_the_endpoints() {
    let fx = fixture();
    let request = submit(&fx, 2).await;

    let (status, inbox) = call(&fx, "GET", "/api/approvals", Some("ana"), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox["items"][0]["id"], request.id.as_str());
    assert_eq!(inbox["items"][0]["argsRedacted"]["iban"], "DE00 ****");
    assert!(inbox["items"][0].get("fingerprint").is_none(), "the raw arguments stay hidden");
    let (_, other) = call(&fx, "GET", "/api/approvals", Some("eve"), Value::Null).await;
    assert_eq!(other["items"], json!([]), "eve is not an approver");
    let (status, _) =
        call(&fx, "GET", &format!("/api/approvals/{}", request.id), Some("eve"), Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let decide = |version: u64| json!({ "expectedVersion": version, "action": "approve" });
    let uri = format!("/api/approvals/{}/decide", request.id);
    let (status, body) = call(&fx, "POST", &uri, Some("carol"), decide(1)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "approval.self_approval");

    let (status, first) = call(&fx, "POST", &uri, Some("ana"), decide(1)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["state"], "pending");
    assert!(!fx.store.has_decision(&request.scope, &request.fingerprint).await.unwrap());

    let (status, stale) = call(&fx, "POST", &uri, Some("bo"), decide(1)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "approval.version_conflict");

    let (status, second) = call(&fx, "POST", &uri, Some("bo"), decide(2)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["state"], "approved");
    assert_eq!(
        fx.store.take_approval(&request.scope, &request.fingerprint).await.unwrap(),
        Some(ToolApproval::approve())
    );
}

/// `?session=` lists the decisions one session waits on: the session of `user` (the
/// caller by default) in `app`, which it needs (spec 3.3 §10 C1).
#[tokio::test]
async fn the_inbox_lists_one_session() {
    let fx = fixture();
    let carol_s1 = submit_in(&fx, "carol", "s1", 1).await;
    let carol_s2 = submit_in(&fx, "carol", "s2", 1).await;
    let dave_s1 = submit_in(&fx, "dave", "s1", 1).await;
    let ids = |inbox: &Value| -> Vec<String> {
        inbox["items"]
            .as_array()
            .unwrap_or_else(|| panic!("no items in {inbox}"))
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_string())
            .collect()
    };
    let get =
        |uri: &'static str, user: &'static str| call(&fx, "GET", uri, Some(user), Value::Null);

    let (status, every) = get("/api/approvals", "ana").await;
    assert_eq!(status, StatusCode::OK, "{every}");
    assert_eq!(ids(&every).len(), 3, "every session without a filter");
    let (status, one) = get("/api/approvals?app=bank&user=carol&session=s1", "ana").await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(ids(&one), [carol_s1.id], "carol's s1, not dave's s1");
    let (_, two) = get("/api/approvals?app=bank&user=carol&session=s2", "ana").await;
    assert_eq!(ids(&two), [carol_s2.id]);
    let (_, dave) = get("/api/approvals?app=bank&user=dave&session=s1", "ana").await;
    assert_eq!(ids(&dave), [dave_s1.id]);
    let (_, own) = get("/api/approvals?app=bank&session=s1", "ana").await;
    assert_eq!(own["items"], json!([]), "without `user`, the caller's own session");
    let (status, problem) = get("/api/approvals?session=s1", "ana").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert_eq!(problem["error"]["code"], "approval.session_needs_app");
    let (_, elsewhere) = get("/api/approvals?app=shop&user=carol&session=s1", "ana").await;
    assert_eq!(elsewhere["items"], json!([]), "the session ID in another application");
    let (_, outsider) = get("/api/approvals?app=bank&user=carol&session=s1", "eve").await;
    assert_eq!(outsider["items"], json!([]), "the filter never widens the caller's inbox");
}

#[tokio::test]
async fn delegations_and_cancellation_go_through_the_endpoints() {
    let fx = fixture();
    let request = submit(&fx, 1).await;
    let not_after = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let (status, delegation) = call(
        &fx,
        "POST",
        "/api/approvals/delegations",
        Some("ana"),
        json!({ "to": { "kind": "human", "id": "eve" }, "notAfter": not_after }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{delegation}");
    let (_, inbox) = call(&fx, "GET", "/api/approvals", Some("eve"), Value::Null).await;
    assert_eq!(inbox["items"][0]["id"], request.id.as_str(), "eve sees ana's requests");

    let id = delegation["id"].as_str().unwrap();
    let revoke = format!("/api/approvals/delegations/{id}?toKind=human&toId=eve");
    let (status, _) = call(&fx, "DELETE", &revoke, Some("bo"), Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "only the delegator or delegate revokes");
    let (status, _) = call(&fx, "DELETE", &revoke, Some("ana"), Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let cancel = format!("/api/approvals/{}/cancel", request.id);
    let (status, _) = call(&fx, "POST", &cancel, Some("eve"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "eve no longer sees it");
    let (status, cancelled) =
        call(&fx, "POST", &cancel, Some("carol"), json!({ "reason": "duplicate" })).await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["state"], "cancelled");
}

#[tokio::test]
async fn webhook_signatures_follow_standard_webhooks() {
    use adk_core::ManualClock;
    use adk_core::webhook::{WebhookError, WebhookHeaders, WebhookSecret, WebhookVerifier};

    let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    let notifier = WebhookApprovalNotifier::new("https://hooks.invalid/approvals", secret).unwrap();
    let (id, timestamp, body) = ("apr_1:2", 1_760_000_000, "{\"a\":1}");
    let signature = notifier.signature(id, timestamp, body);
    let headers =
        WebhookHeaders { id, timestamp: timestamp.to_string().into(), signature: &signature };
    let clock = Arc::new(ManualClock::new(chrono::DateTime::from_timestamp(timestamp, 0).unwrap()));

    let verifier =
        WebhookVerifier::new(WebhookSecret::from_base64(secret).unwrap()).with_clock(clock.clone());
    let verified = verifier.verify(&headers, body.as_bytes()).unwrap();
    assert_eq!(verified.id, id);
    assert!(matches!(
        verifier.verify(&headers, b"{\"a\":2}"),
        Err(WebhookError::SignatureMismatch)
    ));

    let other = WebhookSecret::from_bytes(vec![7u8; 32]).unwrap();
    let wrong_key = WebhookVerifier::new(other).with_clock(clock);
    assert!(matches!(
        wrong_key.verify(&headers, body.as_bytes()),
        Err(WebhookError::SignatureMismatch)
    ));
    assert!(WebhookApprovalNotifier::new("https://x.invalid", "not base64!").is_err());
}

/// A request says whether its shown arguments are the call's own (spec 3.3 §7.2): an edit
/// that starts from a redacted view would submit the masks as the call's arguments.
#[tokio::test]
async fn a_request_says_whether_it_shows_the_call_s_own_arguments() {
    let fx = fixture();
    let masked = submit(&fx, 1).await;
    let call = ToolConfirmationRequest::new("transfer", json!({ "amount": 20 }));
    let plain = fx
        .service
        .submit(ApprovalRequest::new(
            "apr-plain",
            ApprovalScope::new("bank", "carol", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("finance")]),
        ))
        .await
        .unwrap();
    for (request, shown) in [(&masked, false), (&plain, true)] {
        let uri = format!("/api/approvals/{}", request.id);
        let (status, body) = call_route(&fx, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["showsCallArgs"], json!(shown), "{body}");
        assert!(body.get("fingerprint").is_none(), "the raw arguments stay hidden");
    }
}

async fn call_route(fx: &Fixture, uri: &str) -> (StatusCode, Value) {
    call(fx, "GET", uri, Some("bo"), Value::Null).await
}

/// Refuses every edit, and notes each decision the service recorded with the caller.
#[derive(Default)]
struct NoEdits {
    decided: std::sync::Mutex<Vec<(String, String, adk_core::ApprovalState)>>,
}

#[async_trait]
impl adk_server::approvals::DecideGuard for NoEdits {
    async fn check(
        &self,
        call: &adk_server::approvals::DecideCall<'_>,
    ) -> Result<(), adk_core::AdkError> {
        assert_eq!(
            call.request.state,
            adk_core::ApprovalState::Pending,
            "the check sees it before"
        );
        match call.action {
            adk_core::ApprovalAction::Edit => Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Server,
                adk_core::ErrorCategory::Forbidden,
                "test.no_edits",
                "edit it in the home",
            )),
            _ => Ok(()),
        }
    }

    async fn decided(
        &self,
        call: &adk_server::approvals::DecideCall<'_>,
        decided: &ApprovalRequest,
    ) {
        self.decided.lock().unwrap().push((
            call.caller.user_id.clone(),
            decided.id.clone(),
            decided.state,
        ));
    }
}

/// A host's decide guard (spec 3.3 §7.2) checks each decision after the caller is known and
/// before the service records it: its refusal is the caller's answer and nothing is decided;
/// a decision it admits is recorded, and the guard hears of it with the caller.
#[tokio::test]
async fn a_decide_guard_refuses_before_the_service_and_hears_what_it_recorded() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let service = Arc::new(StoreApprovalService::new(store.clone()));
    let guard = Arc::new(NoEdits::default());
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(Idle)));
    let sessions = Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>;
    let config = ServerConfig::new(loader, sessions).with_request_context(Arc::new(UserExtractor));
    let endpoints = adk_server::approvals::ApprovalEndpoints::new(service.clone())
        .with_decide_guard(guard.clone());
    let app = ServerBuilder::new(config).with_approval_endpoints(endpoints).build();
    let fx = Fixture { app, service, store };
    let request = submit(&fx, 1).await;
    let uri = format!("/api/approvals/{}/decide", request.id);

    let edit = json!({ "expectedVersion": 1, "action": "edit", "editedArgs": { "amount": 1 } });
    let (status, body) = call(&fx, "POST", &uri, Some("ana"), edit).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "test.no_edits");
    let waiting = fx.service.get(&request.id).await.unwrap().unwrap();
    assert_eq!((waiting.state, waiting.version), (adk_core::ApprovalState::Pending, 1));
    assert!(guard.decided.lock().unwrap().is_empty());

    let approve = json!({ "expectedVersion": 1, "action": "approve" });
    let (status, body) = call(&fx, "POST", &uri, Some("ana"), approve).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        guard.decided.lock().unwrap().clone(),
        [("ana".to_string(), request.id.clone(), adk_core::ApprovalState::Approved)]
    );
}
