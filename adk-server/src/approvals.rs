//! Approval inbox endpoints, a background ticker, and a signed webhook notifier for an
//! [`ApprovalService`].
//!
//! Mount the endpoints with [`ServerBuilder::with_approval_service`](crate::ServerBuilder::with_approval_service),
//! which places them behind the server's auth middleware. Every route needs an
//! authenticated caller: without a `RequestContextExtractor`, or without a request
//! context, they answer `401`. The caller is the approver: by default the person named
//! by `RequestContext::user_id`, a member of the groups in `RequestContext::scopes`
//! ([`ApprovalEndpoints::with_identity`] changes the mapping).
//!
//! | Route | Effect |
//! |-------|--------|
//! | `GET /api/approvals` | The caller's inbox. Query: `state` (repeatable via comma list), `group`, `app`, `cursor`, `limit` |
//! | `GET /api/approvals/{id}` | One request the caller may decide, cancel, or asked for |
//! | `POST /api/approvals/{id}/decide` | Body `{"expectedVersion": 3, "action": "approve" \| "edit" \| "reject" \| "respond", "editedArgs": {...}, "reason": "...", "via": {"kind": "human", "id": "ana"}}` |
//! | `POST /api/approvals/{id}/cancel` | Body `{"reason": "..."}`; the requester or an approver |
//! | `POST /api/approvals/delegations` | Body `{"to": {"kind": "human", "id": "bo"}, "notBefore": "...", "notAfter": "...", "scope": {...}}`; delegates the caller's authority |
//! | `DELETE /api/approvals/delegations/{id}?toKind=human&toId=bo` | Revokes a delegation the caller granted or holds |
//!
//! Errors are problem documents with the service's error code: `409` for
//! `approval.version_conflict` (re-read and decide again), `403` for self-approval or a
//! caller who is not an approver, `404` for an unknown request. Request bodies omit the
//! call's fingerprint, which carries the unredacted arguments.

use std::sync::Arc;
use std::time::Duration;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    AdkError, ApprovalAction, ApprovalDecision, ApprovalDelegation, ApprovalNotification,
    ApprovalNotifier, ApprovalRequest, ApprovalService, ApprovalState, DelegationScope,
    DelegationWindow, ErrorCategory, ErrorComponent, InboxQuery, RequestContext,
};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

/// Maps an authenticated request to the approver it acts as: a subject and its groups.
pub type ApproverIdentity = Arc<dyn Fn(&RequestContext) -> (SubjectRef, Vec<String>) + Send + Sync>;

/// State for the approval endpoints.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_server::approvals::{ApprovalEndpoints, approvals_router};
///
/// let service = Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
/// let router = approvals_router(ApprovalEndpoints::new(service));
/// # let _ = router;
/// ```
#[derive(Clone)]
pub struct ApprovalEndpoints {
    service: Arc<dyn ApprovalService>,
    identity: ApproverIdentity,
}

impl ApprovalEndpoints {
    /// Endpoints over `service`, with callers mapped to `human(user_id)` in the groups
    /// named by their scopes.
    pub fn new(service: Arc<dyn ApprovalService>) -> Self {
        Self {
            service,
            identity: Arc::new(|caller: &RequestContext| {
                (SubjectRef::human(caller.user_id.clone()), caller.scopes.clone())
            }),
        }
    }

    /// Maps callers to approvers with `identity`.
    #[must_use]
    pub fn with_identity(
        mut self,
        identity: impl Fn(&RequestContext) -> (SubjectRef, Vec<String>) + Send + Sync + 'static,
    ) -> Self {
        self.identity = Arc::new(identity);
        self
    }

    fn caller(
        &self,
        context: Option<RequestContext>,
    ) -> Result<(SubjectRef, Vec<String>), Response> {
        match context {
            Some(context) => Ok((self.identity)(&context)),
            None => Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "approval endpoints need an authenticated caller; configure a \
                              RequestContextExtractor"
                })),
            )
                .into_response()),
        }
    }

    /// Whether `subject` in `groups` may see `request`: an approver now, directly or by
    /// a live delegation, or the requester.
    async fn may_see(
        &self,
        request: &ApprovalRequest,
        subject: &SubjectRef,
        groups: &[String],
    ) -> Result<bool, AdkError> {
        if request.is_eligible(subject, groups) || request.is_self(subject) {
            return Ok(true);
        }
        let now = Utc::now();
        Ok(self
            .service
            .delegations_to(subject)
            .await?
            .iter()
            .any(|delegation| delegation.applies_to(request, now)))
    }
}

