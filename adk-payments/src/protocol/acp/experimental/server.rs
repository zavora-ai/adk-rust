use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ACP_DELEGATE_AUTH_BASELINE;
use crate::ACP_EXPERIMENTAL_CHANNEL;
use crate::domain::{
    CommerceActor, CommerceActorRole, OrderSnapshot, OrderState, ProtocolDescriptor,
    ProtocolExtensionEnvelope, ProtocolExtensions, ReceiptState, TransactionId,
};
use crate::kernel::{CommerceContext, MerchantCheckoutService, OrderUpdateCommand};
use crate::protocol::acp::experimental::delegate_authentication::{
    AcpAuthenticateDelegateAuthenticationSessionCommand,
    AcpCreateDelegateAuthenticationSessionCommand, AcpDelegateAuthenticationAuthenticateRequest,
    AcpDelegateAuthenticationCreateRequest, AcpDelegateAuthenticationService,
    AcpDelegateAuthenticationSessionLookup,
};
use crate::protocol::acp::experimental::discovery::AcpDiscoveryDocument;
use crate::protocol::acp::experimental::webhook::{
    AcpMerchantWebhookVerificationConfig, AcpMerchantWebhookVerifier, AcpWebhookEvent,
    MerchantWebhookVerificationError, VerifiedMerchantWebhookHeaders,
};
use crate::protocol::acp::mapper::request_metadata_extensions;
use crate::protocol::acp::server::AcpContextTemplate;
use crate::protocol::acp::types::AcpErrorResponse;
use crate::protocol::acp::verification::{
    AcpRequestVerifier, AcpVerificationConfig, AcpVerificationError, StoredIdempotentResponse,
};

/// Builder for ACP experimental discovery, webhook, and delegated-auth routes.
pub struct AcpExperimentalRouterBuilder {
    context_template: AcpContextTemplate,
    webhook_context_template: Option<AcpContextTemplate>,
    discovery_document: Option<AcpDiscoveryDocument>,
    merchant_checkout_service: Option<Arc<dyn MerchantCheckoutService>>,
    delegate_authentication_service: Option<Arc<dyn AcpDelegateAuthenticationService>>,
    verification: AcpVerificationConfig,
    webhook_verification: Option<AcpMerchantWebhookVerificationConfig>,
}

#[derive(Clone)]
struct AcpExperimentalRouterState {
    context_template: AcpContextTemplate,
    webhook_context_template: AcpContextTemplate,
    discovery_document: Option<AcpDiscoveryDocument>,
    merchant_checkout_service: Option<Arc<dyn MerchantCheckoutService>>,
    delegate_authentication_service: Option<Arc<dyn AcpDelegateAuthenticationService>>,
    verifier: AcpRequestVerifier,
    webhook_verifier: Option<AcpMerchantWebhookVerifier>,
}

#[derive(Serialize)]
struct AcpWebhookAcknowledgement {
    received: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

struct AcpApiError {
    status: StatusCode,
    payload: AcpErrorResponse,
    headers: BTreeMap<String, String>,
}

impl AcpExperimentalRouterBuilder {
    /// Creates a new builder for ACP experimental routes.
    #[must_use]
    pub fn new(context_template: AcpContextTemplate) -> Self {
        Self {
            webhook_context_template: None,
            discovery_document: None,
            merchant_checkout_service: None,
            delegate_authentication_service: None,
            verification: AcpVerificationConfig::permissive()
                .with_supported_api_versions(vec![ACP_DELEGATE_AUTH_BASELINE.to_string()]),
            webhook_verification: None,
            context_template,
        }
    }

    /// Attaches a static ACP discovery document.
    #[must_use]
    pub fn with_discovery_document(mut self, discovery_document: AcpDiscoveryDocument) -> Self {
        self.discovery_document = Some(discovery_document);
        self
    }

    /// Attaches the canonical checkout backend used by signed order webhooks.
    #[must_use]
    pub fn with_merchant_checkout_service(
        mut self,
        merchant_checkout_service: Arc<dyn MerchantCheckoutService>,
    ) -> Self {
        self.merchant_checkout_service = Some(merchant_checkout_service);
        self
    }

