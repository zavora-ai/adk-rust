//! # adk-kernel
//!
//! The Agent Kernel of ADK-Rust, the always-on form of the harness (spec 3.2,
//! `docs/design/spec-3.2-agent-kernel.md`): a resident supervisor with a process model,
//! wake semantics, an event bus and a control plane, composed from the durable-run
//! machinery `adk-core` and `adk-server` already hold.
//!
//! Three rules shape it (§1):
//!
//! - **Always resumable, not always running.** A process is a durable run; asleep is
//!   `Waiting` with a recorded [`WakeSet`] and no lease, so a sleeping agent makes no model
//!   call (K-I2).
//! - **Every wake is a governed run.** A wake produces a `NewRun` or wakes a parked run;
//!   nothing calls a tool outside a run (K-I1), so policy, guardrails, confirmation, the
//!   action ledger and audit apply as for an interactive turn.
//! - **The kernel owns state, surfaces attach.** CLI, web and phone are terminals on one
//!   running system over the [`control`] plane (K-I17).
//!
//! `adk-kernel` hosts any `Arc<Runner>` and depends on no harness type (§6 D-1); the
//! `kernel` feature of `adk-harness` carries the glue (§10 HK1), and `adk-rust up` starts
//! it.
//!
//! | Item | Spec |
//! |---|---|
//! | [`Kernel`], [`KernelBuilder`], [`KernelConfig`], [`KernelHealth`], [`KernelState`] | §7.2 |
//! | [`Process`], [`ProcessMeta`], [`ProcessView`], [`GoalProgress`], [`ProcessKind`], [`Suspension`], [`ProcessTable`], [`ProcessSpec`], [`ProgressSource`] | §7.3 |
//! | [`ProcessStore`], [`ProcessFilter`], [`SpendScope`], [`InMemoryProcessStore`], `SqliteProcessStore`, `PostgresProcessStore`, `RedisProcessStore` | §7.3, §13 Q8 |
//! | [`Wake`], [`WakeSet`], [`ExpiryAction`], [`WakeSource`], [`WakeEvent`], [`keys`], [`Trigger`], [`WakeRouter`], [`WakeOutcome`], [`KernelWakeExecutor`], the wake source adapters | §7.4 |
//! | [`KernelEvent`], [`EventRecord`], [`EventBus`], [`EventSubscriber`], [`EventSink`], [`AuditSink`], [`SpendDelta`] | §7.6 |
//! | [`Agenda`], [`AgendaHandle`], [`ProposalOutcome`], [`AgendaItem`], [`AgendaState`], [`AgendaStateKind`], [`AgendaOrigin`], [`AgendaAnswer`], [`ObservationAudience`], [`PlanRef`], [`AgendaStore`], [`AgendaFilter`], [`InMemoryAgendaStore`], [`ProactivityPolicy`], [`ProactiveApproval`] | §7.5 |
//! | [`ConsolidationTick`], [`Consolidator`], [`admit_tick`], [`TickInputs`], [`TickAdmission`], [`TickMaterial`], [`TickRun`], [`TickOutcome`], [`TickProposal`], [`TickSpend`], [`CONSOLIDATION_TARGET`], [`CONSOLIDATION_LABEL`] | §7.5, §13 Q1, Q12 |
//! | [`ReviewReport`], [`StandingRuleSource`], [`RuleAge`], [`StaleRule`], [`AgedProcess`] | §7.4, §9.1 |
//! | [`QueueClass`], [`QuietHours`], [`Quotas`] | §7.9 |
//! | [`drivers`]: `DriverManifest`, [`EffectClass`](drivers::EffectClass), `DriverStore`, `DriverSupervisor`, `DriverPolicy` (feature `drivers`) | §7.7 |
//! | `proc` (feature `proc`): `Ledger`, `LedgerEntry`, `LedgerStore` on every backend, `LedgerSink`, `LedgerAuditTap`; `proc::baselines` (feature `proc-baselines`): `BaselinePass`, `BaselinePolicy`, `ObservationPhraser`, `Observation` | §7.10 |
//! | [`PartitionId`], [`PartitionMode`] | §7.8 (the phase-1 part) |
//! | [`control`]: [`ControlPlaneConfig`](control::ControlPlaneConfig), [`router`](control::router), [`ControlPlane`](control::ControlPlane), [`ControlClient`](control::ControlClient), [`api`](control::api) | §7.11 |
//! | [`audit`] action names | §9.3 |
//! | `service` (feature `service`): `ServiceDefinition`, `install`, `uninstall`, `run_as_service` (Windows) | §7.12, §10 W3 |
//! | `conformance` (feature `conformance`) | §11 |
//! | `managed` (feature `managed`): `KernelDef`, `TenantKernel`, `KernelResolver`, `ManagedKernels` | §7.13, §10 M1 |
//!
//! Agenda and proactivity (§7.5) and attention and resources (§7.9) are phase 2; drivers
//! (§7.7) and partitions (§7.8) phase 3; channel wakes phase 5. Each adds to these types
//! without changing them.
//!
//! # Features
//!
//! | Feature | Default | Pulls in |
//! |---|---|---|
//! | `sqlite` | on | `SqliteProcessStore`, on the pool of `harness.db` |
//! | `postgres`, `redis` | off | `PostgresProcessStore`, `RedisProcessStore`, opened from the URL of `adk-rust serve --store` |
//! | `webhooks` | on | `webhook_wake_hook` over `adk-server`'s `WebhookIngress` |
//! | `drivers` | on | [`drivers`]: manifests, the driver store, the supervisor over `adk-tool`'s MCP manager, the stdio launcher on `adk-sandbox` |
//! | `service` | on | The `service` module: `adk-rust up` as a launchd agent, a systemd user unit or a Windows service (`windows-service` on Windows) |
//! | `proc` | on | The content-free activity ledger (`proc`), its stores and `GET /api/kernel/proc/*`; the OTel GenAI histograms of the live aggregates |
//! | `proc-baselines` | off | The daily baseline pass and observations over `adk-eval`'s ledger statistics (implies `proc`; the only `adk-eval` edge) |
//! | `channels` | off | Empty until phase 5 |
//! | `managed` | off | The managed form: `KernelDef` in a managed definition, one kernel per tenant (`adk-managed`) |
//! | `conformance` | off | The `conformance` module |
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_core::InMemoryRunStore;
//! use adk_kernel::{Kernel, KernelConfig, ProcessFilter};
//!
//! # async fn example(runner: Arc<adk_runner::Runner>) -> adk_kernel::Result<()> {
//! let kernel = Kernel::builder()
//!     .config(KernelConfig::default())
//!     .run_store(Arc::new(InMemoryRunStore::new()))
//!     .agent("adk-rust", runner)
//!     .build()?;
//! kernel.start().await?;
//! for process in kernel.processes().ps(ProcessFilter::default()).await? {
//!     println!("{} {:?} {}", process.meta.pid, process.run.state, process.meta.kind);
//! }
//! kernel.shutdown_on_signal().await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