impl std::fmt::Debug for ApprovalEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalEndpoints").field("service", &self.service).finish_non_exhaustive()
    }
}

/// The approval routes, relative to `/api`. Mount them behind the auth middleware, as
/// [`ServerBuilder::with_approval_service`](crate::ServerBuilder::with_approval_service)
/// does.
pub fn approvals_router(endpoints: ApprovalEndpoints) -> Router {
    Router::new()
        .route("/approvals", get(inbox))
        .route("/approvals/delegations", post(create_delegation))
        .route("/approvals/delegations/{id}", delete(revoke_delegation))
        .route("/approvals/{id}", get(get_request))
        .route("/approvals/{id}/decide", post(decide))
        .route("/approvals/{id}/cancel", post(cancel))
        .with_state(endpoints)
}

/// Runs [`ApprovalService::tick`] every `interval` until the task is aborted, so
/// requests expire and escalate on time. Safe to run on every replica.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
///
/// # async fn demo() {
/// let service = Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
/// let ticker = adk_server::approvals::spawn_approval_ticker(service, Duration::from_secs(30));
/// # ticker.abort();
/// # }
/// ```
pub fn spawn_approval_ticker(
    service: Arc<dyn ApprovalService>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(interval);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            timer.tick().await;
            match service.tick(Utc::now()).await {
                Ok(report) if report.expired.is_empty() && report.escalated.is_empty() => {}
                Ok(report) => tracing::info!(
                    approvals.expired = report.expired.len(),
                    approvals.escalated = report.escalated.len(),
                    "approval tick"
                ),
                Err(error) => tracing::warn!(error = %error, "approval tick failed"),
            }
        }
    })
}

fn problem(error: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(error.to_problem_json())).into_response()
}

fn not_found(id: &str) -> Response {
    problem(&AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::NotFound,
        "approval.not_found",
        format!("no approval request '{id}'"),
    ))
}

/// A request as shown to callers: without its fingerprint, which holds the raw
/// arguments.
fn view(request: &ApprovalRequest) -> Value {
    let mut value = serde_json::to_value(request).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("fingerprint");
    }
    value
}

/// `GET /api/approvals` query.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxParams {
    /// Comma-separated states; defaults to pending.
    #[serde(default)]
    pub state: Option<String>,
    /// List one of the caller's groups' queue instead of the caller's inbox.
    #[serde(default)]
    pub group: Option<String>,
    /// Only one application.
    #[serde(default)]
    pub app: Option<String>,
    /// Page token.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size, at most 200.
    #[serde(default)]
    pub limit: Option<usize>,
}

async fn inbox(
    State(endpoints): State<ApprovalEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Query(params): Query<InboxParams>,
) -> Response {
    let (subject, groups) = match endpoints.caller(caller) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let mut states = Vec::new();
    for name in params.state.iter().flat_map(|states| states.split(',')) {
        match serde_json::from_value::<ApprovalState>(Value::String(name.trim().to_string())) {
            Ok(state) => states.push(state),
            Err(_) => {
                return problem(&AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::InvalidInput,
                    "approval.invalid_state",
                    format!("unknown approval state '{name}'"),
                ));
            }
        }
    }
    let mut query = match &params.group {
        // A group's queue is visible only to its members.
        Some(group) if groups.contains(group) => InboxQuery::for_group(group.clone()),
        Some(group) => {
            return problem(&AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Forbidden,
                "approval.not_a_member",
                format!("the caller is not a member of '{group}'"),
            ));
        }
        None => InboxQuery::for_approver(subject, groups),
    }
    .with_states(states)
    .with_cursor(params.cursor)
    .with_limit(params.limit.unwrap_or(50).clamp(1, 200));
    if let Some(app) = params.app {
        query = query.with_app_name(app);
    }
    match endpoints.service.inbox(query).await {
        Ok(page) => Json(serde_json::json!({
            "items": page.items.iter().map(view).collect::<Vec<_>>(),
            "nextCursor": page.next_cursor,
        }))
        .into_response(),
        Err(error) => problem(&error),
    }
}