    /// Attaches the delegated-authentication backend.
    #[must_use]
    pub fn with_delegate_authentication_service(
        mut self,
        delegate_authentication_service: Arc<dyn AcpDelegateAuthenticationService>,
    ) -> Self {
        self.delegate_authentication_service = Some(delegate_authentication_service);
        self
    }

    /// Replaces the delegated-authentication request verification profile.
    #[must_use]
    pub fn with_verification(mut self, verification: AcpVerificationConfig) -> Self {
        self.verification = verification;
        self
    }

    /// Replaces the commerce context used for merchant webhooks.
    #[must_use]
    pub fn with_webhook_context(mut self, webhook_context_template: AcpContextTemplate) -> Self {
        self.webhook_context_template = Some(webhook_context_template);
        self
    }

    /// Enables signed merchant webhook verification.
    #[must_use]
    pub fn with_merchant_webhook_verification(
        mut self,
        webhook_verification: AcpMerchantWebhookVerificationConfig,
    ) -> Self {
        self.webhook_verification = Some(webhook_verification);
        self
    }

    /// Builds the configured ACP experimental router.
    ///
    /// # Errors
    ///
    /// Returns an error when no experimental routes are configured or when a
    /// configured route is missing a required backend service.
    pub fn build(self) -> Result<Router> {
        let webhook_context_template = self
            .webhook_context_template
            .unwrap_or_else(|| default_webhook_context(&self.context_template));
        let state = AcpExperimentalRouterState {
            context_template: self.context_template,
            webhook_context_template,
            discovery_document: self.discovery_document,
            merchant_checkout_service: self.merchant_checkout_service,
            delegate_authentication_service: self.delegate_authentication_service,
            verifier: AcpRequestVerifier::new(self.verification),
            webhook_verifier: self.webhook_verification.map(AcpMerchantWebhookVerifier::new),
        };

        let mut routes = 0_u8;
        let mut router = Router::new();

        if state.discovery_document.is_some() {
            router = router.route("/.well-known/acp.json", get(get_discovery_document));
            routes += 1;
        }

        if state.webhook_verifier.is_some() {
            if state.merchant_checkout_service.is_none() {
                return Err(missing_service_error("merchant_order_webhooks"));
            }
            router =
                router.route("/agentic_checkout/webhooks/order_events", post(post_order_event));
            routes += 1;
        }

        if state.delegate_authentication_service.is_some() {
            router = router
                .route("/delegate_authentication", post(create_delegate_authentication_session))
                .route(
                    "/delegate_authentication/{authentication_session_id}/authenticate",
                    post(authenticate_delegate_authentication_session),
                )
                .route(
                    "/delegate_authentication/{authentication_session_id}",
                    get(get_delegate_authentication_session),
                );
            routes += 1;
        }

        if routes == 0 {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.experimental.no_routes",
                "ACP experimental router requires discovery, webhooks, or delegated-authentication before build()",
            ));
        }

        Ok(router.with_state(state))
    }
}

impl IntoResponse for AcpApiError {
    fn into_response(self) -> Response {
        build_json_response(self.status, &self.payload, &self.headers, false)
    }
}

async fn get_discovery_document(State(state): State<AcpExperimentalRouterState>) -> Response {
    let Some(discovery_document) = &state.discovery_document else {
        return missing_route_response();
    };

    build_json_response(
        StatusCode::OK,
        discovery_document,
        &BTreeMap::from([("Cache-Control".to_string(), "public, max-age=3600".to_string())]),
        false,
    )
}

