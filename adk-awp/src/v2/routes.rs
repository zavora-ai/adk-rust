//! The AWP 2.0 routes under `/awp/v2/` (spec 3.5 §7.9; AWP 2.0 §3.7).
//!
//! | Route | Answers |
//! |---|---|
//! | `GET /awp/v2/manifest` | The capability manifest for the caller's tier |
//! | `GET /awp/v2/trust` | The published trust rules |
//! | `GET /awp/v2/status` | The signed site status |
//! | `POST /awp/v2/capabilities/{name}` | A capability call: `200` output, `202` held call, or an error envelope |
//! | `GET /awp/v2/calls/{id}` | A held call; from `known` up only to the requester that made it |
//!
//! Every response names `AWP-Version: 2.0`; every failure is an `ErrorEnvelope`. Event,
//! consent, A2A and MCP routes are mounted by the edge server beside these.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use awp_types::{CapabilityEffect, ErrorCode, HeldCallState, SiteState, TrustRules};
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::from_fn;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use uuid::Uuid;

use super::capability::CapabilityRegistry;
use super::executor::{Binding, CapabilityExecutor, DirectExecutor, Invocation, InvocationOutcome};
use super::negotiation::v2_version_negotiation;
use super::response::{awp_json, error_response, read_only_response};
use super::store::{HeldCallStore, MemoryEdgeStores, StatusStore};
use super::tier::{PublicTierAssigner, TierAssigner};

/// Largest capability call body the routes read.
pub const MAX_CALL_BODY: usize = 64 * 1024;

/// The `Idempotency-Key` request field (AWP 2.0 §7.4.1).
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";

/// The `AWP-Consent` request field: consent receipt ids (AWP 2.0 §7.4.1, C12).
pub const CONSENT_HEADER: &str = "AWP-Consent";

/// Seconds a requester waits before polling a pending held call again.
pub const HELD_CALL_RETRY_AFTER: u64 = 30;

/// Shared state of the `/awp/v2/` routes (spec 3.5 §7.9).
#[derive(Clone)]
pub struct EdgeState {
    registry: Arc<CapabilityRegistry>,
    executor: Arc<dyn CapabilityExecutor>,
    tiers: Arc<dyn TierAssigner>,
    status: Arc<dyn StatusStore>,
    held_calls: Arc<dyn HeldCallStore>,
    trust_rules: Arc<TrustRules>,
    read_only: Arc<AtomicBool>,
}

