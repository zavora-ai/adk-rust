//! Paired devices on the surface API (spec 3.3 §7.2, §10 K2, §11 S-I10): the routes a device
//! answers and stops through, and the map of which device scope opens each surface route.

use adk_kernel::pairing::DeviceScope;
use axum::extract::{Path, State};
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};

use super::phone::{AnswerRequest, StopRequest};
use super::{Context, Unauthenticated, caller, device_of, failed, paths, unauthenticated};
use crate::engine::{Command, CommandOutcome, SurfaceEngine};
use crate::error::SurfaceError;
use crate::origin::SurfaceKind;

/// The pairing whose devices' feeds end with them, for [`router_with_devices`].
#[derive(Clone)]
pub(crate) struct DeviceEnds(pub(crate) adk_kernel::pairing::Pairing);

/// [`router`](super::router) for a server that pairs devices (spec 3.3 §10 K2, S-I7): a
/// paired device's feed ends when the device is removed or its pairing ends — at once on this
/// kernel, within the pairing's re-read (five seconds by default) on another replica.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(source: LocalSource, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
/// let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
/// let builder = builder.add_api_routes(adk_surface::web::router_with_devices(engine, pairing));
/// # let _ = builder;
/// # }
/// ```
pub fn router_with_devices(engine: SurfaceEngine, pairing: adk_kernel::pairing::Pairing) -> Router {
    super::router(engine).layer(Extension(DeviceEnds(pairing)))
}

/// The routes a paired device answers and stops through, relative to `/api` (spec 3.3 §7.2,
/// §10 K2): mount them with `ServerBuilder::add_api_routes` beside [`router`](super::router),
/// over the same engine.
///
/// | Route | Answer |
/// |---|---|
/// | `POST /surface/decisions/{id}/answer` | [`AnswerRequest`] → [`CommandOutcome`]: the caller's answer at the version it saw |
/// | `POST /surface/stop` | [`StopRequest`] → [`CommandOutcome`]: **Stop all**, for a paired device only; every other caller uses `POST /surface/commands` |
///
/// A device answers as its kind's surface (a phone by touch), never as the web home, and a
/// high-risk approval needs "Confirm it's you" (`adk_surface::mobile`), never this route.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(source: LocalSource, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
/// let builder = builder
///     .add_api_routes(adk_surface::web::router(engine.clone()))
///     .add_api_routes(adk_surface::web::devices_router(engine));
/// # let _ = builder;
/// # }
/// ```
pub fn devices_router(engine: SurfaceEngine) -> Router {
    Router::new()
        .route(paths::ANSWER, post(answer))
        .route(paths::STOP, post(stop))
        .with_state(engine)
}

async fn answer(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(id): Path<String>,
    Json(request): Json<AnswerRequest>,
) -> Response {
    let caller = match caller(context, SurfaceKind::Web, None) {
        Ok(caller) => caller,
        Err(Unauthenticated) => return unauthenticated(),
    };
    let command = Command::Decide {
        decision_id: id,
        version: request.version,
        answer: request.answer,
        edited_args: request.edited_args,
        reason: request.reason,
        expanded: request.expanded,
    };
    match engine.execute(&caller, command).await {
        Ok(outcome) => Json::<CommandOutcome>(outcome).into_response(),
        Err(error) => failed(error),
    }
}

async fn stop(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Json(request): Json<StopRequest>,
) -> Response {
    if let Some(Extension(Some(context))) = &context
        && device_of(context).is_none()
    {
        return failed(SurfaceError::NotHere(
            "this route is a paired device's Stop all; the home and the terminal stop with a \
             stopAll command"
                .into(),
        ));
    }
    let caller = match caller(context, SurfaceKind::Web, None) {
        Ok(caller) => caller,
        Err(Unauthenticated) => return unauthenticated(),
    };
    match engine.execute(&caller, Command::StopAll { reason: request.reason }).await {
        Ok(outcome) => Json::<CommandOutcome>(outcome).into_response(),
        Err(error) => failed(error),
    }
}