async fn post_order_event(
    State(state): State<AcpExperimentalRouterState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(verifier) = &state.webhook_verifier else {
        return missing_route_response();
    };
    let verified = match verifier.verify(&headers, &body) {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_webhook_verification(error).into_response(),
    };

    let event: AcpWebhookEvent =
        match parse_json_body("/agentic_checkout/webhooks/order_events", &body) {
            Ok(event) => event,
            Err(error) => return error.into_response(),
        };
    let Some(merchant_checkout_service) = &state.merchant_checkout_service else {
        return missing_route_response();
    };

    let result = async {
        let checkout_session_id = required_order_field(&event.data, "checkout_session_id")?;
        let order_id = required_order_field(&event.data, "id")?;
        let receipt_id = optional_order_field(&event.data, "receipt_id");
        let status = optional_order_field(&event.data, "status");
        let order = OrderSnapshot {
            order_id: Some(order_id.to_string()),
            receipt_id: receipt_id.map(ToOwned::to_owned),
            state: map_webhook_order_state(status),
            receipt_state: map_webhook_receipt_state(status, &event.data),
            extensions: ProtocolExtensions::from(vec![
                ProtocolExtensionEnvelope::new(ProtocolDescriptor::acp(ACP_EXPERIMENTAL_CHANNEL))
                    .with_field("webhook_event_type", json!(event.r#type.as_str()))
                    .with_field("order", event.data.clone()),
            ]),
        };
        let context = webhook_commerce_context(&state, checkout_session_id, &verified);
        merchant_checkout_service.apply_order_update(OrderUpdateCommand { context, order }).await
    }
    .await;

    match result {
        Ok(_) => build_json_response(
            StatusCode::OK,
            &AcpWebhookAcknowledgement { received: true, request_id: verified.request_id.clone() },
            &webhook_response_headers(&verified),
            false,
        ),
        Err(error) => {
            AcpApiError::from_adk(error, webhook_response_headers(&verified)).into_response()
        }
    }
}

async fn create_delegate_authentication_session(
    State(state): State<AcpExperimentalRouterState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = "/delegate_authentication";
    let verified = match state.verifier.verify("POST", path, &headers, &body).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpDelegateAuthenticationCreateRequest = match parse_json_body(path, &body) {
        Ok(request) => request,
        Err(error) => {
            return finalize_error(
                &state,
                "POST",
                path,
                verified.headers.idempotency_key.as_deref(),
                &body,
                error,
            )
            .await;
        }
    };
    let Some(delegate_authentication_service) = &state.delegate_authentication_service else {
        return missing_route_response();
    };
    let context = delegate_authentication_context(
        &state,
        request.checkout_session_id.clone().unwrap_or_else(|| {
            format!("delegate_authentication_{}", uuid::Uuid::new_v4().simple())
        }),
        "create_delegate_authentication_session",
        &verified.headers,
    );
    let result = delegate_authentication_service
        .create_authentication_session(AcpCreateDelegateAuthenticationSessionCommand {
            context,
            request,
        })
        .await;

    finalize_post_result(
        &state,
        path,
        verified,
        &body,
        StatusCode::CREATED,
        result.map(|session| session.to_session()),
    )
    .await
}

async fn authenticate_delegate_authentication_session(
    State(state): State<AcpExperimentalRouterState>,
    Path(authentication_session_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("/delegate_authentication/{authentication_session_id}/authenticate");
    let verified = match state.verifier.verify("POST", &path, &headers, &body).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpDelegateAuthenticationAuthenticateRequest = match parse_json_body(&path, &body)
    {
        Ok(request) => request,
        Err(error) => {
            return finalize_error(
                &state,
                "POST",
                &path,
                verified.headers.idempotency_key.as_deref(),
                &body,
                error,
            )
            .await;
        }
    };
    let Some(delegate_authentication_service) = &state.delegate_authentication_service else {
        return missing_route_response();
    };
    let context = delegate_authentication_context(
        &state,
        request.checkout_session_id.clone().unwrap_or_else(|| authentication_session_id.clone()),
        "authenticate_delegate_authentication_session",
        &verified.headers,
    );
    let result = delegate_authentication_service
        .authenticate_session(AcpAuthenticateDelegateAuthenticationSessionCommand {
            context,
            authentication_session_id,
            request,
        })
        .await;

    finalize_post_result(
        &state,
        &path,
        verified,
        &body,
        StatusCode::OK,
        result.map(|session| session.to_session()),
    )
    .await
}

async fn get_delegate_authentication_session(
    State(state): State<AcpExperimentalRouterState>,
    Path(authentication_session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let path = format!("/delegate_authentication/{authentication_session_id}");
    let verified = match state.verifier.verify("GET", &path, &headers, &[]).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let Some(delegate_authentication_service) = &state.delegate_authentication_service else {
        return missing_route_response();
    };
    let result = delegate_authentication_service
        .get_authentication_session(AcpDelegateAuthenticationSessionLookup {
            authentication_session_id,
            session_identity: state.context_template.session_identity.clone(),
        })
        .await
        .and_then(|session| {
            session.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::NotFound,
                    "payments.acp.delegate_authentication.not_found",
                    "ACP delegated-authentication session was not found",
                )
            })
        });

    match result {
        Ok(session) => success_response(
            StatusCode::OK,
            &session.to_session_with_result(),
            response_headers(&verified.headers),
        ),
        Err(error) => {
            AcpApiError::from_adk(error, response_headers(&verified.headers)).into_response()
        }
    }
}

fn default_webhook_context(context_template: &AcpContextTemplate) -> AcpContextTemplate {
    AcpContextTemplate {
        session_identity: context_template.session_identity.clone(),
        actor: CommerceActor {
            actor_id: context_template.merchant_of_record.merchant_id.clone(),
            role: CommerceActorRole::Merchant,
            display_name: context_template
                .merchant_of_record
                .display_name
                .clone()
                .or_else(|| Some(context_template.merchant_of_record.legal_name.clone())),
            tenant_id: context_template.actor.tenant_id.clone(),
            extensions: ProtocolExtensions::default(),
        },
        merchant_of_record: context_template.merchant_of_record.clone(),
        payment_processor: context_template.payment_processor.clone(),
        mode: context_template.mode,
    }
}

fn webhook_commerce_context(
    state: &AcpExperimentalRouterState,
    checkout_session_id: &str,
    verified: &VerifiedMerchantWebhookHeaders,
) -> CommerceContext {
    CommerceContext {
        transaction_id: TransactionId::from(checkout_session_id),
        session_identity: state.webhook_context_template.session_identity.clone(),
        actor: state.webhook_context_template.actor.clone(),
        merchant_of_record: state.webhook_context_template.merchant_of_record.clone(),
        payment_processor: state.webhook_context_template.payment_processor.clone(),
        mode: state.webhook_context_template.mode,
        protocol: ProtocolDescriptor::acp(ACP_EXPERIMENTAL_CHANNEL),
        extensions: request_metadata_extensions(
            "order_webhook",
            ACP_EXPERIMENTAL_CHANNEL,
            verified.request_id.as_deref(),
            None,
            Some(verified.signed_at),
            true,
        ),
    }
}

fn delegate_authentication_context(
    state: &AcpExperimentalRouterState,
    transaction_id: String,
    operation: &str,
    verified: &crate::protocol::acp::verification::VerifiedRequestHeaders,
) -> CommerceContext {
    CommerceContext {
        transaction_id: TransactionId::from(transaction_id),
        session_identity: state.context_template.session_identity.clone(),
        actor: state.context_template.actor.clone(),
        merchant_of_record: state.context_template.merchant_of_record.clone(),
        payment_processor: state.context_template.payment_processor.clone(),
        mode: state.context_template.mode,
        protocol: ProtocolDescriptor::acp(verified.api_version.clone()),
        extensions: request_metadata_extensions(
            operation,
            &verified.api_version,
            verified.request_id.as_deref(),
            verified.idempotency_key.as_deref(),
            verified.timestamp,
            verified.signature_present,
        ),
    }
}

fn required_order_field<'a>(order: &'a Value, key: &str) -> Result<&'a str> {
    order.get(key).and_then(Value::as_str).ok_or_else(|| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            "payments.acp.experimental.webhook.invalid_order",
            format!("ACP webhook order is missing the required `{key}` field"),
        )
    })
}

