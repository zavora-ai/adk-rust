//! Audit actions of the kernel (spec 3.2 §9.3).
//!
//! The kernel writes these through the signed audit log (spec 3.1 §9.3) as kind
//! `Other(`[`KERNEL_AUDIT_KIND`]`)`, with the actor taken from the control-plane subject or
//! the process's acting context.
//! `governance.freeze` and `governance.unfreeze` are written by
//! `GovernanceControl::freeze_recorded` and `unfreeze_recorded`; a freeze adds one
//! [`PROCESS_SUSPENDED`] record per process it parks, with `suspendedBy: freeze`.
//! `harness.goal.stalled` is the harness's own record (§10 HK3).
//!
//! | Action | Fields |
//! |---|---|
//! | [`KERNEL_STARTED`], [`KERNEL_PAUSED`], [`KERNEL_RESUMED`], [`KERNEL_DRAINED`], [`KERNEL_STOPPED`] | `kernelId`, `home`, `replicas`, `reason` |
//! | [`PROCESS_SPAWNED`], [`PROCESS_WOKEN`], [`PROCESS_SUSPENDED`], [`PROCESS_RESUMED`], [`PROCESS_KILLED`], [`PROCESS_REQUEUED`] | `pid`, `parent`, `partition`, `queue`, `kind`, `wake`, `reason`, `by` |
//! | [`WAKE`] | `source`, `key`, `matchedPid` or `newPid`, `deduplicated` |
//! | [`CONTROL`] | `subject`, `transport`, `action`, `target` |
//!
//! The agenda, driver, partition and observation actions of §9.3 arrive with their phases.
//!
//! # Example
//!
//! ```rust
//! assert_eq!(adk_kernel::audit::PROCESS_SUSPENDED, "kernel.process.suspended");
//! ```

/// The kind of every kernel audit record, as `AuditRecordKind::Other(KERNEL_AUDIT_KIND.into())`
/// (spec 3.2 §9.3).
pub const KERNEL_AUDIT_KIND: &str = "kernel";

/// The kernel started its workers, scheduler, wake sources and control plane.
pub const KERNEL_STARTED: &str = "kernel.started";
/// The kernel stopped claiming new work.
pub const KERNEL_PAUSED: &str = "kernel.paused";
/// The kernel resumed claiming after a pause.
pub const KERNEL_RESUMED: &str = "kernel.resumed";
/// The kernel suspended its running processes at their next checkpoint and stopped claiming.
pub const KERNEL_DRAINED: &str = "kernel.drained";
/// The kernel stopped and closed its control plane.
pub const KERNEL_STOPPED: &str = "kernel.stopped";
/// A process was created.
pub const PROCESS_SPAWNED: &str = "kernel.process.spawned";
/// A sleeping process was woken.
pub const PROCESS_WOKEN: &str = "kernel.process.woken";
/// A process was parked `Waiting`; `suspendedBy` says why.
pub const PROCESS_SUSPENDED: &str = "kernel.process.suspended";
/// A suspended process was resumed.
pub const PROCESS_RESUMED: &str = "kernel.process.resumed";
/// A process was killed.
pub const PROCESS_KILLED: &str = "kernel.process.killed";
/// A process moved to another queue.
pub const PROCESS_REQUEUED: &str = "kernel.process.requeued";
/// A wake event arrived and woke, spawned or deduplicated a process.
pub const WAKE: &str = "kernel.wake";
/// A control-plane action.
pub const CONTROL: &str = "kernel.control";
