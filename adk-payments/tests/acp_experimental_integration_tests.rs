#![cfg(feature = "acp-experimental")]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, OrderState, ProtocolExtensions,
    ReceiptState, TransactionId, TransactionRecord, TransactionState,
};
use adk_payments::journal::SessionBackedTransactionStore;
use adk_payments::kernel::{
    DelegatePaymentCommand, DelegatedPaymentResult, MerchantCheckoutService, OrderUpdateCommand,
    TransactionLookup,
};
use adk_payments::kernel::{DelegatedPaymentService, TransactionStore};
use adk_payments::protocol::acp::experimental::{
    AcpAuthenticateDelegateAuthenticationSessionCommand,
    AcpCreateDelegateAuthenticationSessionCommand, AcpDelegateAuthenticationAction,
    AcpDelegateAuthenticationActionType, AcpDelegateAuthenticationChallengeAction,
    AcpDelegateAuthenticationFingerprintAction, AcpDelegateAuthenticationFingerprintCompletion,
    AcpDelegateAuthenticationResult, AcpDelegateAuthenticationService,
    AcpDelegateAuthenticationSessionLookup, AcpDelegateAuthenticationSessionState,
    AcpDelegateAuthenticationSessionStatus, AcpDiscoveryCapabilities, AcpDiscoveryDocument,
    AcpDiscoveryProtocol, AcpDiscoveryService, AcpDiscoveryTransport, AcpExperimentalRouterBuilder,
    AcpMerchantWebhookVerificationConfig,
};
use adk_payments::protocol::acp::{
    AcpContextTemplate, AcpRouterBuilder, AcpVerificationConfig, IdempotencyMode,
};
use adk_payments::{ACP_DELEGATE_AUTH_BASELINE, ACP_STABLE_BASELINE};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::RwLock;
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

fn sample_actor() -> CommerceActor {
    CommerceActor {
        actor_id: "shopper-agent".to_string(),
        role: CommerceActorRole::AgentSurface,
        display_name: Some("shopper".to_string()),
        tenant_id: Some("tenant-1".to_string()),
        extensions: ProtocolExtensions::default(),
    }
}

fn sample_merchant_actor() -> CommerceActor {
    CommerceActor {
        actor_id: "merchant-123".to_string(),
        role: CommerceActorRole::Merchant,
        display_name: Some("Merchant Example".to_string()),
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

async fn create_identity(session_service: &InMemorySessionService) -> AdkIdentity {
    let identity = AdkIdentity::new(
        AppName::try_from("payments-app").unwrap(),
        UserId::try_from("user-1").unwrap(),
        SessionId::try_from("session-1").unwrap(),
    );

    session_service
        .create(CreateRequest {
            app_name: identity.app_name.as_ref().to_string(),
            user_id: identity.user_id.as_ref().to_string(),
            session_id: Some(identity.session_id.as_ref().to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    identity
}

struct ExperimentalBackend {
    store: Arc<dyn TransactionStore>,
    auth_sessions: RwLock<BTreeMap<String, AcpDelegateAuthenticationSessionState>>,
    now: RwLock<i64>,
}

impl ExperimentalBackend {
    fn new(store: Arc<dyn TransactionStore>) -> Self {
        Self { store, auth_sessions: RwLock::new(BTreeMap::new()), now: RwLock::new(0) }
    }

    async fn load_transaction(&self, lookup: TransactionLookup) -> Result<TransactionRecord> {
        self.store.get(lookup).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "payments.acp.experimental.test.not_found",
                "ACP experimental test transaction was not found",
            )
        })
    }

    async fn timestamp(&self) -> chrono::DateTime<Utc> {
        let mut now = self.now.write().await;
        *now += 1;
        Utc::now() + Duration::seconds(*now)
    }
}

#[async_trait]
impl MerchantCheckoutService for ExperimentalBackend {
    async fn create_checkout(
        &self,
        command: adk_payments::kernel::CreateCheckoutCommand,
    ) -> Result<TransactionRecord> {
        let now = self.timestamp().await;
        let mut record = TransactionRecord::new(
            command.context.transaction_id.clone(),
            command.context.actor.clone(),
            command.context.merchant_of_record.clone(),
            command.context.mode,
            command.cart.clone(),
            now,
        );
        record.session_identity = command.context.session_identity.clone();
        record.protocol_refs.acp_checkout_session_id =
            Some(command.context.transaction_id.as_str().to_string());
        record
            .transition_to(TransactionState::Negotiating, self.timestamp().await)
            .map_err(AdkError::from)?;
        record
            .transition_to(TransactionState::AwaitingPaymentMethod, self.timestamp().await)
            .map_err(AdkError::from)?;
        record.recompute_safe_summary();
        self.store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn update_checkout(
        &self,
        command: adk_payments::kernel::UpdateCheckoutCommand,
    ) -> Result<TransactionRecord> {
        self.load_transaction(TransactionLookup {
            transaction_id: command.context.transaction_id,
            session_identity: command.context.session_identity,
        })
        .await
    }

    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.store.get(lookup).await
    }

    async fn complete_checkout(
        &self,
        command: adk_payments::kernel::CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id,
                session_identity: command.context.session_identity,
            })
            .await?;
        record
            .transition_to(TransactionState::Completed, self.timestamp().await)
            .map_err(AdkError::from)?;
        record.recompute_safe_summary();
        self.store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn cancel_checkout(
        &self,
        command: adk_payments::kernel::CancelCheckoutCommand,
    ) -> Result<TransactionRecord> {
        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id,
                session_identity: command.context.session_identity,
            })
            .await?;
        record
            .transition_to(TransactionState::Canceled, self.timestamp().await)
            .map_err(AdkError::from)?;
        record.recompute_safe_summary();
        self.store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn apply_order_update(&self, command: OrderUpdateCommand) -> Result<TransactionRecord> {
        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id,
                session_identity: command.context.session_identity,
            })
            .await?;
        record.protocol_refs.acp_order_id = command.order.order_id.clone();
        record.order = Some(command.order);
        record.last_updated_at = self.timestamp().await;
        record.recompute_safe_summary();
        self.store.upsert(record.clone()).await?;
        Ok(record)
    }
}

