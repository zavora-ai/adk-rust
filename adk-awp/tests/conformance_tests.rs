//! AWP conformance test suite.
//!
//! These tests spin up an in-process Axum server with all AWP routes and
//! verify protocol compliance against the endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use adk_awp::{
    AwpA2aHandler, AwpState, BusinessContextLoader, InMemoryEventSubscriptionService,
    InMemoryRateLimiter, RateLimitConfig, awp_management_routes, awp_routes,
};
use arc_swap::ArcSwap;
use awp_types::{
    AwpDiscoveryDocument, AwpError, BusinessCapability, BusinessContext, BusinessPolicy,
    CURRENT_VERSION, CapabilityManifest, TrustLevel,
};
use axum::body::Body;
use axum::http::HeaderMap;
use axum::http::{Request, StatusCode};
use tower::util::ServiceExt;

fn sample_context() -> BusinessContext {
    let mut ctx = BusinessContext::core(
        "Conformance Test Site",
        "AWP conformance testing",
        "test.example.com",
    );
    ctx.capabilities = vec![
        BusinessCapability {
            name: "read_data".to_string(),
            description: "Read data".to_string(),
            endpoint: "/api/data".to_string(),
            method: "GET".to_string(),
            access_level: TrustLevel::Anonymous,
        },
        BusinessCapability {
            name: "write_data".to_string(),
            description: "Write data".to_string(),
            endpoint: "/api/data".to_string(),
            method: "POST".to_string(),
            access_level: TrustLevel::Known,
        },
    ];
    ctx.policies = vec![BusinessPolicy {
        name: "privacy".to_string(),
        description: "Privacy policy".to_string(),
        policy_type: "privacy".to_string(),
    }];
    ctx.contact = Some("test@example.com".to_string());
    ctx
}

struct TestA2aHandler;

#[async_trait::async_trait]
impl AwpA2aHandler for TestA2aHandler {
    async fn handle(
        &self,
        _headers: HeaderMap,
        message: serde_json::Value,
    ) -> Result<serde_json::Value, AwpError> {
        Ok(serde_json::json!({
            "status": "processed",
            "messageId": message["id"],
        }))
    }
}

fn build_state(ctx: BusinessContext) -> AwpState {
    AwpState::builder(Arc::new(ArcSwap::from_pointee(ctx)))
        .event_service(Arc::new(InMemoryEventSubscriptionService::new()))
        .a2a_handler(Arc::new(TestA2aHandler))
        .build()
}

fn app() -> axum::Router {
    let state = build_state(sample_context());
    axum::Router::new()
        .merge(awp_routes(state.clone()))
        // Test-only composition: production callers apply authentication here.
        .merge(awp_management_routes(state))
}

// --- 1. Discovery document ---

