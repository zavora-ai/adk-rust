//! What a kernel runs, and what it would silently not do (spec 3.2 §7.1, §7.4; #1278):
//! [`Kernel::inventory`](crate::Kernel::inventory), logged when the kernel starts and served at
//! `GET /api/kernel/inventory`.

use serde::{Deserialize, Serialize};

use crate::partition::PartitionId;
use crate::stores::StoreUse;
use crate::wake::Wake;

/// What a kernel runs: its targets, standing triggers, wake sources, the partitions it files
/// signals in and its stores, with a warning for each thing it would silently not do.
///
/// | Warning | When |
/// |---|---|
/// | [`TRIGGER_UNSERVED`](InventoryWarning::TRIGGER_UNSERVED) | A trigger's run names a target no agent or workflow of the kernel serves |
/// | [`SIGNAL_UNFILED`](InventoryWarning::SIGNAL_UNFILED) | A trigger wakes on a signal in a partition the kernel files no signals in (`KernelBuilder::signals`) |
/// | [`STORE_IN_MEMORY`](InventoryWarning::STORE_IN_MEMORY) | A stateful store is held in memory: one per store the builder defaulted, one per bundle that keeps stores in memory |
/// | [`KERNEL_IDLE`](InventoryWarning::KERNEL_IDLE) | The kernel has no agent, trigger, workflow, schedule store or agent factory |
/// | any other code | A warning the host or a kit added (`KernelBuilder::inventory_warning`), such as a job it does not schedule |
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::Kernel;
/// use adk_kernel::inventory::InventoryWarning;
///
/// let kernel = Kernel::builder().run_store(Arc::new(InMemoryRunStore::new())).build()?;
/// let inventory = kernel.inventory();
/// assert!(inventory.warnings.iter().any(|warning| warning.code == InventoryWarning::KERNEL_IDLE));
/// # Ok::<(), adk_kernel::KernelError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KernelInventory {
    /// The app the kernel's runs belong to.
    pub app_name: String,
    /// The agent targets, sorted: those given to the builder and the agents a person added.
    /// Workflow ids cannot be listed.
    pub targets: Vec<String>,
    /// The standing triggers, in the order they were added.
    pub triggers: Vec<InventoryTrigger>,
    /// The wake sources' names, the kernel's own included.
    pub wake_sources: Vec<String>,
    /// Whether the kernel's scheduler fires a schedule store.
    pub schedules: bool,
    /// The partitions the kernel files signals in (`KernelBuilder::signals`).
    pub signal_partitions: Vec<PartitionId>,
    /// Each stateful store: where it came from and whether it is held in memory.
    pub stores: Vec<StoreUse>,
    /// What the kernel would silently not do.
    pub warnings: Vec<InventoryWarning>,
}

/// One standing trigger of a [`KernelInventory`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct InventoryTrigger {
    /// The trigger's name.
    pub name: String,
    /// What fires it.
    pub wake: Wake,
    /// The target its run names.
    pub target: String,
    /// The queue its run waits on.
    pub queue: String,
    /// The partition its processes run in.
    pub partition: PartitionId,
    /// Whether an agent or workflow of the kernel serves the target.
    pub served: bool,
}

/// Something a kernel would silently not do, in a [`KernelInventory`].
///
/// # Example
///
/// ```rust
/// use adk_kernel::inventory::InventoryWarning;
///
/// let warning = InventoryWarning::new("site.job_unscheduled", "the weekly digest is not scheduled: the owner has no channel")
///     .with_subject("site.digest");
/// assert_eq!(serde_json::to_value(&warning)?["subject"], "site.digest");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct InventoryWarning {
    /// A stable code, such as [`TRIGGER_UNSERVED`](Self::TRIGGER_UNSERVED).
    pub code: String,
    /// What the warning concerns: a trigger, a store or a job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// What happens, and how to fix it.
    pub message: String,
}

impl InventoryWarning {
    /// A trigger's run names a target no agent or workflow of the kernel serves.
    pub const TRIGGER_UNSERVED: &'static str = "kernel.trigger.unserved";
    /// A trigger wakes on a signal in a partition the kernel files no signals in.
    pub const SIGNAL_UNFILED: &'static str = "kernel.trigger.signal_unfiled";
    /// A stateful store is held in memory, so its state ends with the process.
    pub const STORE_IN_MEMORY: &'static str = "kernel.store.in_memory";
    /// The kernel has nothing to run.
    pub const KERNEL_IDLE: &'static str = "kernel.idle";

    /// A warning of `code` saying `message`.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), subject: None, message: message.into() }
    }

    /// Names what the warning concerns.
    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }
}
