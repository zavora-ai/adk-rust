//! Admin endpoints for the organisation kill switch.
//!
//! Mounted by [`ServerBuilder::enable_governance_endpoints`](crate::ServerBuilder::enable_governance_endpoints)
//! behind the server's auth middleware:
//!
//! - `POST /api/admin/freeze` — freezes execution; on a mirrored control the freeze is
//!   written to the shared governance store and reaches every replica. Running durable
//!   runs are asked to cancel through the run store, or parked by a [`FreezeDelegate`]
//!   when one is registered (spec 3.2 §10 S4).
//! - `POST /api/admin/unfreeze` — lifts the freeze.
//! - `GET /api/admin/governance` — reports the current state, including what a freeze does
//!   with running runs (`runningRuns`: `park` under a delegate, `cancel` otherwise).
//!
//! When the control has an audit trail attached
//! ([`GovernanceControl::attach_audit`]), both changes are recorded with the calling
//! user as the actor. A freeze takes effect even when its record fails, and answers
//! `503`; an unfreeze that cannot be recorded under `AuditFailureMode::Block` is refused
//! with `503` and execution stays frozen.

use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::principal::SubjectRef;
use adk_core::{DurableRunStore, GovernanceControl, RequestContext, RunFilter, RunState};
use async_trait::async_trait;
use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

/// What a freeze does with the runs that were running (spec 3.2 §10 S4), as
/// `GET /api/admin/governance` reports it in `runningRuns`.
///
/// # Example
///
/// ```rust
/// use adk_server::rest::RunningRuns;
///
/// assert_eq!(serde_json::to_value(RunningRuns::Park).unwrap(), "park");
/// assert_eq!(serde_json::to_value(RunningRuns::Cancel).unwrap(), "cancel");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunningRuns {
    /// Running runs are parked `Waiting` at their next checkpoint and resume on unfreeze.
    Park,
    /// Running runs are asked to cancel and cannot resume.
    Cancel,
}

/// What a [`FreezeDelegate`]'s freeze did with the runs that were running.
///
/// # Example
///
/// ```rust
/// use adk_server::rest::FreezeReport;
///
/// let report = FreezeReport { parked_runs: 6, cancelled_runs: 0 };
/// assert_eq!(report.parked_runs + report.cancelled_runs, 6);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FreezeReport {
    /// Runs parked `Waiting`, to resume on unfreeze.
    pub parked_runs: usize,
    /// Runs asked to cancel.
    pub cancelled_runs: usize,
}

/// Freezes and unfreezes on behalf of the admin endpoints, in place of the controller's own
/// freeze (spec 3.2 §10 S4): a kernel registers one so `POST /api/admin/freeze` parks its
/// running processes instead of cancelling them, and `POST /api/admin/unfreeze` wakes them.
///
/// The delegate owns the whole change: it writes the control, with
/// [`GovernanceControl::freeze_recorded`] and [`unfreeze_recorded`], records the audit
/// records, and stops or resumes the runs. The endpoints report what it returns.
///
/// [`unfreeze_recorded`]: GovernanceControl::unfreeze_recorded
///
/// # Example
///
/// ```rust
/// use adk_core::GovernanceControl;
/// use adk_core::audit_log::AuditActor;
/// use adk_server::rest::{FreezeDelegate, FreezeReport};
/// use async_trait::async_trait;
///
/// struct ParkEverything(GovernanceControl);
///
/// #[async_trait]
/// impl FreezeDelegate for ParkEverything {
///     async fn freeze(&self, reason: &str, by: AuditActor) -> adk_core::Result<FreezeReport> {
///         self.0.freeze_recorded(reason, by).await?;
///         // ... suspend every running process at its next checkpoint ...
///         Ok(FreezeReport { parked_runs: 0, cancelled_runs: 0 })
///     }
///
///     async fn unfreeze(&self, by: AuditActor) -> adk_core::Result<()> {
///         self.0.unfreeze_recorded(by).await
///         // ... then wake what the freeze parked ...
///     }
/// }
/// ```
#[async_trait]
pub trait FreezeDelegate: Send + Sync {
    /// Freezes execution for `reason` as `by`, stops the running runs, and reports what
    /// happened to them.
    ///
    /// # Errors
    ///
    /// Returns the control's or the store's error; the endpoint answers `503` with it, and
    /// the delegate is expected to have frozen this replica regardless.
    async fn freeze(&self, reason: &str, by: AuditActor) -> adk_core::Result<FreezeReport>;

    /// Lifts the freeze as `by` and resumes what the freeze stopped.
    ///
    /// # Errors
    ///
    /// Returns the control's or the store's error; the endpoint answers `503` with it, and
    /// execution stays frozen.
    async fn unfreeze(&self, by: AuditActor) -> adk_core::Result<()>;

    /// What a freeze does with running runs, reported as `runningRuns`. A delegate parks.
    fn running_runs(&self) -> RunningRuns {
        RunningRuns::Park
    }
}