#[async_trait]
impl DelegatedPaymentService for ExperimentalBackend {
    async fn delegate_payment(
        &self,
        _command: DelegatePaymentCommand,
    ) -> Result<DelegatedPaymentResult> {
        Ok(DelegatedPaymentResult {
            delegated_payment_id: "vt_test_123".to_string(),
            created_at: self.timestamp().await,
            transaction: None,
            generated_evidence_refs: Vec::new(),
            metadata: BTreeMap::new(),
            extensions: ProtocolExtensions::default(),
        })
    }
}

#[async_trait]
impl AcpDelegateAuthenticationService for ExperimentalBackend {
    async fn create_authentication_session(
        &self,
        command: AcpCreateDelegateAuthenticationSessionCommand,
    ) -> Result<AcpDelegateAuthenticationSessionState> {
        let authentication_session_id = format!("auth_session_{}", uuid::Uuid::new_v4().simple());
        let session = AcpDelegateAuthenticationSessionState {
            authentication_session_id: authentication_session_id.clone(),
            status: AcpDelegateAuthenticationSessionStatus::ActionRequired,
            action: Some(AcpDelegateAuthenticationAction {
                r#type: AcpDelegateAuthenticationActionType::Fingerprint,
                fingerprint: Some(AcpDelegateAuthenticationFingerprintAction {
                    three_ds_method_url: "https://acs.issuer.test/3dsmethod".to_string(),
                    three_ds_server_trans_id: "3ds-method-123".to_string(),
                }),
                challenge: None,
            }),
            authentication_result: None,
            checkout_session_id: command.request.checkout_session_id,
            extensions: ProtocolExtensions::default(),
        };
        self.auth_sessions.write().await.insert(authentication_session_id, session.clone());
        Ok(session)
    }

