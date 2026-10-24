//! The web home on a served harness (spec 3.3 §5.1, §10 S1): the engine `adk-surface` serves
//! at `/api/surface` and the page at `/ui/`.

use std::sync::Arc;

use adk_core::{ApprovalService, Result};
use adk_session::SessionService;
use adk_surface::{LocalSource, SurfaceEngine};

/// What the served harness's engine records: how each answer was given, and every command,
/// on the signed audit log, so "You said yes in Telegram" survives a restart (spec 3.3 §11).
#[derive(Clone)]
pub(crate) struct SurfaceRecords {
    pub(crate) trail: adk_core::audit_log::AuditTrail,
    pub(crate) answers: Arc<dyn adk_surface::AnswerLog>,
}

/// The engine over a served harness (spec 3.3 §6): its sessions and approval inbox, its
/// modes and standing rules, and, when resident, the kernel that runs the turns.
pub(crate) fn harness_engine(
    app_name: &str,
    harness: Arc<adk_harness::Harness>,
    sessions: Arc<dyn SessionService>,
    approvals: Option<Arc<dyn ApprovalService>>,
    kernel: Option<adk_kernel::Kernel>,
    records: Option<SurfaceRecords>,
) -> Result<SurfaceEngine> {
    let mut source = LocalSource::builder(app_name).sessions(sessions).harness(harness);
    if let Some(approvals) = approvals {
        source = source.approvals(approvals);
    }
    if let Some(kernel) = kernel {
        source = source.kernel(kernel, app_name);
    }
    // Decisions also wait in the terminal: `adk-rust approvals`, `attach`.
    let mut engine = SurfaceEngine::builder(Arc::new(source.build()?))
        .places([adk_surface::Place::terminal()])
        .agent_name(app_name, "ADK-Rust");
    if let Some(records) = records {
        engine = engine.audit(records.trail).answers(records.answers);
    }
    Ok(engine.build())
}

/// Most a command body may hold; a command is a few hundred bytes.
const COMMAND_LIMIT: usize = 1 << 20;

/// The home's routes, with **Stop all** and **Resume all** for the operator only (spec 3.3
/// §11 S-I10): on a served harness they freeze and unfreeze every run the server makes, as
/// the kernel's own freeze route does, which needs the operator too. Any other caller the
/// server admits gets `403` for them; every other command is unchanged.
pub(crate) fn operator_stops(routes: axum::Router) -> axum::Router {
    routes.layer(axum::middleware::from_fn(stops_need_the_operator))
}

async fn stops_need_the_operator(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if request.method() != axum::http::Method::POST
        || !request.uri().path().ends_with(adk_surface::web::paths::COMMANDS)
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, COMMAND_LIMIT).await else {
        return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let stops = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| {
            value.pointer("/command/type").and_then(|kind| kind.as_str().map(str::to_string))
        })
        .is_some_and(|kind| kind == "stopAll" || kind == "resumeAll");
    // No caller at all is the surface's own `401`.
    let caller =
        parts.extensions.get::<Option<adk_core::RequestContext>>().and_then(Option::as_ref);
    if stops
        && let Some(caller) = caller
        && !caller.scopes.iter().any(|scope| scope == crate::harness::OPERATOR_SCOPE)
    {
        let error = adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::Forbidden,
            "surface.operator_only",
            format!(
                "stopping and resuming every agent needs the `{}` scope: they freeze and \
                 unfreeze every run this server makes",
                crate::harness::OPERATOR_SCOPE
            ),
        );
        return (axum::http::StatusCode::FORBIDDEN, axum::Json(error.to_problem_json()))
            .into_response();
    }
    next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))).await
}