impl std::fmt::Debug for EdgeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeState")
            .field("registry", &self.registry)
            .field("read_only", &self.read_only.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl EdgeState {
    /// Starts a builder over `registry`.
    pub fn builder(registry: CapabilityRegistry) -> EdgeStateBuilder {
        EdgeStateBuilder {
            registry: Arc::new(registry),
            executor: None,
            tiers: None,
            stores: None,
            memory: None,
            trust_rules: TrustRules::default(),
        }
    }

    /// The capability registry.
    pub fn registry(&self) -> &Arc<CapabilityRegistry> {
        &self.registry
    }

    /// Switches the edge's read-only mode: every non-`read_only` capability answers `503
    /// read_only` with the status document (spec 3.5 §7.9, A-I11).
    pub fn set_read_only(&self, read_only: bool) {
        self.read_only.store(read_only, Ordering::SeqCst);
    }

    /// Returns `true` in read-only mode, set here or by a `read_only` status.
    pub async fn is_read_only(&self) -> bool {
        if self.read_only.load(Ordering::SeqCst) {
            return true;
        }
        matches!(self.status.current().await, Ok(Some(status)) if status.state == SiteState::ReadOnly)
    }
}

/// Builder for [`EdgeState`]; only the registry is required.
///
/// Defaults: the fail-closed [`PublicTierAssigner`], in-memory stores, a [`DirectExecutor`]
/// over them, and empty trust rules.
pub struct EdgeStateBuilder {
    registry: Arc<CapabilityRegistry>,
    executor: Option<Arc<dyn CapabilityExecutor>>,
    tiers: Option<Arc<dyn TierAssigner>>,
    stores: Option<(Arc<dyn StatusStore>, Arc<dyn HeldCallStore>)>,
    memory: Option<MemoryEdgeStores>,
    trust_rules: TrustRules,
}

impl EdgeStateBuilder {
    /// Sets the executor; it must share the held-call store given to [`Self::stores`].
    pub fn executor(mut self, executor: Arc<dyn CapabilityExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Sets the tier assigner.
    pub fn tiers(mut self, tiers: Arc<dyn TierAssigner>) -> Self {
        self.tiers = Some(tiers);
        self
    }

    /// Sets the status and held-call stores the routes read.
    pub fn stores(
        mut self,
        status: Arc<dyn StatusStore>,
        held_calls: Arc<dyn HeldCallStore>,
    ) -> Self {
        self.stores = Some((status, held_calls));
        self
    }

    /// Uses the in-memory `stores` for status, held calls, idempotency and consent, so a
    /// caller holding a clone can publish status or issue receipts the routes see.
    pub fn memory_stores(mut self, stores: MemoryEdgeStores) -> Self {
        self.memory = Some(stores);
        self
    }

    /// Sets the published trust rules.
    pub fn trust_rules(mut self, trust_rules: TrustRules) -> Self {
        self.trust_rules = trust_rules;
        self
    }

    /// Builds the state.
    pub fn build(self) -> EdgeState {
        let memory = self.memory.unwrap_or_default();
        let (status, held_calls) = self.stores.unwrap_or_else(|| {
            (
                Arc::new(memory.status.clone()) as Arc<dyn StatusStore>,
                Arc::new(memory.held_calls.clone()) as Arc<dyn HeldCallStore>,
            )
        });
        let executor = self.executor.unwrap_or_else(|| {
            Arc::new(DirectExecutor::new(
                Arc::clone(&self.registry),
                Arc::clone(&held_calls),
                Arc::new(memory.idempotency.clone()),
                Arc::new(memory.consent.clone()),
            ))
        });
        EdgeState {
            registry: self.registry,
            executor,
            tiers: self.tiers.unwrap_or_else(|| Arc::new(PublicTierAssigner)),
            status,
            held_calls,
            trust_rules: Arc::new(self.trust_rules),
            read_only: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Builds the `/awp/v2/` routes (spec 3.5 §7.9).
///
/// No route changes the site, its content, its policies or its keys (AWP 2.0 §19.6).
///
/// # Example
///
/// ```
/// use adk_awp::v2::capability::CapabilityRegistry;
/// use adk_awp::v2::routes::{EdgeState, awp_v2_routes};
///
/// let state = EdgeState::builder(CapabilityRegistry::new("example.com")).build();
/// let app: axum::Router = awp_v2_routes(state);
/// # let _ = app;
/// ```
pub fn awp_v2_routes(state: EdgeState) -> Router {
    Router::new()
        .route("/awp/v2/manifest", get(manifest))
        .route("/awp/v2/trust", get(trust_rules))
        .route("/awp/v2/status", get(status))
        .route(
            "/awp/v2/capabilities/{name}",
            post(call_capability).layer(DefaultBodyLimit::max(MAX_CALL_BODY)),
        )
        .route("/awp/v2/calls/{id}", get(held_call))
        .layer(from_fn(v2_version_negotiation))
        .with_state(state)
}

async fn manifest(State(state): State<EdgeState>, request: Request) -> Response {
    let (head, _) = request.into_parts();
    let assignment = state.tiers.assign(&head).await;
    awp_json(StatusCode::OK, &state.registry.manifest_for(assignment.tier))
}

async fn trust_rules(State(state): State<EdgeState>) -> Response {
    awp_json(StatusCode::OK, state.trust_rules.as_ref())
}

async fn status(State(state): State<EdgeState>) -> Response {
    match state.status.current().await {
        Ok(Some(status)) => awp_json(StatusCode::OK, &status),
        Ok(None) => error_response(ErrorCode::ResourceNotFound, "no status is published", None),
        Err(error) => {
            tracing::error!(error = %error, "awp status store failed");
            error_response(ErrorCode::InternalError, "the status could not be read", None)
        }
    }
}

/// Reads `Idempotency-Key`: an RFC 8941 string (`"abc"`), or a bare token.
fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, &'static str> {
    let Some(value) = headers.get(IDEMPOTENCY_KEY_HEADER) else { return Ok(None) };
    let raw = value.to_str().map_err(|_| "Idempotency-Key must be ASCII")?.trim();
    let key = raw.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(raw);
    if key.is_empty() || key.len() > 255 {
        return Err("Idempotency-Key must hold 1 to 255 characters");
    }
    Ok(Some(key.to_owned()))
}

/// Reads `AWP-Consent`: a list of receipt ids, each an RFC 8941 string or a bare token.
fn consent_references(headers: &HeaderMap) -> Result<Vec<Uuid>, &'static str> {
    let mut ids = Vec::new();
    for value in headers.get_all(CONSENT_HEADER) {
        let raw = value.to_str().map_err(|_| "AWP-Consent must be ASCII")?;
        for member in raw.split(',').map(str::trim).filter(|member| !member.is_empty()) {
            let id =
                member.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(member);
            ids.push(id.parse().map_err(|_| "AWP-Consent must list consent receipt ids")?);
        }
    }
    Ok(ids)
}

async fn call_capability(
    State(state): State<EdgeState>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let (head, body) = request.into_parts();
    let assignment = state.tiers.assign(&head).await;
    let Some(tool) = state.registry.visible(&name, assignment.tier) else {
        return error_response(ErrorCode::CapabilityNotFound, "no such capability", None);
    };
    if tool.declaration().effect != CapabilityEffect::ReadOnly && state.is_read_only().await {
        return match state.status.current().await {
            Ok(Some(status)) => read_only_response(status),
            _ => error_response(ErrorCode::ReadOnly, "the site is read-only", None),
        };
    }
    let (key, consent) = match (idempotency_key(&head.headers), consent_references(&head.headers)) {
        (Ok(key), Ok(consent)) => (key, consent),
        (Err(message), _) | (_, Err(message)) => {
            return error_response(ErrorCode::InvalidRequest, message, None);
        }
    };
    let bytes = match axum::body::to_bytes(body, MAX_CALL_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(ErrorCode::InvalidRequest, "the body is too large", None);
        }
    };
    let input = if bytes.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(input) => input,
            Err(_) => {
                return error_response(ErrorCode::InvalidRequest, "the body must be JSON", None);
            }
        }
    };
    let invocation = Invocation {
        capability: name,
        input,
        tier: assignment.tier,
        requester: assignment.requester,
        idempotency_key: key,
        consent,
        binding: Binding::Http,
    };
    match state.executor.invoke(invocation).await {
        InvocationOutcome::Completed(output) => {
            (StatusCode::OK, axum::Json(output)).into_response()
        }
        InvocationOutcome::Held(call) => {
            let mut response = awp_json(StatusCode::ACCEPTED, &call);
            if let Ok(location) = HeaderValue::from_str(&call.status) {
                response.headers_mut().insert(header::LOCATION, location);
            }
            response
        }
        InvocationOutcome::Failed(error) => {
            super::response::envelope_response(&awp_types::ErrorEnvelope {
                awp: awp_types::VERSION_2_0,
                error,
                status: None,
            })
        }
    }
}

async fn held_call(
    State(state): State<EdgeState>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> Response {
    let (head, _) = request.into_parts();
    let assignment = state.tiers.assign(&head).await;
    let record = match state.held_calls.get(&id).await {
        Ok(record) => record,
        Err(error) => {
            tracing::error!(error = %error, "awp held-call store failed");
            return error_response(ErrorCode::InternalError, "the call could not be read", None);
        }
    };
    // From `known` up a call is visible only to the requester that made it.
    let visible = record
        .filter(|record| record.requester.is_none() || record.requester == assignment.requester);
    let Some(record) = visible else {
        return error_response(ErrorCode::ResourceNotFound, "no such call", None);
    };
    let mut response = awp_json(StatusCode::OK, &record.call);
    if record.call.state == HeldCallState::Pending
        && let Ok(value) = HeaderValue::from_str(&HELD_CALL_RETRY_AFTER.to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