fn optional_order_field<'a>(order: &'a Value, key: &str) -> Option<&'a str> {
    order.get(key).and_then(Value::as_str)
}

fn map_webhook_order_state(status: Option<&str>) -> OrderState {
    match status.map(str::to_ascii_lowercase).as_deref() {
        Some("created" | "authorized" | "paid" | "confirmed") => OrderState::Authorized,
        Some("processing" | "ready_for_fulfillment" | "shipped" | "in_transit") => {
            OrderState::FulfillmentPending
        }
        Some("fulfilled" | "delivered") => OrderState::Fulfilled,
        Some("completed") => OrderState::Completed,
        Some("canceled" | "cancelled") => OrderState::Canceled,
        Some("refunded") => OrderState::Refunded,
        Some("partially_refunded") => OrderState::PartiallyRefunded,
        Some("failed" | "payment_failed") => OrderState::Failed,
        _ => OrderState::PendingPayment,
    }
}

fn map_webhook_receipt_state(status: Option<&str>, order: &Value) -> ReceiptState {
    let has_refund =
        order.get("adjustments").and_then(Value::as_array).is_some_and(|adjustments| {
            adjustments.iter().any(|adjustment| {
                adjustment.get("type").and_then(Value::as_str) == Some("refund")
                    && adjustment.get("status").and_then(Value::as_str) == Some("completed")
            })
        });

    match status.map(str::to_ascii_lowercase).as_deref() {
        Some("refunded") => ReceiptState::Refunded,
        _ if has_refund => ReceiptState::PartiallyRefunded,
        Some("created" | "authorized" | "paid" | "confirmed") => ReceiptState::Authorized,
        Some(
            "processing"
            | "ready_for_fulfillment"
            | "shipped"
            | "in_transit"
            | "fulfilled"
            | "delivered"
            | "completed",
        ) => ReceiptState::Settled,
        Some("canceled" | "cancelled") => ReceiptState::Voided,
        Some("failed" | "payment_failed") => ReceiptState::Failed,
        _ => ReceiptState::Pending,
    }
}

