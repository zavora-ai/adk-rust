//! Snapshots and rewind (spec 3.1 §7.27, amending spec 3.3 §7.10; invariant I68).
//!
//! Before each user turn, before each plan-graph step, before a `Destructive` call and
//! before a rewind, the harness snapshots the whole tree under the workspace root, so what a
//! command wrote is covered like what a file tool wrote. A rewind restores files, the
//! transcript and the plan from one of those snapshots, and `/redo` restores the snapshot
//! taken before the rewind.
//!
//! | Item | Role |
//! |---|---|
//! | [`SnapshotConfig`] | `keep_turns` (50), `max_bytes` (2 GiB per workspace), `max_files` (50,000), `max_file_bytes` (8 MiB), `skip_dirs` |
//! | [`walk`], [`WalkOptions`] | The breadth-first walker ultracode shares: `.gitignore` and `Workspace::exclude_globs` honoured, `.git` never opened, build directories skipped |
//! | [`Snapshots`] | The stat cache (size, mtime, inode), the object store, the record store, retention and garbage collection; [`Snapshots::take`] is "snapshot now" |
//! | [`SnapshotObjects`]: [`FileSnapshotObjects`], [`InMemorySnapshotObjects`] | Content-addressed objects, beside `harness.db`, sealed under the store key when one is configured |
//! | [`SnapshotStore`]: [`InMemorySnapshotStore`], `SqliteSnapshotStore` (feature `sqlite`) | One record per snapshot |
//! | [`RewindTarget`], [`RewindPlan`], [`RewindReport`] | What `Harness::rewind_plan` shows and `Harness::rewind` does |
//!
//! Snapshots are off in the library until [`HarnessAgentBuilder::snapshots`](crate::HarnessAgentBuilder::snapshots)
//! is set, and on in adk-cli (spec 3.1 §13 Q11).
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_harness::snapshot::{RewindTarget, SnapshotConfig};
//!
//! let harness = HarnessAgent::builder("harness")
//!     .model(model)
//!     .workspace(workspace)
//!     .snapshots(SnapshotConfig::default())
//!     .build()?
//!     .into_harness("my-app", sessions)?;
//! // ... turns run ...
//! let mut plan = harness.rewind_plan(&user, &session, RewindTarget::Turn(1)).await?;
//! println!("{}", plan.render());
//! plan.confirm_protected();
//! let report = harness.rewind(&user, &session, &plan).await?;
//! ```

mod objects;
mod plan;
#[cfg(feature = "runner")]
mod rewind;
mod runtime;
mod store;
mod walk;

#[cfg(feature = "encrypted")]
pub use objects::rewrap_all;
pub use objects::{
    FileSnapshotObjects, InMemorySnapshotObjects, ObjectInfo, SnapshotObjects, object_id,
    root_digest,
};
pub use plan::{
    ChangeKind, NotUndone, PlanRewind, RewindEntry, RewindPlan, RewindReport, RewindTarget,
    RewindTurn, SkippedPath, TranscriptPlan,
};
pub use runtime::{SnapshotInfo, SnapshotRequest, Snapshots, TreeScan};
#[cfg(feature = "sqlite")]
pub use store::SqliteSnapshotStore;
pub use store::{
    FileEntry, InMemorySnapshotStore, PlanMark, SnapshotKind, SnapshotRecord, SnapshotStore,
    TranscriptTail,
};
pub use walk::{
    DEFAULT_SKIP_DIRS, EntryKind, ExcludeFilter, NotCovered, SkipReason, TreeWalk, WalkOptions,
    WalkedFile, walk,
};

pub(crate) use plan::{harness_writes, transcript_cut, turns_of};

use std::sync::Arc;

/// Most not-covered files a snapshot record lists; the rest are counted.
pub const NOT_COVERED_LISTED: usize = 200;

/// The audit action of a snapshot (spec 3.1 §9.3).
pub const SNAPSHOT_TAKEN_ACTION: &str = "harness.snapshot.taken";

/// The audit action of a rewind (spec 3.1 §9.3), recorded before anything changes.
pub const REWIND_ACTION: &str = "harness.rewind";