/// The device scope each surface route needs, or `None` when the route is closed to devices
/// (spec 3.3 §7.2, §10 K2, §11 S-I10). The server registers it beside the kernel's own map,
/// so the kernel never names a surface path. `path` may carry the `/api` prefix and a query.
///
/// | Routes | Scope |
/// |---|---|
/// | Every `GET /surface/…` but the operator's phone reads | `read` |
/// | `POST /surface/decisions/{id}/answer`, `…/confirm/options`, `…/confirm`, `…/take-over` | `decide` |
/// | `POST /surface/stop` | `stop` |
/// | The phone's own `/surface/phone` and `/surface/phone/push…` | `read` |
/// | `/surface/phone/passkey…` | `decide` |
/// | `POST /surface/commands`, `/surface/phone/qr`, `/surface/phone/devices`, `/surface/phone/push/deliveries` | closed |
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::DeviceScope;
/// use axum::http::Method;
/// use adk_surface::web::device_scope_for;
///
/// assert_eq!(device_scope_for(&Method::GET, "/api/surface/decisions?surface=phone"), Some(DeviceScope::Read));
/// assert_eq!(device_scope_for(&Method::POST, "/api/surface/decisions/apr_1/answer"), Some(DeviceScope::Decide));
/// assert_eq!(device_scope_for(&Method::POST, "/api/surface/stop"), Some(DeviceScope::Stop));
/// assert_eq!(device_scope_for(&Method::POST, "/api/surface/commands"), None);
/// ```
pub fn device_scope_for(method: &Method, path: &str) -> Option<DeviceScope> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let path = path.strip_prefix("/api").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    match (method.as_str(), segments.as_slice()) {
        // The operator's own phone reads and the pairing QR code.
        ("GET", ["surface", "phone", "devices"] | ["surface", "phone", "push", "deliveries"])
        | (_, ["surface", "phone", "qr"] | ["surface", "commands"]) => None,
        ("GET", ["surface", "phone"])
        | ("PUT" | "DELETE", ["surface", "phone", "push"])
        | ("POST", ["surface", "phone", "push", "received"]) => Some(DeviceScope::Read),
        ("POST", ["surface", "phone", "passkey"] | ["surface", "phone", "passkey", "options"])
        | (
            "POST",
            ["surface", "decisions", _, "answer" | "confirm" | "take-over"]
            | ["surface", "decisions", _, "confirm", "options"],
        ) => Some(DeviceScope::Decide),
        ("POST", ["surface", "stop"]) => Some(DeviceScope::Stop),
        ("GET", ["surface", ..]) => Some(DeviceScope::Read),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phone's routes have their scopes; commands, the QR code and the operator's reads are
    /// closed (spec 3.3 §10 K2).
    #[test]
    fn every_surface_route_has_its_device_scope() {
        let cases: &[(Method, &str, Option<DeviceScope>)] = &[
            (Method::GET, "/api/surface/decisions?surface=phone", Some(DeviceScope::Read)),
            (Method::GET, "/api/surface/decisions/apr_1", Some(DeviceScope::Read)),
            (Method::GET, "/api/surface/sessions/s1/feed", Some(DeviceScope::Read)),
            (Method::GET, "/api/surface/status", Some(DeviceScope::Read)),
            (Method::GET, "/api/surface/phone", Some(DeviceScope::Read)),
            (Method::PUT, "/api/surface/phone/push", Some(DeviceScope::Read)),
            (Method::DELETE, "/api/surface/phone/push", Some(DeviceScope::Read)),
            (Method::POST, "/api/surface/phone/push/received", Some(DeviceScope::Read)),
            (Method::POST, "/api/surface/phone/passkey/options", Some(DeviceScope::Decide)),
            (Method::POST, "/api/surface/phone/passkey", Some(DeviceScope::Decide)),
            (Method::POST, "/api/surface/decisions/apr_1/answer", Some(DeviceScope::Decide)),
            (
                Method::POST,
                "/api/surface/decisions/apr_1/confirm/options",
                Some(DeviceScope::Decide),
            ),
            (Method::POST, "/api/surface/decisions/apr_1/confirm", Some(DeviceScope::Decide)),
            (Method::POST, "/api/surface/decisions/apr_1/take-over", Some(DeviceScope::Decide)),
            (Method::POST, "/api/surface/stop", Some(DeviceScope::Stop)),
            (Method::POST, "/api/surface/commands", None),
            (Method::POST, "/api/surface/phone/qr", None),
            (Method::GET, "/api/surface/phone/devices", None),
            (Method::GET, "/api/surface/phone/push/deliveries", None),
            (Method::DELETE, "/api/surface/phone/passkey/abc", None),
            (Method::POST, "/api/surface/decisions/apr_1", None),
            (Method::POST, "/api/kernel/unfreeze", None),
            (Method::GET, "/api/kernel/health", None),
        ];
        for (method, path, scope) in cases {
            assert_eq!(device_scope_for(method, path), *scope, "{method} {path}");
        }
    }
}
