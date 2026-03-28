use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;

use crate::domain::{
    CommerceActor, CommerceMode, MerchantRef, PaymentProcessorRef, ProtocolDescriptor,
    TransactionId,
};
use crate::kernel::{DelegatedPaymentService, MerchantCheckoutService, TransactionLookup};
use crate::protocol::acp::mapper::{
    cancel_checkout_command, checkout_session_from_record, complete_checkout_command,
    create_checkout_command, delegate_payment_command, request_metadata_extensions,
    update_checkout_command,
};
use crate::protocol::acp::types::{
    AcpCancelSessionRequest, AcpCheckoutSessionCompleteRequest, AcpCheckoutSessionUpdateRequest,
    AcpCreateCheckoutSessionRequest, AcpDelegatePaymentRequest, AcpErrorResponse,
};
use crate::protocol::acp::verification::{
    AcpRequestVerifier, AcpVerificationConfig, AcpVerificationError, StoredIdempotentResponse,
};

/// Static commerce identity and merchant defaults for ACP requests.
#[derive(Clone)]
pub struct AcpContextTemplate {
    pub session_identity: Option<AdkIdentity>,
    pub actor: CommerceActor,
    pub merchant_of_record: MerchantRef,
    pub payment_processor: Option<PaymentProcessorRef>,
    pub mode: CommerceMode,
}

/// Builder for ACP stable `2026-01-30` routes.
pub struct AcpRouterBuilder {
    context_template: AcpContextTemplate,
    merchant_checkout_service: Option<Arc<dyn MerchantCheckoutService>>,
    delegated_payment_service: Option<Arc<dyn DelegatedPaymentService>>,
    verification: AcpVerificationConfig,
}

#[derive(Clone)]
struct AcpRouterState {
    context_template: AcpContextTemplate,
    merchant_checkout_service: Arc<dyn MerchantCheckoutService>,
    delegated_payment_service: Arc<dyn DelegatedPaymentService>,
    verifier: AcpRequestVerifier,
}

struct AcpApiError {
    status: StatusCode,
    payload: AcpErrorResponse,
    headers: BTreeMap<String, String>,
}

impl AcpRouterBuilder {
    /// Creates a new ACP router builder from static commerce context defaults.
    #[must_use]
    pub fn new(context_template: AcpContextTemplate) -> Self {
        Self {
            context_template,
            merchant_checkout_service: None,
            delegated_payment_service: None,
            verification: AcpVerificationConfig::default(),
        }
    }

    /// Attaches the canonical checkout backend used by ACP routes.
    #[must_use]
    pub fn with_merchant_checkout_service(
        mut self,
        merchant_checkout_service: Arc<dyn MerchantCheckoutService>,
    ) -> Self {
        self.merchant_checkout_service = Some(merchant_checkout_service);
        self
    }

    /// Attaches the delegated-payment backend used by ACP delegate-payment.
    #[must_use]
    pub fn with_delegated_payment_service(
        mut self,
        delegated_payment_service: Arc<dyn DelegatedPaymentService>,
    ) -> Self {
        self.delegated_payment_service = Some(delegated_payment_service);
        self
    }

    /// Replaces the ACP request verification profile.
    #[must_use]
    pub fn with_verification(mut self, verification: AcpVerificationConfig) -> Self {
        self.verification = verification;
        self
    }

    /// Builds the ACP stable router.
    ///
    /// # Errors
    ///
    /// Returns an error when required backend services are missing.
    pub fn build(self) -> Result<Router> {
        let merchant_checkout_service =
            self.merchant_checkout_service.ok_or_else(|| missing_service_error("merchant"))?;
        let delegated_payment_service = self
            .delegated_payment_service
            .ok_or_else(|| missing_service_error("delegate_payment"))?;
        let state = AcpRouterState {
            context_template: self.context_template,
            merchant_checkout_service,
            delegated_payment_service,
            verifier: AcpRequestVerifier::new(self.verification),
        };

        Ok(Router::new()
            .route("/checkout_sessions", post(create_checkout_session))
            .route(
                "/checkout_sessions/{checkout_session_id}",
                post(update_checkout_session).get(get_checkout_session),
            )
            .route(
                "/checkout_sessions/{checkout_session_id}/complete",
                post(complete_checkout_session),
            )
            .route("/checkout_sessions/{checkout_session_id}/cancel", post(cancel_checkout_session))
            .route("/agentic_commerce/delegate_payment", post(delegate_payment))
            .with_state(state))
    }
}

impl IntoResponse for AcpApiError {
    fn into_response(self) -> Response {
        build_json_response(self.status, &self.payload, &self.headers, false)
    }
}