    async fn authenticate_session(
        &self,
        command: AcpAuthenticateDelegateAuthenticationSessionCommand,
    ) -> Result<AcpDelegateAuthenticationSessionState> {
        let mut auth_sessions = self.auth_sessions.write().await;
        let Some(session) = auth_sessions.get_mut(&command.authentication_session_id) else {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "payments.acp.experimental.test.auth_not_found",
                "ACP delegated-authentication session was not found",
            ));
        };

        session.checkout_session_id = command.request.checkout_session_id.clone();
        match command.request.fingerprint_completion {
            AcpDelegateAuthenticationFingerprintCompletion::Completed => {
                session.status = AcpDelegateAuthenticationSessionStatus::ActionRequired;
                session.action = Some(AcpDelegateAuthenticationAction {
                    r#type: AcpDelegateAuthenticationActionType::Challenge,
                    fingerprint: None,
                    challenge: Some(AcpDelegateAuthenticationChallengeAction {
                        acs_url: "https://acs.issuer.test/challenge".to_string(),
                        acs_trans_id: "acs-123".to_string(),
                        three_ds_server_trans_id: "3ds-server-123".to_string(),
                        message_version: "2.2.0".to_string(),
                    }),
                });
            }
            AcpDelegateAuthenticationFingerprintCompletion::NotCompleted => {
                session.status = AcpDelegateAuthenticationSessionStatus::Expired;
                session.action = None;
            }
            AcpDelegateAuthenticationFingerprintCompletion::Unavailable => {
                session.status = AcpDelegateAuthenticationSessionStatus::Unavailable;
                session.action = None;
            }
        }

        Ok(session.clone())
    }

    async fn get_authentication_session(
        &self,
        lookup: AcpDelegateAuthenticationSessionLookup,
    ) -> Result<Option<AcpDelegateAuthenticationSessionState>> {
        let mut auth_sessions = self.auth_sessions.write().await;
        let Some(session) = auth_sessions.get_mut(&lookup.authentication_session_id) else {
            return Ok(None);
        };

        if session.status == AcpDelegateAuthenticationSessionStatus::ActionRequired
            && session.action.as_ref().is_some_and(|action| {
                action.r#type == AcpDelegateAuthenticationActionType::Challenge
            })
        {
            session.status = AcpDelegateAuthenticationSessionStatus::Authenticated;
            session.action = None;
            session.authentication_result = Some(AcpDelegateAuthenticationResult {
                trans_status: "Y".to_string(),
                electronic_commerce_indicator: Some("05".to_string()),
                three_ds_cryptogram: Some("AQIDBAUGBwgJCgsMDQ4PEBESExQ=".to_string()),
                transaction_id: "directory-server-123".to_string(),
                three_ds_server_trans_id: "3ds-server-123".to_string(),
                version: "2.2.0".to_string(),
                authentication_value: Some("CAVV_VALUE".to_string()),
                trans_status_reason: None,
                cardholder_info: None,
            });
        }

        Ok(Some(session.clone()))
    }
}

fn request(
    method: Method,
    uri: &str,
    api_version: &str,
    body: Value,
    idempotency_key: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("API-Version", api_version)
        .header("Authorization", "Bearer test-token");
    if !body.is_null() {
        builder = builder.header("Content-Type", "application/json");
    }
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("Idempotency-Key", idempotency_key);
    }
    builder
        .body(Body::from(if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        }))
        .unwrap()
}

async fn response_parts(
    app: &mut axum::Router,
    request: Request<Body>,
) -> (StatusCode, HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, headers, body)
}

fn sign_webhook(secret: &[u8], body: &Value, timestamp: i64) -> (Vec<u8>, String) {
    let payload = serde_json::to_vec(body).unwrap();
    let mut mac = HmacSha256::new_from_slice(secret).unwrap();
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(&payload);
    let signature = hex::encode(mac.finalize().into_bytes());
    (payload, format!("t={timestamp},v1={signature}"))
}

