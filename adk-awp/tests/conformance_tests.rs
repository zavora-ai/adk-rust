//! AWP conformance test suite.
//!
//! These tests spin up an in-process Axum server with all AWP routes and
//! verify protocol compliance against the endpoints.

use std::sync::Arc;

use adk_awp::{
    AwpState, BusinessContextLoader, DefaultTrustAssigner, HealthStateMachine,
    InMemoryConsentService, InMemoryEventSubscriptionService, InMemoryRateLimiter, awp_routes,
};
use arc_swap::ArcSwap;
use awp_types::{
    AwpDiscoveryDocument, BusinessCapability, BusinessContext, BusinessPolicy, CURRENT_VERSION,
    CapabilityManifest, TrustLevel,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::util::ServiceExt;

fn sample_context() -> BusinessContext {
    BusinessContext {
        site_name: "Conformance Test Site".to_string(),
        site_description: "AWP conformance testing".to_string(),
        domain: "test.example.com".to_string(),
        capabilities: vec![
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
        ],
        policies: vec![BusinessPolicy {
            name: "privacy".to_string(),
            description: "Privacy policy".to_string(),
            policy_type: "privacy".to_string(),
        }],
        contact: Some("test@example.com".to_string()),
    }
}

fn build_state(ctx: BusinessContext) -> AwpState {
    let event_service = Arc::new(InMemoryEventSubscriptionService::new());
    AwpState {
        business_context: Arc::new(ArcSwap::from_pointee(ctx)),
        rate_limiter: Arc::new(InMemoryRateLimiter::new()),
        consent_service: Arc::new(InMemoryConsentService::new()),
        event_service: event_service.clone(),
        health: Arc::new(HealthStateMachine::new(event_service)),
        trust_assigner: Arc::new(DefaultTrustAssigner),
    }
}

fn app() -> axum::Router {
    awp_routes(build_state(sample_context()))
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
        "secret": "test-secret"
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
async fn test_a2a_message_acknowledged() {
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
    assert_eq!(json["status"], "acknowledged");
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