async fn create_checkout_session(
    State(state): State<AcpRouterState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let verified = match state.verifier.verify("POST", "/checkout_sessions", &headers, &body).await
    {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpCreateCheckoutSessionRequest =
        match parse_json_body("/checkout_sessions", &body) {
            Ok(request) => request,
            Err(error) => {
                return finalize_error(
                    &state,
                    "POST",
                    "/checkout_sessions",
                    verified.headers.idempotency_key.as_deref(),
                    &body,
                    error,
                )
                .await;
            }
        };
    let transaction_id = format!("checkout_session_{}", uuid::Uuid::new_v4().simple());
    let context = commerce_context(&state, &transaction_id, "create_checkout_session", &verified);
    let command = create_checkout_command(request, context);
    let result = state.merchant_checkout_service.create_checkout(command).await;

    finalize_post_result(
        &state,
        "/checkout_sessions",
        verified,
        &body,
        StatusCode::CREATED,
        result.map(|record| checkout_session_from_record(&record, false)),
    )
    .await
}

async fn update_checkout_session(
    State(state): State<AcpRouterState>,
    Path(checkout_session_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("/checkout_sessions/{checkout_session_id}");
    let verified = match state.verifier.verify("POST", &path, &headers, &body).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpCheckoutSessionUpdateRequest = match parse_json_body(&path, &body) {
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
    let context =
        commerce_context(&state, &checkout_session_id, "update_checkout_session", &verified);
    let command = update_checkout_command(request, context);
    let result = state.merchant_checkout_service.update_checkout(command).await;

    finalize_post_result(
        &state,
        &path,
        verified,
        &body,
        StatusCode::OK,
        result.map(|record| checkout_session_from_record(&record, false)),
    )
    .await
}

async fn get_checkout_session(
    State(state): State<AcpRouterState>,
    Path(checkout_session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let path = format!("/checkout_sessions/{checkout_session_id}");
    let verified = match state.verifier.verify("GET", &path, &headers, &[]).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let lookup = TransactionLookup {
        transaction_id: TransactionId::from(checkout_session_id),
        session_identity: state.context_template.session_identity.clone(),
    };
    let result = state.merchant_checkout_service.get_checkout(lookup).await.and_then(|record| {
        record.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "payments.acp.checkout.not_found",
                "ACP checkout session was not found",
            )
        })
    });

    match result {
        Ok(record) => success_response(
            StatusCode::OK,
            &checkout_session_from_record(&record, false),
            response_headers(&verified.headers),
        ),
        Err(error) => {
            AcpApiError::from_adk(error, response_headers(&verified.headers)).into_response()
        }
    }
}

async fn complete_checkout_session(
    State(state): State<AcpRouterState>,
    Path(checkout_session_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("/checkout_sessions/{checkout_session_id}/complete");
    let verified = match state.verifier.verify("POST", &path, &headers, &body).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpCheckoutSessionCompleteRequest = match parse_json_body(&path, &body) {
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
    let context =
        commerce_context(&state, &checkout_session_id, "complete_checkout_session", &verified);
    let command = complete_checkout_command(request, context);
    let result = state.merchant_checkout_service.complete_checkout(command).await;

    finalize_post_result(
        &state,
        &path,
        verified,
        &body,
        StatusCode::OK,
        result.map(|record| checkout_session_from_record(&record, true)),
    )
    .await
}

async fn cancel_checkout_session(
    State(state): State<AcpRouterState>,
    Path(checkout_session_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("/checkout_sessions/{checkout_session_id}/cancel");
    let verified = match state.verifier.verify("POST", &path, &headers, &body).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpCancelSessionRequest = if body.is_empty() {
        AcpCancelSessionRequest::default()
    } else {
        match parse_json_body(&path, &body) {
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
        }
    };
    let context =
        commerce_context(&state, &checkout_session_id, "cancel_checkout_session", &verified);
    let command = cancel_checkout_command(request, context);
    let result = state.merchant_checkout_service.cancel_checkout(command).await;

    finalize_post_result(
        &state,
        &path,
        verified,
        &body,
        StatusCode::OK,
        result.map(|record| checkout_session_from_record(&record, false)),
    )
    .await
}

async fn delegate_payment(
    State(state): State<AcpRouterState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = "/agentic_commerce/delegate_payment";
    let verified = match state.verifier.verify("POST", path, &headers, &body).await {
        Ok(verified) => verified,
        Err(error) => return AcpApiError::from_verification(error).into_response(),
    };
    if let Some(replay) = verified.replay {
        return replay_response(replay);
    }

    let request: AcpDelegatePaymentRequest = match parse_json_body(path, &body) {
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
    let context = commerce_context(
        &state,
        &request.allowance.checkout_session_id,
        "delegate_payment",
        &verified,
    );
    let command = delegate_payment_command(request, context);
    let result = state.delegated_payment_service.delegate_payment(command).await;

    finalize_post_result(
        &state,
        path,
        verified,
        &body,
        StatusCode::CREATED,
        result.map(|result| crate::protocol::acp::types::AcpDelegatePaymentResponse {
            id: result.delegated_payment_id,
            created: result.created_at,
            metadata: result.metadata,
        }),
    )
    .await
}

fn commerce_context(
    state: &AcpRouterState,
    transaction_id: &str,
    operation: &str,
    verified: &crate::protocol::acp::verification::VerifiedRequest,
) -> crate::kernel::CommerceContext {
    crate::kernel::CommerceContext {
        transaction_id: TransactionId::from(transaction_id),
        session_identity: state.context_template.session_identity.clone(),
        actor: state.context_template.actor.clone(),
        merchant_of_record: state.context_template.merchant_of_record.clone(),
        payment_processor: state.context_template.payment_processor.clone(),
        mode: state.context_template.mode,
        protocol: ProtocolDescriptor::acp(verified.headers.api_version.clone()),
        extensions: request_metadata_extensions(
            operation,
            &verified.headers.api_version,
            verified.headers.request_id.as_deref(),
            verified.headers.idempotency_key.as_deref(),
            verified.headers.timestamp,
            verified.headers.signature_present,
        ),
    }
}

async fn finalize_post_result<T: Serialize>(
    state: &AcpRouterState,
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
    state: &AcpRouterState,
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
            "payments.acp.serialize_response",
            format!("failed to serialize ACP response payload: {error}"),
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
        .expect("ACP JSON response should build");
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
        "payments.acp.missing_service",
        format!("ACP router requires the `{service}` service before build()"),
    )
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
