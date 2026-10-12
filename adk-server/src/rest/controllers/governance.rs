//! Admin endpoints for the organisation kill switch.
//!
//! Mounted by [`ServerBuilder::enable_governance_endpoints`](crate::ServerBuilder::enable_governance_endpoints)
//! behind the server's auth middleware:
//!
//! - `POST /api/admin/freeze` — freezes execution; on a mirrored control the freeze is
//!   written to the shared governance store and reaches every replica. Running durable
//!   runs are asked to cancel through the run store.
//! - `POST /api/admin/unfreeze` — lifts the freeze.
//! - `GET /api/admin/governance` — reports the current state.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{DurableRunStore, GovernanceControl, RequestContext, RunFilter, RunState};
use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

/// State for the governance admin endpoints.
#[derive(Clone)]
pub struct GovernanceController {
    control: GovernanceControl,
    run_stores: Vec<Arc<dyn DurableRunStore>>,
}

impl GovernanceController {
    /// Endpoints over `control`.
    pub fn new(control: GovernanceControl) -> Self {
        Self { control, run_stores: Vec::new() }
    }

    /// A freeze also requests cancellation of every running run in `store`.
    pub fn with_run_store(mut self, store: Arc<dyn DurableRunStore>) -> Self {
        if !self.run_stores.iter().any(|existing| Arc::ptr_eq(existing, &store)) {
            self.run_stores.push(store);
        }
        self
    }

    fn status(&self) -> GovernanceStatus {
        GovernanceStatus {
            frozen: self.control.is_frozen(),
            reason: self.control.reason(),
            mirrored: self.control.is_mirrored(),
            stale: self.control.is_stale(),
            cancelled_runs: None,
            error: None,
        }
    }
}

/// `POST /api/admin/freeze` request body.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeRequest {
    /// Why execution is frozen; carried in every error a frozen run returns.
    #[serde(default)]
    pub reason: Option<String>,
}

/// The kill switch's current state.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceStatus {
    /// Whether execution is frozen.
    pub frozen: bool,
    /// The freeze reason, while frozen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether the control mirrors a shared governance store.
    pub mirrored: bool,
    /// Whether a mirrored control has gone past its staleness limit.
    pub stale: bool,
    /// Running durable runs asked to cancel by this freeze.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_runs: Option<usize>,
    /// Why the shared store could not be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn issued_by(caller: Option<&RequestContext>) -> Option<SubjectRef> {
    caller.map(|caller| SubjectRef::human(caller.user_id.clone()))
}

/// `POST /api/admin/freeze`
///
/// Responds `503` with the status when the shared store could not be written; this
/// replica is frozen regardless.
pub async fn freeze(
    State(controller): State<GovernanceController>,
    Extension(caller): Extension<Option<RequestContext>>,
    Json(request): Json<FreezeRequest>,
) -> Response {
    let reason = request.reason.unwrap_or_else(|| "frozen by an administrator".to_string());
    tracing::warn!(
        caller.user_id = caller.as_ref().map(|c| c.user_id.as_str()).unwrap_or("anonymous"),
        governance.reason = %reason,
        "governance freeze requested"
    );
    let written = controller.control.freeze_replicated(reason, issued_by(caller.as_ref())).await;

    let mut cancelled = 0;
    for store in &controller.run_stores {
        let running = store.list(RunFilter::default().states([RunState::Running])).await;
        match running {
            Ok(runs) => {
                for run in runs {
                    match store.request_cancel(&run.run_id).await {
                        Ok(()) => cancelled += 1,
                        Err(error) => {
                            tracing::error!(run.id = %run.run_id, error = %error, "could not request cancellation of a running run")
                        }
                    }
                }
            }
            Err(error) => tracing::error!(error = %error, "could not list running runs to cancel"),
        }
    }
    tracing::warn!(runs.cancel_requested = cancelled, "governance freeze applied");

    let mut status = controller.status();
    status.cancelled_runs = Some(cancelled);
    match written {
        Ok(_) => Json(status).into_response(),
        Err(error) => {
            status.error = Some(error.to_string());
            (StatusCode::SERVICE_UNAVAILABLE, Json(status)).into_response()
        }
    }
}

/// `POST /api/admin/unfreeze`
///
/// Responds `503` with the status when the shared store could not be written; the
/// freeze then stays in force.
pub async fn unfreeze(
    State(controller): State<GovernanceController>,
    Extension(caller): Extension<Option<RequestContext>>,
) -> Response {
    tracing::warn!(
        caller.user_id = caller.as_ref().map(|c| c.user_id.as_str()).unwrap_or("anonymous"),
        "governance unfreeze requested"
    );
    match controller.control.unfreeze_replicated(issued_by(caller.as_ref())).await {
        Ok(_) => Json(controller.status()).into_response(),
        Err(error) => {
            let mut status = controller.status();
            status.error = Some(error.to_string());
            (StatusCode::SERVICE_UNAVAILABLE, Json(status)).into_response()
        }
    }
}

/// `GET /api/admin/governance`
pub async fn status(State(controller): State<GovernanceController>) -> Json<GovernanceStatus> {
    Json(controller.status())
}
