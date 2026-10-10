//! Admin endpoints for the organisation kill switch.
//!
//! Mounted by [`ServerBuilder::enable_governance_endpoints`](crate::ServerBuilder::enable_governance_endpoints)
//! behind the server's auth middleware:
//!
//! - `POST /api/admin/freeze` — freezes every runner the server builds and pauses background
//!   and cron scheduling.
//! - `POST /api/admin/unfreeze` — lifts both.
//! - `GET /api/admin/governance` — reports the current state.

use adk_core::{GovernanceControl, RequestContext};
use axum::{Extension, Json, extract::State};
use serde::{Deserialize, Serialize};

/// State for the governance admin endpoints.
#[derive(Clone)]
pub struct GovernanceController {
    control: GovernanceControl,
    #[cfg(feature = "background")]
    background: Vec<crate::background::BackgroundRunner>,
    #[cfg(feature = "background")]
    cron: Option<crate::background::cron::CronJobStore>,
}

impl GovernanceController {
    /// Endpoints over `control`.
    pub fn new(control: GovernanceControl) -> Self {
        Self {
            control,
            #[cfg(feature = "background")]
            background: Vec::new(),
            #[cfg(feature = "background")]
            cron: None,
        }
    }

    /// Also pauses `runner` while frozen.
    #[cfg(feature = "background")]
    pub fn with_background_runner(mut self, runner: crate::background::BackgroundRunner) -> Self {
        self.background.push(runner);
        self
    }

    /// Also pauses cron scheduling in `store` while frozen.
    #[cfg(feature = "background")]
    pub fn with_cron_store(mut self, store: crate::background::cron::CronJobStore) -> Self {
        self.cron = Some(store);
        self
    }

    fn status(&self) -> GovernanceStatus {
        GovernanceStatus { frozen: self.control.is_frozen(), reason: self.control.reason() }
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
}

/// `POST /api/admin/freeze`
pub async fn freeze(
    State(controller): State<GovernanceController>,
    Extension(caller): Extension<Option<RequestContext>>,
    Json(request): Json<FreezeRequest>,
) -> Json<GovernanceStatus> {
    let reason = request.reason.unwrap_or_else(|| "frozen by an administrator".to_string());
    tracing::warn!(
        caller.user_id = caller.as_ref().map(|c| c.user_id.as_str()).unwrap_or("anonymous"),
        governance.reason = %reason,
        "governance freeze requested"
    );
    controller.control.freeze(reason);
    #[cfg(feature = "background")]
    {
        controller.background.iter().for_each(crate::background::BackgroundRunner::pause);
        if let Some(cron) = &controller.cron {
            cron.pause_scheduling();
        }
    }
    Json(controller.status())
}

/// `POST /api/admin/unfreeze`
pub async fn unfreeze(
    State(controller): State<GovernanceController>,
    Extension(caller): Extension<Option<RequestContext>>,
) -> Json<GovernanceStatus> {
    tracing::warn!(
        caller.user_id = caller.as_ref().map(|c| c.user_id.as_str()).unwrap_or("anonymous"),
        "governance unfreeze requested"
    );
    controller.control.unfreeze();
    #[cfg(feature = "background")]
    {
        controller.background.iter().for_each(crate::background::BackgroundRunner::resume);
        if let Some(cron) = &controller.cron {
            cron.resume_scheduling();
        }
    }
    Json(controller.status())
}

/// `GET /api/admin/governance`
pub async fn status(State(controller): State<GovernanceController>) -> Json<GovernanceStatus> {
    Json(controller.status())
}
