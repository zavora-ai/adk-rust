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
//! | [`AGENDA_PROPOSED`], [`AGENDA_APPROVED`], [`AGENDA_REJECTED`], [`AGENDA_SCHEDULED`], [`AGENDA_EXPIRED`] | `item`, `partition`, `origin`, `by`, `budget`, `reason` |
//! | [`REVIEW`] | `partition`, `reviewPeriodMs`, `rules`, `sleepers`, `frozen` (ids and ages), `ask` |
//! | [`DRIVER_MOUNTED`], [`DRIVER_UNMOUNTED`], [`DRIVER_STARTED`], [`DRIVER_CRASHED`], [`DRIVER_RESTARTED`], [`DRIVER_STALE`], [`DRIVER_REAPPROVED`] | `driverId`, `commandDigest`, `partitions`, `effectClasses`, `digests`, `attempt` |
//! | [`OBSERVATION`] | `id`, `partition`, `dimensions`, `windows` — no value that identifies content |
//! | [`CHANNEL`] | `surface`, `origin`, `channel`, `account`, `action`, `target` (spec 3.3 §11) |
//! | [`PARTITION_CREATED`], [`PARTITION_CHANGED`] | `partition`, `fields`, `from`, `to` |
//! | [`PARTITION_CROSSING`] | `partition`, `to`, `resource`, `name`, `access`, `outcome`, `run`, `tool`, `fingerprintDigest`, `reason`, `by` |
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
/// An agenda item was proposed (spec 3.2 §7.5).
pub const AGENDA_PROPOSED: &str = "kernel.agenda.proposed";
/// An agenda item was approved, by a person or an Auto partition's policy.
pub const AGENDA_APPROVED: &str = "kernel.agenda.approved";
/// An agenda item, or a proposal the policy refused, was rejected.
pub const AGENDA_REJECTED: &str = "kernel.agenda.rejected";
/// An agenda item's process was created.
pub const AGENDA_SCHEDULED: &str = "kernel.agenda.scheduled";
/// An agenda item expired before it ran.
pub const AGENDA_EXPIRED: &str = "kernel.agenda.expired";
/// The daily review of standing rules, sleepers and freezes older than the review period
/// (spec 3.2 §7.4, §9.1).
pub const REVIEW: &str = "kernel.review";
/// A driver's manifest was approved and mounted (spec 3.2 §7.7).
pub const DRIVER_MOUNTED: &str = "kernel.driver.mounted";
/// A driver was unmounted.
pub const DRIVER_UNMOUNTED: &str = "kernel.driver.unmounted";
/// A driver's server started.
pub const DRIVER_STARTED: &str = "kernel.driver.started";
/// A driver's server crashed.
pub const DRIVER_CRASHED: &str = "kernel.driver.crashed";
/// A driver's server started again after a crash or a restart.
pub const DRIVER_RESTARTED: &str = "kernel.driver.restarted";
/// A driver's tool declares something other than what was approved (K-I14).
pub const DRIVER_STALE: &str = "kernel.driver.stale";
/// A driver's changed declarations were approved and pinned.
pub const DRIVER_REAPPROVED: &str = "kernel.driver.reapproved";
/// A baseline pass recorded an observation (spec 3.2 §7.10, feature `proc-baselines`).
pub const OBSERVATION: &str = "kernel.observation";
/// Something a chat thread did or was sent (spec 3.3 §10 K1, §11): a pairing request, a
/// message, a decision card, an answer, a stop or a refused resume, with `surface: chat`,
/// `origin`, `channel`, `account`, `action` and `target` (feature `channels`).
pub const CHANNEL: &str = "kernel.channel";
/// A partition was created (spec 3.2 §7.8).
pub const PARTITION_CREATED: &str = "kernel.partition.created";
/// A partition's record changed: its mode, memory scope, drivers, crossings or blocks.
pub const PARTITION_CHANGED: &str = "kernel.partition.changed";
/// A request reached, or tried to reach, another partition (K-I15).
pub const PARTITION_CROSSING: &str = "kernel.partition.crossing";