async fn get_request(
    State(endpoints): State<ApprovalEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Path(id): Path<String>,
) -> Response {
    let (subject, groups) = match endpoints.caller(caller) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match endpoints.service.get(&id).await {
        Ok(Some(request)) => match endpoints.may_see(&request, &subject, &groups).await {
            Ok(true) => Json(view(&request)).into_response(),
            Ok(false) => not_found(&id),
            Err(error) => problem(&error),
        },
        Ok(None) => not_found(&id),
        Err(error) => problem(&error),
    }
}

/// `POST /api/approvals/{id}/decide` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideBody {
    /// The version the caller read; a stale one answers `409`.
    pub expected_version: u64,
    /// What the caller does.
    pub action: ApprovalAction,
    /// Replacement arguments for `edit`.
    #[serde(default)]
    pub edited_args: Option<Value>,
    /// Rejection reason, or the text of `respond`.
    #[serde(default)]
    pub reason: Option<String>,
    /// Act under the delegation from this subject.
    #[serde(default)]
    pub via: Option<SubjectRef>,
}

async fn decide(
    State(endpoints): State<ApprovalEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Path(id): Path<String>,
    Json(body): Json<DecideBody>,
) -> Response {
    let (subject, groups) = match endpoints.caller(caller) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let mut decision = match body.action {
        ApprovalAction::Approve => ApprovalDecision::approve(subject),
        ApprovalAction::Edit => {
            ApprovalDecision::edit(subject, body.edited_args.unwrap_or(Value::Null))
        }
        ApprovalAction::Reject => {
            ApprovalDecision::reject(subject, body.reason.clone().unwrap_or_default())
        }
        ApprovalAction::Respond => {
            ApprovalDecision::respond(subject, body.reason.clone().unwrap_or_default())
        }
    }
    .with_groups(groups);
    if matches!(body.action, ApprovalAction::Edit) && decision.edited_args == Some(Value::Null) {
        decision.edited_args = None;
    }
    if matches!(body.action, ApprovalAction::Reject)
        && body.reason.as_deref().is_none_or(str::is_empty)
    {
        decision.reason = None;
    }
    if let Some(via) = body.via {
        decision = decision.via(via);
    }
    match endpoints.service.decide(&id, body.expected_version, decision).await {
        Ok(request) => Json(view(&request)).into_response(),
        Err(error) => problem(&error),
    }
}

/// `POST /api/approvals/{id}/cancel` body.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelBody {
    /// Why; reported to the model as part of the denial.
    #[serde(default)]
    pub reason: Option<String>,
}

async fn cancel(
    State(endpoints): State<ApprovalEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Path(id): Path<String>,
    body: Option<Json<CancelBody>>,
) -> Response {
    let (subject, groups) = match endpoints.caller(caller) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let request = match endpoints.service.get(&id).await {
        Ok(Some(request)) => request,
        Ok(None) => return not_found(&id),
        Err(error) => return problem(&error),
    };
    match endpoints.may_see(&request, &subject, &groups).await {
        Ok(true) => {}
        Ok(false) => return not_found(&id),
        Err(error) => return problem(&error),
    }
    let reason = body.and_then(|Json(body)| body.reason);
    match endpoints.service.cancel(&id, reason).await {
        Ok(request) => Json(view(&request)).into_response(),
        Err(error) => problem(&error),
    }
}

/// `POST /api/approvals/delegations` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateBody {
    /// The delegate.
    pub to: SubjectRef,
    /// When the delegation starts; defaults to now.
    #[serde(default)]
    pub not_before: Option<DateTime<Utc>>,
    /// When it ends.
    pub not_after: DateTime<Utc>,
    /// Which requests it covers; defaults to all.
    #[serde(default)]
    pub scope: Option<DelegationScope>,
}

async fn create_delegation(
    State(endpoints): State<ApprovalEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Json(body): Json<DelegateBody>,
) -> Response {
    let (subject, groups) = match endpoints.caller(caller) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let window = DelegationWindow::new(body.not_before.unwrap_or_else(Utc::now), body.not_after);
    let delegation = ApprovalDelegation::new(subject, body.to, window)
        .with_from_groups(groups)
        .with_scope(body.scope.unwrap_or_default());
    match endpoints.service.delegate(delegation).await {
        Ok(delegation) => (StatusCode::CREATED, Json(delegation)).into_response(),
        Err(error) => problem(&error),
    }
}

/// `DELETE /api/approvals/delegations/{id}` query: the delegate, so the delegation can
/// be found.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeParams {
    /// The delegate's kind.
    pub to_kind: SubjectKind,
    /// The delegate's ID.
    pub to_id: String,
}