/// What snapshots keep and skip (spec 3.1 §7.27).
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::SnapshotConfig;
///
/// let config = SnapshotConfig::default();
/// assert_eq!(config.keep_turns, 50);
/// assert_eq!(config.max_bytes, 2 * 1024 * 1024 * 1024);
/// assert_eq!(config.max_files, 50_000);
/// assert_eq!(config.max_file_bytes, 8 * 1024 * 1024);
/// assert!(config.skip_dirs.iter().any(|dir| dir == "target"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotConfig {
    /// Turn snapshots kept per session, with whatever was taken after the oldest of them.
    pub keep_turns: u32,
    /// Object bytes kept per workspace; the oldest snapshots go first.
    pub max_bytes: u64,
    /// Files a snapshot records before it stops and is partial.
    pub max_files: usize,
    /// Larger files are listed as not covered.
    pub max_file_bytes: u64,
    /// Directory names never entered: build output and dependencies.
    pub skip_dirs: Vec<String>,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            keep_turns: 50,
            max_bytes: 2 * 1024 * 1024 * 1024,
            max_files: 50_000,
            max_file_bytes: 8 * 1024 * 1024,
            skip_dirs: DEFAULT_SKIP_DIRS.iter().map(|dir| (*dir).to_string()).collect(),
        }
    }
}

/// What the builder collects for snapshots: the configuration and, optionally, where
/// records and objects go (both in memory by default).
#[derive(Default)]
pub(crate) struct SnapshotSetup {
    pub(crate) config: Option<SnapshotConfig>,
    pub(crate) store: Option<Arc<dyn SnapshotStore>>,
    pub(crate) objects: Option<Arc<dyn SnapshotObjects>>,
}

impl SnapshotSetup {
    /// The workspace's snapshots, when a configuration was set.
    pub(crate) fn build(
        self,
        workspace: &adk_devtools::Workspace,
    ) -> adk_core::Result<Option<Arc<Snapshots>>> {
        let Some(config) = self.config else {
            return Ok(None);
        };
        let store = self.store.unwrap_or_else(|| Arc::new(InMemorySnapshotStore::new()));
        let objects = self.objects.unwrap_or_else(|| Arc::new(InMemorySnapshotObjects::new()));
        Ok(Some(Arc::new(Snapshots::new(config, workspace, store, objects)?)))
    }
}

/// The capability-block line for snapshots (spec 3.1 §7.25, §13 Q11): what they cover and
/// when a snapshot is partial.
pub(crate) fn safety_covers(config: &SnapshotConfig, plan_rewind: bool) -> (String, String) {
    let plan = if plan_rewind { ", transcript and plan" } else { " and transcript" };
    (
        format!(
            "files: before each user turn and plan step the whole workspace is snapshotted, \
             shell writes included; /rewind restores those files{plan}, and /redo undoes a \
             rewind; the last {} turns",
            config.keep_turns
        ),
        format!(
            "pushes, payments, sent messages, webhooks or running background shells; \
             .gitignore'd files, build \
             directories ({}), excluded paths and files over {} MiB; a snapshot that reaches \
             {} files is partial and /rewind lists what it did not cover",
            config.skip_dirs.join(", "),
            config.max_file_bytes / (1024 * 1024),
            config.max_files
        ),
    )
}

/// Takes the turn snapshot of a run that carries a user message (spec 3.1 §7.27): once per
/// run, before its first model call. A failure is logged and the turn goes on without one.
pub(crate) async fn before_turn(
    snapshots: &Snapshots,
    ctx: &dyn adk_core::InvocationContext,
    plan: Option<PlanMark>,
) {
    if ctx.user_content().parts.is_empty()
        || !snapshots.first_turn_snapshot(ctx.session_id(), ctx.invocation_id())
    {
        return;
    }
    let request = SnapshotRequest::for_context(ctx, SnapshotKind::Turn).plan(plan);
    record_taken(snapshots.take(request).await, ctx).await;
}

/// Takes the snapshot before plan step `step` (spec 3.1 §7.27).
pub(crate) async fn before_step(
    snapshots: &Snapshots,
    ctx: &dyn adk_core::InvocationContext,
    step: &str,
    plan: PlanMark,
) {
    let request = SnapshotRequest::for_context(ctx, SnapshotKind::Step).step(step).plan(Some(plan));
    record_taken(snapshots.take(request).await, ctx).await;
}

async fn record_taken(
    taken: adk_core::Result<SnapshotInfo>,
    ctx: &dyn adk_core::InvocationContext,
) {
    match taken {
        Ok(info) => {
            if let Some(trail) = ctx.run_config().audit.clone() {
                let audit = crate::audit::HarnessAudit::new(
                    trail,
                    adk_core::audit_log::AuditActor::for_context(ctx),
                    ctx.session_id(),
                );
                if let Err(error) = audit.record(SNAPSHOT_TAKEN_ACTION, info.audit_fields()).await {
                    tracing::warn!(error = %error, "the snapshot's audit record was not written");
                }
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "no snapshot before this turn; /rewind cannot restore it");
        }
    }
}