async fn finalize_post_result<T: Serialize>(
    state: &AcpExperimentalRouterState,
    path: &str,
    verified: crate::protocol::acp::verification::VerifiedRequest,
    request_body: &[u8],
    success_status: StatusCode,
    result: Result<T>,
) -> Response {
    match result {
        Ok(payload) => {
            let headers = response_headers(&verified.headers);
            let response = serialize_response(&payload);
            match response {
                Ok(response) => {
                    let _ = state
                        .verifier
                        .finalize(
                            "POST",
                            path,
                            verified.headers.idempotency_key.as_deref(),
                            request_body,
                            &StoredIdempotentResponse {
                                status: success_status.as_u16(),
                                body: response.body.clone(),
                                headers: headers.clone(),
                            },
                        )
                        .await;
                    build_json_response(success_status, &payload, &headers, false)
                }
                Err(error) => AcpApiError::from_adk(error, headers).into_response(),
            }
        }
        Err(error) => {
            let api_error = AcpApiError::from_adk(error, response_headers(&verified.headers));
            let _ = state
                .verifier
                .finalize(
                    "POST",
                    path,
                    verified.headers.idempotency_key.as_deref(),
                    request_body,
                    &StoredIdempotentResponse {
                        status: api_error.status.as_u16(),
                        body: serde_json::to_vec(&api_error.payload).unwrap_or_default(),
                        headers: api_error.headers.clone(),
                    },
                )
                .await;
            api_error.into_response()
        }
    }
}

async fn finalize_error(
    state: &AcpExperimentalRouterState,
    method: &str,
    path: &str,
    idempotency_key: Option<&str>,
    request_body: &[u8],
    api_error: AcpApiError,
) -> Response {
    let _ = state
        .verifier
        .finalize(
            method,
            path,
            idempotency_key,
            request_body,
            &StoredIdempotentResponse {
                status: api_error.status.as_u16(),
                body: serde_json::to_vec(&api_error.payload).unwrap_or_default(),
                headers: api_error.headers.clone(),
            },
        )
        .await;
    api_error.into_response()
}

fn parse_json_body<T: serde::de::DeserializeOwned>(
    path: &str,
    body: &[u8],
) -> std::result::Result<T, AcpApiError> {
    serde_json::from_slice(body).map_err(|error| AcpApiError {
        status: StatusCode::BAD_REQUEST,
        payload: AcpErrorResponse {
            r#type: "invalid_request".to_string(),
            code: "invalid_json".to_string(),
            message: format!("invalid ACP JSON body for `{path}`: {error}"),
            param: None,
        },
        headers: BTreeMap::new(),
    })
}

fn response_headers(
    headers: &crate::protocol::acp::verification::VerifiedRequestHeaders,
) -> BTreeMap<String, String> {
    let mut response_headers = BTreeMap::new();
    if let Some(request_id) = &headers.request_id {
        response_headers.insert("Request-Id".to_string(), request_id.clone());
    }
    if let Some(idempotency_key) = &headers.idempotency_key {
        response_headers.insert("Idempotency-Key".to_string(), idempotency_key.clone());
    }
    response_headers
}

