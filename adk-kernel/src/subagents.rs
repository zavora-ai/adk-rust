//! Background sub-agents as kernel processes.
//!
//! [`KernelBuilder::subagents`](crate::KernelBuilder::subagents) turns them on: the kernel
//! hosts a [`DurableSubagents`] whose runs are processes of its `background` queue, and
//! [`Kernel::subagents`](crate::Kernel::subagents) returns it.
//!
//! | Concern | In a kernel |
//! |---|---|
//! | A sub-agent's run | A process of kind [`ProcessKind::Subagent`], spawned through the process table: a freeze refuses it, its partition's quotas and spend cap admit it, `ps` and `top` list it with its parent |
//! | The run that tells the parent | A process of kind `triggered:subagent` whose parent is the sub-agent's run |
//! | Steering | [`Kernel::steer`](crate::Kernel::steer) on the replica that runs the turn |
//! | Cancel | [`ProcessTable::kill`](crate::ProcessTable::kill) |
//! | Status | [`KernelEvent::Subagent`] on the bus at every change; a `WebhookSink` filtered to [`EventKind::Subagent`](crate::EventKind::Subagent) delivers each as a signed webhook |
//! | Restart | [`Kernel::start`](crate::Kernel::start) runs [`DurableSubagents::reconcile`], so a parent is told about a sub-agent that finished while the kernel was killed |
//!
//! A tool built before the kernel reaches it through a [`SubagentHandle`], bound by
//! [`KernelBuilder::subagent_handle`](crate::KernelBuilder::subagent_handle).

use std::sync::{Arc, OnceLock, RwLock, Weak};

use adk_core::{
    FollowUp, NewRun, Result, RunRecord, SteerReceipt, SubagentHost, SubagentLaunch,
    SubagentRecord, SubagentScope, SubagentStart, SubagentTarget,
};
use adk_server::background::{DurableSubagents, SubagentBackend, WeakSubagents};
use async_trait::async_trait;

use crate::bus::KernelEvent;
use crate::kernel::{Kernel, KernelInner};
use crate::process::{ProcessKind, ProcessSpec, ProcessTable};
use crate::supervise::Supervision;

/// The label of a sub-agent's processes that holds its id.
pub const SUBAGENT_LABEL: &str = "subagent";

/// The label of a sub-agent's processes that holds its name.
pub const SUBAGENT_NAME_LABEL: &str = "subagent.name";

/// The source of the processes that tell a parent about its sub-agent:
/// `triggered:subagent`.
pub const SUBAGENT_NOTICE_SOURCE: &str = "subagent";

/// The kernel's [`SubagentBackend`]: the process table, the kernel's steering and its bus.
pub(crate) struct KernelSubagentBackend {
    pub(crate) processes: ProcessTable,
    pub(crate) sup: Arc<Supervision>,
    pub(crate) kernel: OnceLock<Weak<KernelInner>>,
}

impl KernelSubagentBackend {
    fn spec(launch: &SubagentLaunch, kind: ProcessKind) -> ProcessSpec {
        let spec = ProcessSpec::new(kind)
            .with_label(SUBAGENT_LABEL, launch.id.clone())
            .with_label(SUBAGENT_NAME_LABEL, launch.name.clone());
        match &launch.parent.run_id {
            Some(parent) => spec.with_parent(parent.clone()),
            None => spec,
        }
    }
}

#[async_trait]
impl SubagentBackend for KernelSubagentBackend {
    async fn enqueue_subagent(&self, run: NewRun, launch: &SubagentLaunch) -> Result<RunRecord> {
        let spec = Self::spec(launch, ProcessKind::Subagent);
        Ok(self.processes.spawn(run, spec).await?.run)
    }

    async fn enqueue_notice(
        &self,
        run: NewRun,
        child: &RunRecord,
        launch: &SubagentLaunch,
    ) -> Result<RunRecord> {
        let spec =
            ProcessSpec::new(ProcessKind::Triggered { source: SUBAGENT_NOTICE_SOURCE.to_string() })
                .with_parent(child.run_id.clone())
                .with_label(SUBAGENT_LABEL, launch.id.clone());
        Ok(self.processes.spawn(run, spec).await?.run)
    }

