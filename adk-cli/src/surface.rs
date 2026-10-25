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
    phone: bool,
) -> Result<SurfaceEngine> {
    let mut source = LocalSource::builder(app_name).sessions(sessions).harness(harness);
    if let Some(approvals) = approvals {
        source = source.approvals(approvals);
    }
    if let Some(kernel) = kernel {
        source = source.kernel(kernel, app_name);
    }
    // Decisions also wait in the terminal: `adk-rust approvals`, `attach`. Stop all and Resume
    // all freeze and unfreeze every run the server makes, as the kernel's own freeze route
    // does, which needs the operator too (spec 3.3 §11 S-I10).
    let mut engine = SurfaceEngine::builder(Arc::new(source.build()?))
        .places(
            [adk_surface::Place::terminal()]
                .into_iter()
                .chain(phone.then(adk_surface::Place::phone)),
        )
        .agent_name(app_name, "ADK-Rust")
        .stops_need(crate::harness::OPERATOR_SCOPE);
    if let Some(records) = records {
        engine = engine.audit(records.trail).answers(records.answers);
    }
    Ok(engine.build())
}