fn webhook_response_headers(headers: &VerifiedMerchantWebhookHeaders) -> BTreeMap<String, String> {
    let mut response_headers = BTreeMap::new();
    if let Some(request_id) = &headers.request_id {
        response_headers.insert("Request-Id".to_string(), request_id.clone());
    }
    response_headers
}

fn replay_response(stored: StoredIdempotentResponse) -> Response {
    build_bytes_response(
        StatusCode::from_u16(stored.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        stored.body,
        &stored.headers,
        true,
    )
}

fn success_response<T: Serialize>(
    status: StatusCode,
    payload: &T,
    headers: BTreeMap<String, String>,
) -> Response {
    build_json_response(status, payload, &headers, false)
}

fn serialize_response<T: Serialize>(payload: &T) -> Result<SerializedResponse> {
    let body = serde_json::to_vec(payload).map_err(|error| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Internal,
            "payments.acp.experimental.serialize_response",
            format!("failed to serialize ACP experimental response payload: {error}"),
        )
    })?;

    Ok(SerializedResponse { body })
}

fn build_json_response<T: Serialize>(
    status: StatusCode,
    payload: &T,
    headers: &BTreeMap<String, String>,
    replayed: bool,
) -> Response {
    let mut response = (status, Json(payload)).into_response();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) =
            (axum::http::header::HeaderName::try_from(name.as_str()), HeaderValue::from_str(value))
        {
            response.headers_mut().insert(name, value);
        }
    }
    if replayed {
        response.headers_mut().insert("Idempotent-Replayed", HeaderValue::from_static("true"));
    }
    response
}

fn build_bytes_response(
    status: StatusCode,
    body: Vec<u8>,
    headers: &BTreeMap<String, String>,
    replayed: bool,
) -> Response {
    let mut response = Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("ACP experimental JSON response should build");
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) =
            (axum::http::header::HeaderName::try_from(name.as_str()), HeaderValue::from_str(value))
        {
            response.headers_mut().insert(name, value);
        }
    }
    if replayed {
        response.headers_mut().insert("Idempotent-Replayed", HeaderValue::from_static("true"));
    }
    response
}

fn missing_service_error(service: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        "payments.acp.experimental.missing_service",
        format!("ACP experimental router requires the `{service}` service before build()"),
    )
}

fn missing_route_response() -> Response {
    AcpApiError::from_adk(
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::NotFound,
            "payments.acp.experimental.route_not_configured",
            "ACP experimental route was not configured on this router",
        ),
        BTreeMap::new(),
    )
    .into_response()
}

struct SerializedResponse {
    body: Vec<u8>,
}

impl AcpApiError {
    fn from_verification(error: AcpVerificationError) -> Self {
        Self {
            status: StatusCode::from_u16(error.status_code())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            payload: AcpErrorResponse {
                r#type: error.response_type().to_string(),
                code: error.code().to_string(),
                message: error.to_string(),
                param: None,
            },
            headers: BTreeMap::new(),
        }
    }

    fn from_webhook_verification(error: MerchantWebhookVerificationError) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            payload: AcpErrorResponse {
                r#type: "invalid_request".to_string(),
                code: "invalid_signature".to_string(),
                message: error.to_string(),
                param: None,
            },
            headers: BTreeMap::new(),
        }
    }

    fn from_adk(error: AdkError, headers: BTreeMap<String, String>) -> Self {
        let status = StatusCode::from_u16(error.http_status_code())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let response_type = match error.category {
            ErrorCategory::Unauthorized => "unauthorized",
            ErrorCategory::RateLimited => "rate_limit_exceeded",
            ErrorCategory::Unavailable | ErrorCategory::Timeout => "service_unavailable",
            _ => "invalid_request",
        };
        let code = match error.category {
            ErrorCategory::NotFound => "not_found",
            ErrorCategory::Unauthorized => "unauthorized",
            ErrorCategory::RateLimited => "too_many_requests",
            ErrorCategory::Unavailable | ErrorCategory::Timeout => "service_unavailable",
            ErrorCategory::Forbidden => "forbidden",
            ErrorCategory::Unsupported => "unsupported",
            _ => error.code,
        };
        Self {
            status,
            payload: AcpErrorResponse {
                r#type: response_type.to_string(),
                code: code.to_string(),
                message: error.message,
                param: None,
            },
            headers,
        }
    }
}