#[tokio::test]
async fn test_discovery_document_served() {
    let response = app()
        .oneshot(Request::get("/.well-known/awp.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_discovery_document_contains_version_and_urls() {
    let response = app()
        .oneshot(Request::get("/.well-known/awp.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let doc: AwpDiscoveryDocument = serde_json::from_slice(&body).unwrap();

    assert_eq!(doc.version, CURRENT_VERSION);
    assert!(doc.capability_manifest_url.contains("/awp/manifest"));
    assert!(doc.a2a_endpoint_url.contains("/awp/a2a"));
    assert!(doc.events_endpoint_url.contains("/awp/events"));
    assert!(doc.health_endpoint_url.contains("/awp/health"));
    assert_eq!(doc.supported_trust_levels, vec![TrustLevel::Anonymous]);
}

// --- 2. Capability manifest ---

#[tokio::test]
async fn test_manifest_served() {
    let response =
        app().oneshot(Request::get("/awp/manifest").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_manifest_json_ld_fields() {
    let response =
        app().oneshot(Request::get("/awp/manifest").body(Body::empty()).unwrap()).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let manifest: CapabilityManifest = serde_json::from_slice(&body).unwrap();

    assert_eq!(manifest.context, "https://schema.org");
    assert_eq!(manifest.type_, "WebAPI");
    assert_eq!(manifest.capabilities.len(), 2);
}

// --- 3. Version negotiation ---

#[tokio::test]
async fn test_version_negotiation_accepts_compatible() {
    let response = app()
        .oneshot(
            Request::get("/.well-known/awp.json")
                .header("AWP-Version", "1.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("AWP-Version").unwrap(), "1.0");
}

#[tokio::test]
async fn test_version_negotiation_rejects_incompatible() {
    let response = app()
        .oneshot(
            Request::get("/.well-known/awp.json")
                .header("AWP-Version", "2.0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn test_version_negotiation_defaults_when_absent() {
    let response = app()
        .oneshot(Request::get("/.well-known/awp.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("AWP-Version").unwrap(), "1.0");
}

// --- 4. Error responses ---

#[tokio::test]
async fn test_error_response_not_found() {
    let response =
        app().oneshot(Request::get("/nonexistent").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// --- 5. Health endpoint ---

#[tokio::test]
async fn test_health_endpoint_returns_state() {
    let response =
        app().oneshot(Request::get("/awp/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["state"], "healthy");
}

// --- 6. Event subscription CRUD ---

#[tokio::test]
async fn test_event_subscription_create() {
    let body = serde_json::json!({
        "subscriber": "test",
        "callbackUrl": "https://example.com/webhook",
        "eventTypes": ["health.changed"],
        "secret": "test-secret-at-least-32-bytes-long"
    });
    let response = app()
        .oneshot(
            Request::post("/awp/events/subscribe")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let resp_body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert!(json["id"].is_string());
}

#[tokio::test]
async fn test_event_subscription_rejects_insecure_or_weak_configuration() {
    for body in [
        serde_json::json!({
            "subscriber": "test",
            "callbackUrl": "http://example.com/webhook",
            "eventTypes": ["health.changed"],
            "secret": "test-secret-at-least-32-bytes-long"
        }),
        serde_json::json!({
            "subscriber": "test",
            "callbackUrl": "https://example.com/webhook",
            "eventTypes": ["health.changed"],
            "secret": "short"
        }),
    ] {
        let response = app()
            .oneshot(
                Request::post("/awp/events/subscribe")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_event_subscription_list() {
    let response = app()
        .oneshot(Request::get("/awp/events/subscriptions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_array());
}

// --- 7. A2A message handler ---

#[tokio::test]
async fn test_a2a_message_is_dispatched() {
    let body = serde_json::json!({
        "id": "msg-123",
        "sender": "agent-a",
        "recipient": "agent-b",
        "messageType": "request",
        "timestamp": "2026-04-21T00:00:00Z",
        "payload": {}
    });
    let response = app()
        .oneshot(
            Request::post("/awp/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let resp_body = axum::body::to_bytes(response.into_body(), 8192).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert_eq!(json["status"], "processed");
    assert_eq!(json["messageId"], "msg-123");
}

#[tokio::test]
async fn test_unconfigured_a2a_handler_fails_closed() {
    let state = AwpState::builder(Arc::new(ArcSwap::from_pointee(sample_context()))).build();
    let body = serde_json::json!({ "id": "msg-123" });
    let response = awp_routes(state)
        .oneshot(
            Request::post("/awp/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_a2a_requires_a_bounded_message_id() {
    for body in [
        serde_json::json!({ "payload": {} }),
        serde_json::json!({ "id": " ".repeat(4), "payload": {} }),
        serde_json::json!({ "id": "x".repeat(257), "payload": {} }),
    ] {
        let response = awp_routes(build_state(sample_context()))
            .oneshot(
                Request::post("/awp/a2a")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_a2a_body_is_limited_to_64_kib() {
    let body = serde_json::json!({
        "id": "msg-large",
        "payload": "x".repeat(70 * 1024),
    });
    let response = awp_routes(build_state(sample_context()))
        .oneshot(
            Request::post("/awp/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn test_public_routes_apply_the_configured_rate_limit() {
    let limits = HashMap::from([(
        TrustLevel::Anonymous,
        RateLimitConfig { max_requests: 1, window_secs: 60 },
    )]);
    let state = AwpState::builder(Arc::new(ArcSwap::from_pointee(sample_context())))
        .rate_limiter(Arc::new(InMemoryRateLimiter::with_config(limits)))
        .a2a_handler(Arc::new(TestA2aHandler))
        .build();
    let app = awp_routes(state);

    let first = app
        .clone()
        .oneshot(Request::get("/awp/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let second =
        app.oneshot(Request::get("/awp/health").body(Body::empty()).unwrap()).await.unwrap();

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(second.headers().contains_key("Retry-After"));
}

#[tokio::test]
async fn test_safe_default_router_excludes_management_routes() {
    let response = awp_routes(build_state(sample_context()))
        .oneshot(Request::get("/awp/events/subscriptions").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// --- 8. HMAC signing ---

#[test]
fn test_hmac_sign_and_verify() {
    let payload = b"test event payload";
    let secret = "webhook-secret";
    let sig = adk_awp::sign_payload(payload, secret);
    assert!(adk_awp::verify_signature(payload, secret, &sig));
}

#[test]
fn test_hmac_verify_fails_with_wrong_secret() {
    let payload = b"test event payload";
    let sig = adk_awp::sign_payload(payload, "secret1");
    assert!(!adk_awp::verify_signature(payload, "secret2", &sig));
}

// --- 9. Business.toml parsing ---

#[test]
fn test_dogfood_business_toml_parses() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("business.toml");
    let loader = BusinessContextLoader::from_file(&path).unwrap();
    let ctx = loader.load();
    assert_eq!(ctx.site_name, "Agentic Web Protocol");
    assert_eq!(ctx.domain, "agenticwebprotocol.com");
    assert_eq!(ctx.capabilities.len(), 3);
    assert_eq!(ctx.policies.len(), 3);
}

// --- 10. Discovery document from dogfood config ---

#[test]
fn test_dogfood_discovery_document() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("business.toml");
    let loader = BusinessContextLoader::from_file(&path).unwrap();
    let ctx = loader.load();
    let doc = adk_awp::generate_discovery_document(&ctx);
    assert_eq!(doc.version, CURRENT_VERSION);
    assert_eq!(doc.site_name, "Agentic Web Protocol");
    assert!(doc.capability_manifest_url.contains("agenticwebprotocol.com"));
}

// --- 11. Manifest from dogfood config ---

#[test]
fn test_dogfood_manifest() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("business.toml");
    let loader = BusinessContextLoader::from_file(&path).unwrap();
    let ctx = loader.load();
    let manifest = adk_awp::build_manifest(&ctx);
    assert_eq!(manifest.context, "https://schema.org");
    assert_eq!(manifest.type_, "WebAPI");
    assert_eq!(manifest.capabilities.len(), 3);
    assert_eq!(manifest.capabilities[0].name, "read_spec");
}