#[tokio::test]
async fn acp_experimental_routes_support_discovery_delegate_auth_and_signed_webhooks() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store = Arc::new(SessionBackedTransactionStore::new(session_service.clone()));
    let backend = Arc::new(ExperimentalBackend::new(store.clone()));
    let webhook_secret = std::env::var("TEST_WEBHOOK_SECRET")
        .unwrap_or_else(|_| "test-only-webhook-secret-not-for-production".to_string())
        .into_bytes();

    let stable = AcpRouterBuilder::new(AcpContextTemplate {
        session_identity: Some(identity.clone()),
        actor: sample_actor(),
        merchant_of_record: sample_merchant(),
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
    })
    .with_merchant_checkout_service(backend.clone())
    .with_delegated_payment_service(backend.clone())
    .with_verification(
        AcpVerificationConfig::strict().with_idempotency_mode(IdempotencyMode::RequireForPost),
    )
    .build()
    .unwrap();

    let experimental = AcpExperimentalRouterBuilder::new(AcpContextTemplate {
        session_identity: Some(identity.clone()),
        actor: sample_actor(),
        merchant_of_record: sample_merchant(),
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
    })
    .with_discovery_document(AcpDiscoveryDocument {
        protocol: AcpDiscoveryProtocol {
            name: "acp".to_string(),
            version: ACP_STABLE_BASELINE.to_string(),
            supported_versions: vec![
                ACP_DELEGATE_AUTH_BASELINE.to_string(),
                ACP_STABLE_BASELINE.to_string(),
            ],
            documentation_url: Some("https://merchant.example/docs/acp".to_string()),
        },
        api_base_url: "https://merchant.example/api".to_string(),
        transports: vec![AcpDiscoveryTransport::Rest],
        capabilities: AcpDiscoveryCapabilities {
            services: vec![
                AcpDiscoveryService::Checkout,
                AcpDiscoveryService::Orders,
                AcpDiscoveryService::DelegatePayment,
            ],
            extensions: Vec::new(),
            intervention_types: Vec::new(),
            supported_currencies: vec!["usd".to_string()],
            supported_locales: vec!["en-US".to_string()],
        },
    })
    .with_delegate_authentication_service(backend.clone())
    .with_merchant_checkout_service(backend.clone())
    .with_webhook_context(AcpContextTemplate {
        session_identity: Some(identity.clone()),
        actor: sample_merchant_actor(),
        merchant_of_record: sample_merchant(),
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
    })
    .with_merchant_webhook_verification(AcpMerchantWebhookVerificationConfig::new(
        webhook_secret.clone(),
    ))
    .with_verification(
        AcpVerificationConfig::strict()
            .with_supported_api_versions(vec![ACP_DELEGATE_AUTH_BASELINE.to_string()])
            .with_idempotency_mode(IdempotencyMode::RequireForPost),
    )
    .build()
    .unwrap();

    let mut app = stable.merge(experimental);

    let (status, headers, discovery) = response_parts(
        &mut app,
        Request::builder()
            .method(Method::GET)
            .uri("/.well-known/acp.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "public, max-age=3600");
    assert_eq!(discovery["protocol"]["name"], "acp");

    let (status, _, create_checkout_response) = response_parts(
        &mut app,
        request(
            Method::POST,
            "/checkout_sessions",
            ACP_STABLE_BASELINE,
            serde_json::json!({
                "currency": "usd",
                "line_items": [{"id": "item_123"}],
                "capabilities": {
                    "interventions": {
                        "supported": ["3ds"]
                    }
                }
            }),
            Some("idem-checkout-create"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let checkout_session_id = create_checkout_response["id"].as_str().unwrap().to_string();

    let (status, _, create_auth_response) = response_parts(
        &mut app,
        request(
            Method::POST,
            "/delegate_authentication",
            ACP_DELEGATE_AUTH_BASELINE,
            serde_json::json!({
                "merchant_id": "merchant-123",
                "payment_method": {
                    "type": "card",
                    "number": "4917610000000000",
                    "exp_month": "03",
                    "exp_year": "2030",
                    "name": "Jane Doe"
                },
                "amount": {
                    "value": 1000,
                    "currency": "EUR"
                },
                "checkout_session_id": checkout_session_id
            }),
            Some("idem-auth-create"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(create_auth_response["status"], "action_required");
    assert_eq!(create_auth_response["action"]["type"], "fingerprint");
    let authentication_session_id =
        create_auth_response["authentication_session_id"].as_str().unwrap().to_string();

    let (status, _, authenticate_response) = response_parts(
        &mut app,
        request(
            Method::POST,
            &format!("/delegate_authentication/{authentication_session_id}/authenticate"),
            ACP_DELEGATE_AUTH_BASELINE,
            serde_json::json!({
                "fingerprint_completion": "Y",
                "checkout_session_id": checkout_session_id
            }),
            Some("idem-auth-authenticate"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(authenticate_response["status"], "action_required");
    assert_eq!(authenticate_response["action"]["type"], "challenge");

    let (status, _, retrieve_response) = response_parts(
        &mut app,
        request(
            Method::GET,
            &format!("/delegate_authentication/{authentication_session_id}"),
            ACP_DELEGATE_AUTH_BASELINE,
            Value::Null,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retrieve_response["status"], "authenticated");
    assert_eq!(retrieve_response["authentication_result"]["trans_status"], "Y");

    let webhook_body = serde_json::json!({
        "type": "order_update",
        "data": {
            "type": "order",
            "id": "ord_123",
            "checkout_session_id": checkout_session_id,
            "permalink_url": "https://merchant.example/orders/123",
            "status": "shipped",
            "line_items": [
                {
                    "id": "li_shoes",
                    "title": "Running Shoes",
                    "quantity": { "ordered": 1, "shipped": 1 },
                    "unit_price": 9900,
                    "subtotal": 9900,
                    "status": "shipped"
                }
            ],
            "fulfillments": [
                {
                    "id": "ful_1",
                    "type": "shipping",
                    "status": "shipped",
                    "line_items": [{ "id": "li_shoes", "quantity": 1 }],
                    "carrier": "USPS",
                    "tracking_number": "9400111899223456789012",
                    "tracking_url": "https://tools.usps.com/go/TrackConfirmAction?tLabels=9400111899223456789012",
                    "events": [
                        {
                            "id": "evt_1",
                            "type": "shipped",
                            "occurred_at": "2026-02-05T10:00:00Z"
                        }
                    ]
                }
            ],
            "adjustments": [],
            "totals": [
                { "type": "subtotal", "display_text": "Subtotal", "amount": 9900 },
                { "type": "fulfillment", "display_text": "Shipping", "amount": 500 },
                { "type": "tax", "display_text": "Tax", "amount": 860 },
                { "type": "total", "display_text": "Total", "amount": 11260 }
            ]
        }
    });
    let (payload, signature) = sign_webhook(&webhook_secret, &webhook_body, Utc::now().timestamp());
    let (status, _, webhook_response) = response_parts(
        &mut app,
        Request::builder()
            .method(Method::POST)
            .uri("/agentic_checkout/webhooks/order_events")
            .header("Content-Type", "application/json")
            .header("Merchant-Signature", signature)
            .header("Request-Id", "req-webhook-123")
            .body(Body::from(payload))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(webhook_response["received"], true);
    assert_eq!(webhook_response["request_id"], "req-webhook-123");

    let record = store
        .get(TransactionLookup {
            transaction_id: TransactionId::from(checkout_session_id),
            session_identity: Some(identity),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.protocol_refs.acp_order_id.as_deref(), Some("ord_123"));
    assert_eq!(record.order.as_ref().unwrap().state, OrderState::FulfillmentPending);
    assert_eq!(record.order.as_ref().unwrap().receipt_state, ReceiptState::Settled);
    let envelope = record.order.as_ref().unwrap().extensions.as_slice().last().unwrap();
    assert_eq!(envelope.fields["webhook_event_type"], "order_update");
    assert_eq!(envelope.fields["order"]["fulfillments"][0]["carrier"], "USPS");
}

#[tokio::test]
async fn signed_webhook_rejects_invalid_signature() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store = Arc::new(SessionBackedTransactionStore::new(session_service));
    let backend = Arc::new(ExperimentalBackend::new(store));

    let mut app = AcpExperimentalRouterBuilder::new(AcpContextTemplate {
        session_identity: Some(identity),
        actor: sample_actor(),
        merchant_of_record: sample_merchant(),
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
    })
    .with_merchant_checkout_service(backend)
    .with_merchant_webhook_verification(AcpMerchantWebhookVerificationConfig::new(
        b"expected-secret".to_vec(),
    ))
    .build()
    .unwrap();

    let (status, _, response) = response_parts(
        &mut app,
        Request::builder()
            .method(Method::POST)
            .uri("/agentic_checkout/webhooks/order_events")
            .header("Content-Type", "application/json")
            .header("Merchant-Signature", "t=1700000000,v1=deadbeef")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "type": "order_update",
                    "data": {
                        "type": "order",
                        "id": "ord_123",
                        "checkout_session_id": "checkout_session_123",
                        "permalink_url": "https://merchant.example/orders/123",
                        "status": "created"
                    }
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(response["code"], "invalid_signature");
}
