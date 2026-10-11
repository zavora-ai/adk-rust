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
    let call = ToolConfirmationRequest {
        tool_name: "transfer".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "amount": 900, "iban": "DE00 secret" }),
    };
    let scope = ApprovalScope::new("bank", "carol", "s1");
    fx.store.record_pending(&scope, &call).await.unwrap();
    let request = ApprovalRequest::new(
        uuid::Uuid::new_v4().to_string(),
        scope,
        &call,
        ApprovalPolicy::new([ApproverSelector::group("finance")]).with_quorum(quorum),
    )
    .with_redacted_args(json!({ "amount": 900, "iban": "DE00 ****" }))
    .with_requester(SubjectRef::human("carol"));
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
    use base64::Engine;
    use hmac::{Hmac, Mac};
    let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    let notifier = WebhookApprovalNotifier::new("https://hooks.invalid/approvals", secret).unwrap();
    let key = base64::engine::general_purpose::STANDARD
        .decode(secret.trim_start_matches("whsec_"))
        .unwrap();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&key).unwrap();
    mac.update(b"apr_1:2.1760000000.{\"a\":1}");
    let expected = format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    );
    assert_eq!(notifier.signature("apr_1:2", 1_760_000_000, "{\"a\":1}"), expected);
    assert!(WebhookApprovalNotifier::new("https://x.invalid", "not base64!").is_err());
}