/// State for the governance admin endpoints.
#[derive(Clone)]
pub struct GovernanceController {
    control: GovernanceControl,
    run_stores: Vec<Arc<dyn DurableRunStore>>,
    delegate: Option<Arc<dyn FreezeDelegate>>,
}

impl std::fmt::Debug for GovernanceController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceController")
            .field("control", &self.control)
            .field("run_stores", &self.run_stores.len())
            .field("delegate", &self.delegate.is_some())
            .finish()
    }
}

impl GovernanceController {
    /// Endpoints over `control`.
    pub fn new(control: GovernanceControl) -> Self {
        Self { control, run_stores: Vec::new(), delegate: None }
    }

    /// A freeze also requests cancellation of every running run in `store`, unless a
    /// [`FreezeDelegate`] handles the freeze.
    pub fn with_run_store(mut self, store: Arc<dyn DurableRunStore>) -> Self {
        if !self.run_stores.iter().any(|existing| Arc::ptr_eq(existing, &store)) {
            self.run_stores.push(store);
        }
        self
    }

    /// Hands freezes and unfreezes to `delegate` (spec 3.2 §10 S4), so a freeze parks the
    /// running runs the delegate supervises instead of cancelling the run stores' runs.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let controller = GovernanceController::new(control).with_freeze_delegate(kernel);
    /// ```
    pub fn with_freeze_delegate(mut self, delegate: Arc<dyn FreezeDelegate>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// What a freeze does with running runs.
    pub fn running_runs(&self) -> RunningRuns {
        self.delegate.as_ref().map_or(RunningRuns::Cancel, |delegate| delegate.running_runs())
    }

    fn status(&self) -> GovernanceStatus {
        GovernanceStatus {
            frozen: self.control.is_frozen(),
            reason: self.control.reason(),
            mirrored: self.control.is_mirrored(),
            stale: self.control.is_stale(),
            running_runs: self.running_runs(),
            parked_runs: None,
            cancelled_runs: None,
            error: None,
        }
    }

    /// Asks every running run in the run stores to cancel and returns how many were asked.
    async fn cancel_running(&self) -> usize {
        let mut cancelled = 0;
        for store in &self.run_stores {
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
                Err(error) => {
                    tracing::error!(error = %error, "could not list running runs to cancel")
                }
            }
        }
        cancelled
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
///
/// # Example
///
/// ```rust
/// use adk_server::rest::{GovernanceStatus, RunningRuns};
///
/// let status = GovernanceStatus {
///     frozen: true,
///     reason: Some("incident 7".into()),
///     mirrored: false,
///     stale: false,
///     running_runs: RunningRuns::Park,
///     parked_runs: Some(6),
///     cancelled_runs: Some(0),
///     error: None,
/// };
/// let json = serde_json::to_value(&status).unwrap();
/// assert_eq!(json["runningRuns"], "park");
/// assert_eq!(json["parkedRuns"], 6);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// What a freeze does with running runs: `park` under a [`FreezeDelegate`], else `cancel`.
    pub running_runs: RunningRuns,
    /// Running durable runs this freeze parked, to resume on unfreeze.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parked_runs: Option<usize>,
    /// Running durable runs asked to cancel by this freeze.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_runs: Option<usize>,
    /// Why the shared store could not be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The audit actor for an administrator's request: the authenticated user, who is also
/// the issuer written to the shared store.
fn admin_actor(caller: Option<&RequestContext>) -> AuditActor {
    AuditActor {
        on_behalf_of: caller.map(|caller| SubjectRef::human(caller.user_id.clone())),
        ..AuditActor::default()
    }
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
    let actor = admin_actor(caller.as_ref());
    let mut status = controller.status();
    let written = match &controller.delegate {
        Some(delegate) => match delegate.freeze(&reason, actor).await {
            Ok(report) => {
                status.parked_runs = Some(report.parked_runs);
                status.cancelled_runs = Some(report.cancelled_runs);
                tracing::warn!(
                    runs.parked = report.parked_runs,
                    runs.cancel_requested = report.cancelled_runs,
                    "governance freeze applied by the delegate"
                );
                Ok(())
            }
            Err(error) => Err(error),
        },
        None => {
            let written = controller.control.freeze_recorded(reason, actor).await;
            let cancelled = controller.cancel_running().await;
            tracing::warn!(runs.cancel_requested = cancelled, "governance freeze applied");
            status.parked_runs = Some(0);
            status.cancelled_runs = Some(cancelled);
            written
        }
    };
    // Read again, so the status carries the state the freeze left.
    let current = controller.status();
    status.frozen = current.frozen;
    status.reason = current.reason;
    status.stale = current.stale;
    match written {
        Ok(()) => Json(status).into_response(),
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
    let actor = admin_actor(caller.as_ref());
    let lifted = match &controller.delegate {
        Some(delegate) => delegate.unfreeze(actor).await,
        None => controller.control.unfreeze_recorded(actor).await,
    };
    match lifted {
        Ok(()) => Json(controller.status()).into_response(),
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