    async fn steer(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        text: &str,
    ) -> Result<Option<SteerReceipt>> {
        if app_name != self.sup.app_name {
            return Ok(None);
        }
        let Some(kernel) = self.kernel.get().and_then(Weak::upgrade).map(Kernel::from_inner) else {
            return Ok(None);
        };
        Ok(kernel.steer(user_id, session_id, text).await?.map(|steering| steering.receipt))
    }

    async fn cancel(&self, run_id: &str) -> Result<()> {
        match self.processes.kill(run_id, "cancelled by the agent that started it").await {
            Ok(()) => Ok(()),
            // Finished meanwhile: nothing to cancel.
            Err(crate::KernelError::InvalidState { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn changed(&self, record: &SubagentRecord) {
        self.sup.publish(KernelEvent::Subagent {
            pid: record.run_id.clone(),
            id: record.id.clone(),
            name: record.name.clone(),
            parent: record.parent.run_id.clone(),
            status: record.status,
        });
    }
}

/// A handle on a kernel's sub-agent host that does not keep the kernel alive, bound when
/// the kernel is built with it: an agent's sub-agent tools exist before the kernel that
/// hosts the agent. Until bound, and after the kernel is gone, every operation fails with
/// `subagent.unavailable`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_agent::subagents::SubagentTools;
/// use adk_kernel::SubagentHandle;
///
/// let handle = SubagentHandle::new();
/// let tools = SubagentTools::new(Arc::new(handle.clone())).tools();
/// assert_eq!(tools.len(), 4);
/// // `Kernel::builder().subagents(SubagentConfig::default()).subagent_handle(handle)` binds it.
/// ```
#[derive(Clone, Default)]
pub struct SubagentHandle {
    slot: Arc<RwLock<WeakSubagents>>,
    targets: Arc<RwLock<Vec<SubagentTarget>>>,
}

impl std::fmt::Debug for SubagentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentHandle").field("bound", &self.get().is_some()).finish()
    }
}

impl SubagentHandle {
    /// An unbound handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Names the targets the bound host will run, so a `start_subagent` tool built before
    /// the kernel describes them to the model; binding replaces them with the host's.
    #[must_use]
    pub fn with_targets(self, targets: impl IntoIterator<Item = SubagentTarget>) -> Self {
        *self.targets.write().unwrap_or_else(|e| e.into_inner()) = targets.into_iter().collect();
        self
    }

    /// The host of the kernel last built with this handle, while that kernel lives.
    pub fn get(&self) -> Option<DurableSubagents> {
        self.slot.read().unwrap_or_else(|e| e.into_inner()).upgrade()
    }

    pub(crate) fn bind(&self, host: &DurableSubagents) {
        *self.slot.write().unwrap_or_else(|e| e.into_inner()) = host.downgrade();
        *self.targets.write().unwrap_or_else(|e| e.into_inner()) = host.targets();
    }

    fn host(&self) -> Result<DurableSubagents> {
        self.get().ok_or_else(|| {
            adk_core::AdkError::new(
                adk_core::ErrorComponent::Agent,
                adk_core::ErrorCategory::Unavailable,
                "subagent.unavailable",
                "background sub-agents need the agent to run in a kernel built with \
                 KernelBuilder::subagents and this handle; none is running",
            )
        })
    }
}

#[async_trait]
impl SubagentHost for SubagentHandle {
    async fn start(&self, start: SubagentStart) -> Result<SubagentRecord> {
        self.host()?.start(start).await
    }

    async fn get(&self, scope: &SubagentScope, id: &str) -> Result<Option<SubagentRecord>> {
        SubagentHost::get(&self.host()?, scope, id).await
    }

    async fn list(&self, scope: &SubagentScope) -> Result<Vec<SubagentRecord>> {
        self.host()?.list(scope).await
    }

    async fn send(&self, scope: &SubagentScope, id: &str, message: &str) -> Result<FollowUp> {
        self.host()?.send(scope, id, message).await
    }

    async fn cancel(&self, scope: &SubagentScope, id: &str) -> Result<SubagentRecord> {
        self.host()?.cancel(scope, id).await
    }

    fn targets(&self) -> Vec<SubagentTarget> {
        self.get()
            .map(|host| host.targets())
            .unwrap_or_else(|| self.targets.read().unwrap_or_else(|e| e.into_inner()).clone())
    }
}
