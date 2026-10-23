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
//! | [`Agenda`], [`AgendaHandle`], [`ProposalOutcome`], [`AgendaItem`], [`AgendaState`], [`AgendaStateKind`], [`AgendaOrigin`], [`PlanRef`], [`AgendaStore`], [`AgendaFilter`], [`InMemoryAgendaStore`], [`ProactivityPolicy`], [`ProactiveApproval`] | §7.5 |
//! | [`ConsolidationTick`] | §7.5, §13 Q1, Q12 |
//! | [`QueueClass`], [`QuietHours`], [`Quotas`] | §7.9 |
//! | [`PartitionId`], [`PartitionMode`] | §7.8 (the phase-1 part) |
//! | [`control`]: [`ControlPlaneConfig`](control::ControlPlaneConfig), [`router`](control::router), [`ControlPlane`](control::ControlPlane), [`ControlClient`](control::ControlClient), [`api`](control::api) | §7.11 |
//! | [`audit`] action names | §9.3 |
//! | `conformance` (feature `conformance`) | §11 |
//!
//! Agenda and proactivity (§7.5) and attention and resources (§7.9) are phase 2; drivers
//! (§7.7) and partitions (§7.8) phase 3;
//! `/proc` baselines (§7.10) and service installation (§7.12) phase 4; the managed form
//! (§7.13) and channel wakes phase 5. Each adds to these types without changing them.
//!
//! # Features
//!
//! | Feature | Default | Pulls in |
//! |---|---|---|
//! | `sqlite` | on | `SqliteProcessStore`, on the pool of `harness.db` |
//! | `postgres`, `redis` | off | `PostgresProcessStore`, `RedisProcessStore`, opened from the URL of `adk-rust serve --store` |
//! | `webhooks` | on | `webhook_wake_hook` over `adk-server`'s `WebhookIngress` |
//! | `drivers` | on | Empty until phase 3 |
//! | `service` | on | Empty until phase 4 |
//! | `proc` | on | The OTel GenAI histograms of the live aggregates; the ledger in phase 4 |
//! | `proc-baselines` | off | Empty until phase 4 (implies `proc`) |
//! | `channels`, `managed` | off | Empty until phase 5 |
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
pub mod error;
pub mod kernel;
pub mod partition;
pub mod process;
pub mod sched;
mod serde_util;
pub mod stores;
mod supervise;
pub mod wake;

pub use agenda::{
    AGENDA_TOOL_NAME, Agenda, AgendaFilter, AgendaHandle, AgendaItem, AgendaOrigin, AgendaState,
    AgendaStateKind, AgendaStore, PlanRef, ProactiveApproval, ProactivityPolicy, ProposalOutcome,
};
pub use bus::{
    AuditSink, DEFAULT_EVENT_CAPACITY, EventBus, EventRecord, EventSink, EventSubscriber,
    KernelEvent, SpendDelta,
};
pub use consolidation::ConsolidationTick;
pub use error::{KernelError, Result};
pub use kernel::{Kernel, KernelBuilder, KernelConfig, KernelHealth, KernelState, WorkerHealth};
pub use partition::{PartitionId, PartitionMode};
pub use process::{
    GoalProgress, Process, ProcessHandle, ProcessKind, ProcessMeta, ProcessSpec, ProcessTable,
    ProcessView, ProgressSource, RunUsage, Suspension,
};
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
