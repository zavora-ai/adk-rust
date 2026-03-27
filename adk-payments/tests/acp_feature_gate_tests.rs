#![cfg(all(feature = "acp", not(feature = "acp-experimental")))]

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, ProtocolExtensions,
    TransactionRecord,
};
use adk_payments::kernel::{
    CancelCheckoutCommand, CompleteCheckoutCommand, CreateCheckoutCommand, DelegatePaymentCommand,
    DelegatedPaymentResult, MerchantCheckoutService, OrderUpdateCommand, TransactionLookup,
    UpdateCheckoutCommand,
};
use adk_payments::protocol::acp::{AcpContextTemplate, AcpRouterBuilder};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn sample_actor() -> CommerceActor {
    CommerceActor {
        actor_id: "shopper-agent".to_string(),
        role: CommerceActorRole::AgentSurface,
        display_name: Some("shopper".to_string()),
        tenant_id: Some("tenant-1".to_string()),
        extensions: ProtocolExtensions::default(),
    }
}

fn sample_merchant() -> MerchantRef {
    MerchantRef {
        merchant_id: "merchant-123".to_string(),
        legal_name: "Merchant Example LLC".to_string(),
        display_name: Some("Merchant Example".to_string()),
        statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
        country_code: Some("US".to_string()),
        website: Some("https://merchant.example".to_string()),
        extensions: ProtocolExtensions::default(),
    }
}

struct NoopBackend;

fn unsupported(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unsupported,
        "payments.acp.feature_gate.unsupported",
        message,
    )
}

#[async_trait]
impl MerchantCheckoutService for NoopBackend {
    async fn create_checkout(&self, _command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        Err(unsupported("create_checkout is not used by this feature-gate test"))
    }

    async fn update_checkout(&self, _command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        Err(unsupported("update_checkout is not used by this feature-gate test"))
    }

    async fn get_checkout(&self, _lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        Ok(None)
    }

    async fn complete_checkout(
        &self,
        _command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        Err(unsupported("complete_checkout is not used by this feature-gate test"))
    }

    async fn cancel_checkout(&self, _command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        Err(unsupported("cancel_checkout is not used by this feature-gate test"))
    }

    async fn apply_order_update(&self, _command: OrderUpdateCommand) -> Result<TransactionRecord> {
        Err(unsupported("apply_order_update is not used by this feature-gate test"))
    }
}

#[async_trait]
impl adk_payments::kernel::DelegatedPaymentService for NoopBackend {
    async fn delegate_payment(
        &self,
        _command: DelegatePaymentCommand,
    ) -> Result<DelegatedPaymentResult> {
        Err(unsupported("delegate_payment is not used by this feature-gate test"))
    }
}

async fn status_for(app: &axum::Router, request: Request<Body>) -> StatusCode {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let _ = response.into_body().collect().await.unwrap();
    status
}

#[tokio::test]
async fn stable_acp_router_does_not_mount_experimental_routes() {
    let backend = Arc::new(NoopBackend);
    let app = AcpRouterBuilder::new(AcpContextTemplate {
        session_identity: None,
        actor: sample_actor(),
        merchant_of_record: sample_merchant(),
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
    })
    .with_merchant_checkout_service(backend.clone())
    .with_delegated_payment_service(backend)
    .build()
    .unwrap();

    assert_eq!(
        status_for(
            &app,
            Request::builder()
                .method(Method::GET)
                .uri("/.well-known/acp.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status_for(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/agentic_checkout/webhooks/order_events")
                .body(Body::empty())
                .unwrap(),
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status_for(
            &app,
            Request::builder()
                .method(Method::POST)
                .uri("/delegate_authentication")
                .body(Body::empty())
                .unwrap(),
        )
        .await,
        StatusCode::NOT_FOUND
    );
}

#[test]
fn acp_experimental_types_require_the_feature_flag() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/acp_experimental_requires_feature.rs");
}