pub mod agenda;
pub mod audit;
pub mod bus;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod consolidation;
pub mod control;
pub mod drivers;
pub mod error;
pub mod kernel;
#[cfg(feature = "managed")]
pub mod managed;
pub mod partition;
#[cfg(feature = "proc")]
pub mod proc;
pub mod process;
pub mod review;
pub mod sched;
mod serde_util;
#[cfg(feature = "service")]
pub mod service;
pub mod stores;
mod supervise;
pub mod wake;
#[cfg(windows)]
mod win;

pub use agenda::{
    AGENDA_TOOL_NAME, Agenda, AgendaAnswer, AgendaFilter, AgendaHandle, AgendaItem, AgendaOrigin,
    AgendaState, AgendaStateKind, AgendaStore, ObservationAudience, PlanRef, ProactiveApproval,
    ProactivityPolicy, ProposalOutcome,
};
pub use bus::{
    AuditSink, DEFAULT_EVENT_CAPACITY, EventBus, EventRecord, EventSink, EventSubscriber,
    KernelEvent, SpendDelta,
};
pub use consolidation::{
    CONSOLIDATION_LABEL, CONSOLIDATION_TARGET, ConsolidationTick, Consolidator, TickAdmission,
    TickInputs, TickMaterial, TickOutcome, TickProposal, TickRun, TickSpend, admit_tick,
};
pub use error::{KernelError, Result};
pub use kernel::{Kernel, KernelBuilder, KernelConfig, KernelHealth, KernelState, WorkerHealth};
pub use partition::{PartitionId, PartitionMode};
pub use process::{
    GoalProgress, Process, ProcessHandle, ProcessKind, ProcessMeta, ProcessSpec, ProcessTable,
    ProcessView, ProgressSource, RunUsage, Suspension,
};
pub use review::{AgedProcess, ReviewReport, RuleAge, StaleRule, StandingRuleSource};
pub use sched::{QueueClass, QuietHours, Quotas};
#[cfg(feature = "postgres")]
pub use stores::PostgresAgendaStore;
#[cfg(feature = "postgres")]
pub use stores::PostgresProcessStore;
#[cfg(feature = "redis")]
pub use stores::RedisAgendaStore;
#[cfg(feature = "redis")]
pub use stores::RedisProcessStore;
#[cfg(feature = "sqlite")]
pub use stores::SqliteAgendaStore;
#[cfg(feature = "sqlite")]
pub use stores::SqliteProcessStore;
pub use stores::{
    InMemoryAgendaStore, InMemoryProcessStore, ProcessFilter, ProcessStore, SpendScope,
};
#[cfg(feature = "webhooks")]
pub use wake::webhook_wake_hook;
pub use wake::{
    ApprovalWakeSource, CronWakeSource, ExpiryAction, FileWakeSource, KERNEL_WAKE_TARGET,
    KernelWakeExecutor, RunFinishedWakeSource, Trigger, Wake, WakeEvent, WakeOutcome, WakeRouter,
    WakeSet, WakeSource, kernel_wake_run, keys, queue_wake_ingress,
};

/// A short, stable tag of `path`: sixteen hex digits of the SHA-256 of its absolute form,
/// lowercased on Windows, whose paths ignore case. It names a kernel home's pipe and service
/// (spec 3.2 §10 W2, §7.12), so a client and the kernel derive the same name from one home.
pub(crate) fn path_tag(path: &std::path::Path) -> String {
    use sha2::Digest as _;

    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy();
    let text = if cfg!(windows) { text.to_lowercase() } else { text.into_owned() };
    sha2::Sha256::digest(text.as_bytes())[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}
