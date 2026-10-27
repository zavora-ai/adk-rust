//! Every call the inbox holds, announced on the kernel's event bus (spec 3.3 §12 phase 5: "the
//! tray changes within one event").
//!
//! A call a kernel process holds moves its process to `waiting`, which the bus reports; a call
//! held outside a process, such as a voice conversation's, moved nothing the bus saw, so the
//! tray and the terminal attached to the kernel found it only on their next timed read. This
//! doorbell publishes `KernelEvent::Held` for each request the inbox takes, with the call's
//! redacted arguments, so every surface following `/api/kernel/events` re-reads on it.

use std::fmt;
use std::sync::{Arc, OnceLock};

use adk_core::{
    ApprovalEventKind, ApprovalNotification, ApprovalNotifier, ToolConfirmationRequest,
};
use adk_kernel::{Kernel, KernelEvent};
use async_trait::async_trait;

/// Publishes `KernelEvent::Held` on the kernel's bus once the kernel is built
/// ([`HeldDoorbell::attach`]); before that, it rings no one.
#[derive(Clone, Default)]
pub(super) struct HeldDoorbell(Arc<OnceLock<Kernel>>);

impl fmt::Debug for HeldDoorbell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeldDoorbell").field("attached", &self.0.get().is_some()).finish()
    }
}

impl HeldDoorbell {
    /// Rings `kernel`'s bus from now on.
    pub(super) fn attach(&self, kernel: &Kernel) {
        let _ = self.0.set(kernel.clone());
    }
}

#[async_trait]
impl ApprovalNotifier for HeldDoorbell {
    async fn notify(&self, notification: &ApprovalNotification) -> adk_core::Result<()> {
        if !matches!(notification.kind, ApprovalEventKind::Submitted) {
            return Ok(());
        }
        let Some(kernel) = self.0.get() else { return Ok(()) };
        let request = &notification.request;
        let mut call =
            ToolConfirmationRequest::new(request.tool_name.clone(), request.args_redacted.clone());
        call.origin = request.origin.clone();
        call.acting = request.acting.clone().map(Box::new);
        kernel.bus().publish(KernelEvent::Held {
            pid: request.run_id.clone().unwrap_or_default(),
            request: call,
        });
        Ok(())
    }
}
