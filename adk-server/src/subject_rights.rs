//! HTTP endpoints for a signed-in person's subject rights: export, erase, and linking the
//! visitor id they used before signing in.
//!
//! | Route | Does |
//! |-------|------|
//! | `GET /subject/export` | Everything the deployment's [`SubjectRights`] stores hold about the caller and its linked visitor ids |
//! | `POST /subject/erase` | Erases it; answers the [`ErasureReport`](adk_core::subject::ErasureReport) |
//! | `POST /subject/link` | Links the visitor id the request's token carries to the caller, once |
//!
//! Mount them with [`ServerBuilder::add_api_routes`](crate::ServerBuilder::add_api_routes),
//! so they sit under `/api` behind the server's `RequestContextExtractor`. Every route needs
//! an authenticated caller; the caller acts only on its own data.
//!
//! A link needs the visitor's token — the `AWP-Subject` header or the `__Host-adk_visitor`
//! cookie — and a fresh sign-in: the request context's `auth_time` metadata (Unix seconds,
//! such as an OIDC token's `auth_time` claim) no older than
//! [`with_max_sign_in_age`](SubjectRightsEndpoints::with_max_sign_in_age), ten minutes by
//! default. A visitor id links once; the response clears the visitor cookie, so the browser
//! continues as the signed-in person.

use adk_auth::VisitorTokens;
use adk_core::principal::SubjectRef;
use adk_core::subject::{SUBJECT_LINK_CONSUMED_CODE, SubjectRights};
use adk_core::{AdkError, RequestContext};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde_json::json;

/// The request context metadata key holding when the caller signed in, in Unix seconds.
pub const AUTH_TIME_METADATA: &str = "auth_time";

/// What the subject rights endpoints act on.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::VisitorTokens;
/// use adk_core::subject::{InMemoryConsentStore, InMemorySubjectRegistry, SubjectRights};
/// use adk_server::subject_rights::{SubjectRightsEndpoints, subject_rights_router};
///
/// let rights = SubjectRights::new(
///     Arc::new(InMemorySubjectRegistry::new()),
///     Arc::new(InMemoryConsentStore::new()),
/// )
/// .with_app("shop");
/// let tokens = VisitorTokens::new(b"the site's visitor token key, 32 bytes".to_vec());
/// let router = subject_rights_router(SubjectRightsEndpoints::new(rights, tokens));
/// # let _ = router;
/// ```
#[derive(Clone)]
pub struct SubjectRightsEndpoints {
    rights: SubjectRights,
    tokens: VisitorTokens,
    max_sign_in_age: chrono::Duration,
}

impl std::fmt::Debug for SubjectRightsEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubjectRightsEndpoints")
            .field("max_sign_in_age", &self.max_sign_in_age)
            .finish_non_exhaustive()
    }
}

impl SubjectRightsEndpoints {
    /// Endpoints over `rights`, verifying visitor tokens with `tokens`.
    pub fn new(rights: SubjectRights, tokens: VisitorTokens) -> Self {
        Self { rights, tokens, max_sign_in_age: chrono::Duration::minutes(10) }
    }

    /// Sets how recent the caller's sign-in must be for a link.
    #[must_use]
    pub fn with_max_sign_in_age(mut self, age: chrono::Duration) -> Self {
        self.max_sign_in_age = age;
        self
    }
}

/// The subject rights routes, for
/// [`ServerBuilder::add_api_routes`](crate::ServerBuilder::add_api_routes).
pub fn subject_rights_router(endpoints: SubjectRightsEndpoints) -> Router {
    Router::new()
        .route("/subject/export", get(export))
        .route("/subject/erase", post(erase))
        .route("/subject/link", post(link))
        .with_state(endpoints)
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response()
}

fn failed(error: &AdkError) -> Response {
    tracing::error!(error = %error, "subject rights request failed");
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    refuse(status, error.code, &error.message)
}

/// The signed-in caller as a subject, or the refusal for an anonymous request.
fn caller(context: Option<RequestContext>) -> Result<(SubjectRef, RequestContext), Response> {
    match context {
        Some(context) => Ok((SubjectRef::human(context.user_id.clone()), context)),
        None => Err(refuse(
            StatusCode::UNAUTHORIZED,
            "subject.unauthenticated",
            "subject rights need a signed-in caller; configure a RequestContextExtractor",
        )),
    }
}

async fn export(
    State(endpoints): State<SubjectRightsEndpoints>,
    Extension(context): Extension<Option<RequestContext>>,
) -> Response {
    let (subject, _) = match caller(context) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match endpoints.rights.export(&subject).await {
        Ok(export) => Json(export).into_response(),
        Err(error) => failed(&error),
    }
}

async fn erase(
    State(endpoints): State<SubjectRightsEndpoints>,
    Extension(context): Extension<Option<RequestContext>>,
) -> Response {
    let (subject, _) = match caller(context) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match endpoints.rights.erase(&subject).await {
        Ok(report) => (StatusCode::ACCEPTED, Json(report)).into_response(),
        Err(error) => failed(&error),
    }
}

async fn link(
    State(endpoints): State<SubjectRightsEndpoints>,
    Extension(context): Extension<Option<RequestContext>>,
    headers: HeaderMap,
) -> Response {
    let (subject, context) = match caller(context) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let signed_in = context
        .metadata
        .get(AUTH_TIME_METADATA)
        .and_then(|seconds| seconds.parse::<i64>().ok())
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0));
    if signed_in.is_none_or(|at| Utc::now() - at > endpoints.max_sign_in_age) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "subject.link.sign_in_required",
            "linking a visitor id needs a fresh sign-in; sign in again and retry",
        );
    }
    let token = headers
        .get("awp-subject")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(VisitorTokens::from_cookie_header)
                .map(str::to_owned)
        });
    let Some(visitor) = token.and_then(|token| endpoints.tokens.verify(&token)) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "subject.link.no_visitor",
            "the request carries no valid visitor token to link",
        );
    };
    match endpoints.rights.link(&visitor.subject(), &subject).await {
        Ok(_) => {
            let mut response = Json(json!({ "linked": true })).into_response();
            // The visitor id is spent; the browser continues as the signed-in person.
            let cleared = format!(
                "{}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                adk_auth::VISITOR_COOKIE
            );
            if let Ok(value) = HeaderValue::from_str(&cleared) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        Err(error) if error.code == SUBJECT_LINK_CONSUMED_CODE => {
            refuse(StatusCode::CONFLICT, error.code, &error.message)
        }
        Err(error) => failed(&error),
    }
}