async fn revoke_delegation(
    State(endpoints): State<ApprovalEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Path(id): Path<String>,
    Query(params): Query<RevokeParams>,
) -> Response {
    let (subject, _) = match endpoints.caller(caller) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let to = SubjectRef::new(params.to_kind, params.to_id);
    let held = match endpoints.service.delegations_to(&to).await {
        Ok(held) => held,
        Err(error) => return problem(&error),
    };
    let Some(delegation) = held.into_iter().find(|delegation| delegation.id == id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if delegation.from != subject && delegation.to != subject {
        return StatusCode::NOT_FOUND.into_response();
    }
    match endpoints.service.revoke_delegation(&id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => problem(&error),
    }
}

// ---------------------------------------------------------------------------
// Webhook notifier
// ---------------------------------------------------------------------------

/// Delivers [`ApprovalNotification`]s as signed webhooks in the Standard Webhooks
/// format, through the shared [`WebhookDelivery`](adk_core::webhook_delivery::WebhookDelivery).
///
/// Each delivery is a `POST` of the notification as JSON with `webhook-id` (the
/// notification ID, stable across retries), `webhook-timestamp` (Unix seconds), and
/// `webhook-signature` ([`adk_core::webhook`]). Deliveries run in the background with
/// retries, exponential backoff, and jitter, so a slow receiver never holds up a decision;
/// the last failure is dead-lettered. The default egress policy allows HTTPS to public
/// addresses only; [`with_delivery`](Self::with_delivery) changes it.
///
/// # Example
///
/// ```rust
/// use adk_server::approvals::WebhookApprovalNotifier;
///
/// let notifier = WebhookApprovalNotifier::new(
///     "https://hooks.example.com/approvals",
///     "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
/// )?;
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone)]
pub struct WebhookApprovalNotifier {
    endpoint: adk_core::webhook_delivery::WebhookEndpoint,
    secret: adk_core::webhook::WebhookSecret,
    delivery: Arc<adk_core::webhook_delivery::WebhookDelivery>,
}

impl std::fmt::Debug for WebhookApprovalNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookApprovalNotifier")
            .field("url", &self.endpoint.url)
            .finish_non_exhaustive()
    }
}

impl WebhookApprovalNotifier {
    /// Delivers to `url`, signing with `secret`: base64, optionally `whsec_`-prefixed.
    ///
    /// # Errors
    ///
    /// Returns `approval.webhook_secret` when the secret is not base64.
    pub fn new(url: impl Into<String>, secret: &str) -> Result<Self, AdkError> {
        let secret = adk_core::webhook::WebhookSecret::from_base64(secret).map_err(|e| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "approval.webhook_secret",
                format!("{e}"),
            )
        })?;
        Ok(Self {
            endpoint: adk_core::webhook_delivery::WebhookEndpoint::new(url, secret.clone()),
            secret,
            delivery: Arc::new(adk_core::webhook_delivery::WebhookDelivery::new()),
        })
    }

    /// Delivers through `delivery`, for a custom retry policy, dead-letter sink, or egress
    /// policy (for example `EgressPolicy::allow_private_networks()` for an in-cluster receiver).
    pub fn with_delivery(mut self, delivery: adk_core::webhook_delivery::WebhookDelivery) -> Self {
        self.delivery = Arc::new(delivery);
        self
    }

    /// The `webhook-signature` header for `body` sent as `id` at `timestamp`.
    pub fn signature(&self, id: &str, timestamp: i64, body: &str) -> String {
        self.secret.signature(id, timestamp, body.as_bytes())
    }
}

#[async_trait::async_trait]
impl ApprovalNotifier for WebhookApprovalNotifier {
    async fn notify(&self, notification: &ApprovalNotification) -> adk_core::Result<()> {
        let message =
            adk_core::webhook_delivery::WebhookMessage::json(notification.id.clone(), notification)
                .map_err(|e| {
                    AdkError::new(
                        ErrorComponent::Server,
                        ErrorCategory::Internal,
                        "approval.webhook_encode",
                        format!("could not encode the approval notification: {e}"),
                    )
                })?;
        let delivery = Arc::clone(&self.delivery);
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            let report = delivery.deliver(&endpoint, &message).await;
            tracing::debug!(
                webhook.id = %message.id,
                delivered = report.is_delivered(),
                "approval notification delivery finished"
            );
        });
        Ok(())
    }
}
