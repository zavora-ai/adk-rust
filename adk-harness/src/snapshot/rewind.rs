//! `Harness::rewind_plan`, `Harness::rewind` and `/redo` (spec 3.1 §7.27, §9.2 I68).

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    AdkError, AdkIdentity, AppName, ErrorCategory, Event, Result, SessionId, ToolEffect, UserId,
};
use adk_session::{AppendEventRequest, GetRequest};
use serde_json::{Value, json};

use super::objects::snapshot_error;
use super::plan::{
    ChangeKind, FILE_TOOLS, NotUndone, PlanRewind, RewindEntry, RewindPlan, RewindReport,
    RewindTarget, RewindTurn, SkippedPath, TranscriptPlan, calls_in, changed_paths, diff,
    position_after,
};
use super::runtime::{SnapshotInfo, SnapshotRequest, Snapshots};
use super::store::{FileEntry, PlanMark, SnapshotKind, SnapshotRecord, TranscriptTail};
use super::{REWIND_ACTION, SNAPSHOT_TAKEN_ACTION, harness_writes, transcript_cut, turns_of};
use crate::audit::HarnessAudit;
use crate::harness::Harness;

/// Most paths `harness.rewind` lists per field; the counts cover all of them.
const AUDIT_PATHS: usize = 200;

fn rewind_error(
    code: &'static str,
    category: ErrorCategory,
    message: impl Into<String>,
) -> AdkError {
    snapshot_error(code, category, message)
}

impl Harness {
    /// The workspace's snapshots, when this harness takes them (spec 3.1 §7.27).
    pub fn snapshots(&self) -> Option<&Arc<Snapshots>> {
        self.agent().runtime().snapshots()
    }

    fn require_snapshots(&self) -> Result<&Arc<Snapshots>> {
        self.snapshots().ok_or_else(|| {
            rewind_error(
                "harness.snapshot.off",
                ErrorCategory::Unsupported,
                "snapshots are off for this harness, so there is nothing to rewind to: \
                 HarnessAgentBuilder::snapshots(SnapshotConfig::default()) turns them on (adk-rust \
                 takes them unless --no-snapshots or [snapshots] enabled = false)",
            )
        })
    }

    /// Takes a snapshot of the workspace now, outside a run, for the session — the "snapshot
    /// now" a surface or a governed call takes before something it cannot otherwise undo
    /// (spec 3.1 §7.27, §7.30). Recorded as `harness.snapshot.taken`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_harness::snapshot::SnapshotKind;
    ///
    /// let info = harness.snapshot_now(&user, &session, SnapshotKind::Destructive).await?;
    /// println!("{} files, {} bytes new", info.files, info.bytes_new);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.off` when snapshots are off, or the store's error.
    pub async fn snapshot_now(
        &self,
        user: &UserId,
        session: &SessionId,
        kind: SnapshotKind,
    ) -> Result<SnapshotInfo> {
        let snapshots = self.require_snapshots()?;
        let mark = self.plan_mark(user, session).await;
        let request = SnapshotRequest::new(
            self.app_name(),
            user.as_ref(),
            session.as_ref(),
            format!("now-{}", uuid::Uuid::new_v4().simple()),
            kind,
        )
        .plan(mark);
        let info = snapshots.take(request).await?;
        if let Some(audit) = self.rewind_audit(user, session) {
            audit.record(SNAPSHOT_TAKEN_ACTION, info.audit_fields()).await?;
        }
        Ok(info)
    }

    /// The session's user turns with their snapshots and changed-file counts, as `/rewind`
    /// lists them (spec 3.1 §7.27).
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.off` when snapshots are off, or a store's error.
    pub async fn rewind_turns(
        &self,
        user: &UserId,
        session: &SessionId,
    ) -> Result<Vec<RewindTurn>> {
        let snapshots = self.require_snapshots()?;
        let events = self.session_events(user, session).await?;
        let records = snapshots.list(self.app_name(), user.as_ref(), session.as_ref()).await?;
        let turns = turns_of(&events);
        let turn_record = |invocation: &str| {
            records.iter().rev().find(|record| {
                record.kind == SnapshotKind::Turn && record.invocation_id == invocation
            })
        };
        let mut lists: HashMap<String, Vec<FileEntry>> = HashMap::new();
        let mut current: Option<Vec<FileEntry>> = None;
        let mut out = Vec::with_capacity(turns.len());
        for (index, (number, invocation, _, preview)) in turns.iter().enumerate() {
            let record = turn_record(invocation);
            let mut changed = None;
            if let Some(record) = record {
                let before = self.cached_files(snapshots, record, &mut lists).await;
                let next = turns.get(index + 1).and_then(|next| turn_record(&next.1));
                let after = match next {
                    Some(next) => self.cached_files(snapshots, next, &mut lists).await,
                    None => {
                        if current.is_none() {
                            current = snapshots.scan().await.ok().map(|scan| scan.files);
                        }
                        current.clone()
                    }
                };
                if let (Some(before), Some(after)) = (before, after) {
                    changed = Some(changed_paths(&before, &after).len());
                }
            }
            out.push(RewindTurn {
                number: *number,
                invocation_id: invocation.clone(),
                preview: preview.clone(),
                snapshot_id: record.map(|record| record.id.clone()),
                changed_files: changed,
                partial: record.is_some_and(|record| record.partial),
            });
        }
        Ok(out)
    }

    /// What rewinding the session to `to` would do (spec 3.1 §7.27): the files to restore,
    /// recreate and delete, the conflicts, the protected paths, what the snapshot did not
    /// cover, the effects after the target that are not undone, the transcript fork and the
    /// plan. Nothing changes until [`rewind`](Self::rewind) applies it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut plan = harness.rewind_plan(&user, &session, RewindTarget::Turn(2)).await?;
    /// println!("{}", plan.render());
    /// plan.choose_conflicts(false);
    /// plan.confirm_protected();
    /// let report = harness.rewind(&user, &session, &plan).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.off` when snapshots are off, `harness.rewind.no_turn` or
    /// `harness.rewind.no_snapshot` when the target has no snapshot, or a store's error.
    pub async fn rewind_plan(
        &self,
        user: &UserId,
        session: &SessionId,
        to: RewindTarget,
    ) -> Result<RewindPlan> {
        let snapshots = self.require_snapshots()?;
        let app = self.app_name().to_string();
        let events = self.session_events(user, session).await?;
        let records = snapshots.list(&app, user.as_ref(), session.as_ref()).await?;
        let turns = turns_of(&events);
        let record = match &to {
            RewindTarget::Turn(number) => {
                let (_, invocation, _, _) = turns.iter().find(|turn| turn.0 == *number).ok_or_else(|| {
                    rewind_error(
                        "harness.rewind.no_turn",
                        ErrorCategory::NotFound,
                        format!(
                            "this session has no turn {number} ({} turns); /rewind lists them",
                            turns.len()
                        ),
                    )
                })?;
                records
                    .iter()
                    .rev()
                    .find(|record| record.kind == SnapshotKind::Turn && record.invocation_id == *invocation)
                    .ok_or_else(|| {
                        rewind_error(
                            "harness.rewind.no_snapshot",
                            ErrorCategory::NotFound,
                            format!(
                                "turn {number} has no snapshot: snapshots were off then, or it is \
                                 older than the last {} turns kept",
                                snapshots.config().keep_turns
                            ),
                        )
                    })?
            }
            RewindTarget::PlanStep(step) => records
                .iter()
                .rev()
                .find(|record| record.kind == SnapshotKind::Step && record.step.as_deref() == Some(step.as_str()))
                .ok_or_else(|| {
                    rewind_error(
                        "harness.rewind.no_snapshot",
                        ErrorCategory::NotFound,
                        format!("plan step {step} has no snapshot in this session: it has not run since snapshots were on"),
                    )
                })?,
            RewindTarget::Snapshot(id) => records.iter().find(|record| record.id == *id).ok_or_else(|| {
                rewind_error(
                    "harness.rewind.no_snapshot",
                    ErrorCategory::NotFound,
                    format!("snapshot {id} is not one of this session's"),
                )
            })?,
        }
        .clone();
        let target_files = snapshots.files(&record).await?;
        let current = snapshots.scan().await?;
        let mut entries = diff(&target_files, record.partial, &record.not_covered, &current);
        if let Some(latest) = records.last() {
            mark_conflicts(
                snapshots,
                latest,
                &record,
                &target_files,
                &current.files,
                &events,
                &mut entries,
            )
            .await?;
        }

        let (transcript, removed_from) = transcript_plan(&events, &record)?;
        let not_undone = self.not_undone(user, session, &events[removed_from..]).await;
        let turn = match record.kind {
            SnapshotKind::Turn => {
                turns.iter().find(|turn| turn.1 == record.invocation_id).map(|turn| turn.0)
            }
            _ => None,
        };
        let (plan, plan_note) = self.plan_rewind(user, session, &record).await?;
        Ok(RewindPlan {
            app_name: app,
            user_id: user.to_string(),
            session_id: session.to_string(),
            target: to,
            snapshot_id: record.id.clone(),
            snapshot_kind: record.kind,
            taken_at: record.created_at,
            turn,
            entries,
            not_covered: record.not_covered.clone(),
            not_covered_more: record.not_covered_more,
            partial: record.partial,
            not_undone,
            transcript,
            plan,
            plan_note,
            protected_confirmed: false,
        })
    }

    /// The plan that restores the snapshot taken before the session's last rewind:
    /// what `/redo` shows (spec 3.1 §7.27).
    ///
    /// # Errors
    ///
    /// Returns `harness.rewind.nothing_to_redo` when the session was never rewound, or as
    /// [`rewind_plan`](Self::rewind_plan).
    pub async fn redo_plan(&self, user: &UserId, session: &SessionId) -> Result<RewindPlan> {
        let snapshots = self.require_snapshots()?;
        let records = snapshots.list(self.app_name(), user.as_ref(), session.as_ref()).await?;
        let last = records
            .iter()
            .rev()
            .find(|record| record.kind == SnapshotKind::Rewind && record.transcript.is_some())
            .ok_or_else(|| {
                rewind_error(
                    "harness.rewind.nothing_to_redo",
                    ErrorCategory::NotFound,
                    "this session has not been rewound, so there is nothing to redo",
                )
            })?;
        self.rewind_plan(user, session, RewindTarget::Snapshot(last.id.clone())).await
    }

    /// Applies `plan` (spec 3.1 §7.27, I68): snapshots the workspace first so the rewind
    /// can be redone, records `harness.rewind` before anything changes, forks the
    /// transcript, rewinds the plan, then restores, recreates and deletes files — only
    /// inside the root, never through a symlink, a conflict only when chosen, a protected
    /// path only when confirmed, and a path that changed since the plan was made not at all.
    ///
    /// # Errors
    ///
    /// Returns `harness.rewind.run_active` while a run of the session is active,
    /// `harness.rewind.plan_mismatch` for another session's plan, the audit error under
    /// `AuditFailureMode::Block` (nothing changes then), or the session service's error.
    pub async fn rewind(
        &self,
        user: &UserId,
        session: &SessionId,
        plan: &RewindPlan,
    ) -> Result<RewindReport> {
        let snapshots = self.require_snapshots()?;
        if plan.app_name != self.app_name()
            || plan.user_id != user.as_ref()
            || plan.session_id != session.as_ref()
        {
            return Err(rewind_error(
                "harness.rewind.plan_mismatch",
                ErrorCategory::InvalidInput,
                "this rewind plan was made for another session",
            ));
        }
        if self.runner().active_session_ids().iter().any(|active| active == session.as_ref()) {
            return Err(rewind_error(
                "harness.rewind.run_active",
                ErrorCategory::InvalidInput,
                "the session is running a turn; rewind once it ends",
            ));
        }
        let record = snapshots.store().get(&plan.snapshot_id).await?.ok_or_else(|| {
            rewind_error(
                "harness.rewind.no_snapshot",
                ErrorCategory::NotFound,
                format!(
                    "snapshot {} was collected since the plan was made; make a new plan",
                    plan.snapshot_id
                ),
            )
        })?;
        let events = self.session_events(user, session).await?;
        let keep = match &plan.transcript.keep_through {
            _ if plan.transcript.unchanged => events.len(),
            None => 0,
            Some(id) => {
                events.iter().position(|event| event.id == *id).map(|index| index + 1).ok_or_else(
                    || {
                        rewind_error(
                            "harness.rewind.transcript_changed",
                            ErrorCategory::InvalidInput,
                            "the transcript changed since the plan was made; make a new plan",
                        )
                    },
                )?
            }
        };
        let removed: Vec<Value> =
            events[keep..].iter().filter_map(|event| serde_json::to_value(event).ok()).collect();

        // The snapshot before the rewind is what `/redo` restores.
        let mark = self.plan_mark(user, session).await;
        let before = snapshots
            .take(
                SnapshotRequest::new(
                    self.app_name(),
                    user.as_ref(),
                    session.as_ref(),
                    format!("rewind-{}", uuid::Uuid::new_v4().simple()),
                    SnapshotKind::Rewind,
                )
                .plan(mark)
                .transcript(TranscriptTail {
                    after: (keep > 0).then(|| events[keep - 1].id.clone()),
                    events: removed,
                }),
            )
            .await?;
        let audit = self.rewind_audit(user, session);
        if let Some(audit) = &audit {
            audit.record(SNAPSHOT_TAKEN_ACTION, before.audit_fields()).await?;
            audit.record(REWIND_ACTION, rewind_fields(plan, &before.id)).await?;
        }

        let identity = AdkIdentity::new(
            AppName::new(self.app_name()).map_err(identity_error)?,
            user.clone(),
            session.clone(),
        );
        let sessions = self.runner().session_service();
        let mut report =
            RewindReport { before_rewind: before.id.clone(), ..RewindReport::default() };
        if keep < events.len() {
            if keep == 0 {
                sessions.rewind_steps(&identity, events.len()).await?;
            } else {
                sessions.rewind(&identity, &events[keep - 1].id).await?;
            }
            report.events_removed = events.len() - keep;
        }
        if record.kind == SnapshotKind::Rewind
            && let Some(tail) = &record.transcript
        {
            for value in &tail.events {
                let event: Event = serde_json::from_value(value.clone()).map_err(|error| {
                    rewind_error(
                        "harness.rewind.corrupt_transcript",
                        ErrorCategory::Internal,
                        format!("an event kept for redo does not parse: {error}"),
                    )
                })?;
                sessions
                    .append_event_for_identity(AppendEventRequest {
                        identity: identity.clone(),
                        event,
                    })
                    .await?;
                report.events_restored += 1;
            }
        }
        report.plan_thread = self.apply_plan_rewind(user, session, plan.plan.as_ref()).await?;

        let mut rewound: BTreeMap<String, Option<String>> = BTreeMap::new();
        for entry in &plan.entries {
            match self.apply_entry(snapshots, plan, entry).await {
                Ok(written) => {
                    rewound.insert(entry.path.clone(), written);
                    match entry.change {
                        ChangeKind::Restore => report.restored.push(entry.path.clone()),
                        ChangeKind::Recreate => report.recreated.push(entry.path.clone()),
                        ChangeKind::Delete => report.deleted.push(entry.path.clone()),
                    }
                }
                Err(reason) => {
                    report.skipped.push(SkippedPath { path: entry.path.clone(), reason })
                }
            }
        }
        // What the rewind wrote is the harness's own write: not a conflict for the next one.
        if let Some(mut stored) = snapshots.store().get(&before.id).await? {
            stored.rewound = Some(rewound);
            snapshots.store().put(&stored).await?;
        }
        report.not_undone = plan.not_undone.clone();
        tracing::info!(
            snapshot.id = %plan.snapshot_id,
            rewind.restored = report.restored.len(),
            rewind.recreated = report.recreated.len(),
            rewind.deleted = report.deleted.len(),
            rewind.skipped = report.skipped.len(),
            rewind.events_removed = report.events_removed,
            "rewound"
        );
        Ok(report)
    }

    /// Applies one entry; returns the hash written (`None` for a delete), or why it was
    /// left as it was.
    async fn apply_entry(
        &self,
        snapshots: &Snapshots,
        plan: &RewindPlan,
        entry: &RewindEntry,
    ) -> std::result::Result<Option<String>, String> {
        if !entry.chosen {
            return Err(
                "changed by a command or by you since the latest snapshot; not chosen".into()
            );
        }
        if entry.protected && !plan.protected_confirmed {
            return Err("a protected path; the rewind's confirmation did not cover it".into());
        }
        let root = snapshots.root().to_path_buf();
        let path = confined(&root, &entry.path)?;
        let relative = entry.path.clone();
        let expected = entry.current_sha256.clone();
        let change = entry.change;
        let target = entry.target.clone();
        let content = match (&target, change) {
            (Some(file), ChangeKind::Restore | ChangeKind::Recreate) if file.link.is_none() => {
                Some(snapshots.object(&file.sha256).await.map_err(|error| error.message)?)
            }
            _ => None,
        };
        tokio::task::spawn_blocking(move || {
            check_unchanged(&path, expected.as_deref(), change)?;
            match (change, target) {
                (ChangeKind::Delete, _) => {
                    remove_entry(&path)?;
                    prune_empty_parents(&root, &relative);
                    Ok(None)
                }
                (_, Some(file)) => {
                    write_entry(&root, &path, &file, content.as_deref())?;
                    Ok(Some(file.sha256))
                }
                (_, None) => Err("the snapshot has no entry for it".into()),
            }
        })
        .await
        .map_err(|error| error.to_string())?
    }

    /// Effects after the target that a rewind cannot take back: every call that ran in the
    /// events it removes, other than the file tools whose files it restores, whose tool is
    /// not read-only or idempotent (spec 3.1 §7.27, 3.3 D-8).
    async fn not_undone(
        &self,
        user: &UserId,
        session: &SessionId,
        removed: &[Event],
    ) -> Vec<NotUndone> {
        let runtime = self.agent().runtime();
        let effects: HashMap<String, ToolEffect> = runtime
            .toolset()
            .all_tools()
            .iter()
            .map(|tool| (tool.name().to_string(), tool.effect()))
            .collect();
        let ledger = runtime.action_ledger();
        let mut out = Vec::new();
        for call in calls_in(removed) {
            if !call.succeeded || FILE_TOOLS.contains(&call.name.as_str()) {
                continue;
            }
            let key = call.call_id.as_ref().map(|id| {
                format!("{}/{}/{}/{}/{id}", self.app_name(), user, session, call.invocation_id)
            });
            let recorded = match &key {
                Some(key) => ledger.get(key).await.ok().flatten(),
                None => None,
            };
            let effect = recorded
                .as_ref()
                .map(|record| record.effect)
                .or_else(|| effects.get(&call.name).copied())
                .unwrap_or(ToolEffect::NonIdempotent);
            if effect != ToolEffect::NonIdempotent {
                continue;
            }
            let summary = match call.args.get("command").and_then(Value::as_str) {
                Some(command) => command.lines().next().unwrap_or_default().to_string(),
                None => call.args.to_string(),
            };
            let mut summary: String = summary.chars().take(100).collect();
            if summary.chars().count() == 100 {
                summary.push('…');
            }
            // A background shell keeps running through a rewind (spec 3.1 §7.29).
            if let Some(id) = &call.shell_id {
                let running = match runtime.toolset().shells() {
                    Some(shells) => shells
                        .list(self.app_name(), user.as_ref(), session.as_ref())
                        .await
                        .iter()
                        .any(|shell| {
                            shell.shell_id == *id && shell.status == crate::ShellStatus::Running
                        }),
                    None => false,
                };
                if running {
                    summary.push_str(&format!(
                        " (background shell {id}, still running; /shells kill {id} stops it)"
                    ));
                } else {
                    summary.push_str(&format!(" (background shell {id})"));
                }
            }
            out.push(NotUndone {
                tool: call.name,
                summary,
                ledger_key: recorded.map(|record| record.key),
            });
        }
        out
    }

    async fn session_events(&self, user: &UserId, session: &SessionId) -> Result<Vec<Event>> {
        let session = self
            .runner()
            .session_service()
            .get(GetRequest {
                app_name: self.app_name().to_string(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        Ok(session.events().all())
    }

    async fn cached_files(
        &self,
        snapshots: &Snapshots,
        record: &SnapshotRecord,
        lists: &mut HashMap<String, Vec<FileEntry>>,
    ) -> Option<Vec<FileEntry>> {
        if let Some(files) = lists.get(&record.files_object) {
            return Some(files.clone());
        }
        let files = snapshots.files(record).await.ok()?;
        lists.insert(record.files_object.clone(), files.clone());
        Some(files)
    }

    fn rewind_audit(&self, user: &UserId, session: &SessionId) -> Option<HarnessAudit> {
        let trail = self.runner().audit_trail()?.clone();
        Some(HarnessAudit::new(
            trail,
            AuditActor::subject(SubjectRef::new(SubjectKind::Human, user.to_string())),
            session.to_string(),
        ))
    }

    /// Where the session's plan stands now, if it has one.
    async fn plan_mark(&self, user: &UserId, session: &SessionId) -> Option<PlanMark> {
        #[cfg(feature = "graph")]
        if let Some(graphs) = self.agent().runtime().plan_graph() {
            return graphs
                .mark(self.app_name(), user.as_ref(), session.as_ref())
                .await
                .ok()
                .flatten();
        }
        let _ = (user, session);
        None
    }

    /// What the plan does on a rewind to `record`, or why it does not move.
    async fn plan_rewind(
        &self,
        user: &UserId,
        session: &SessionId,
        record: &SnapshotRecord,
    ) -> Result<(Option<PlanRewind>, Option<String>)> {
        let Some(now) = self.plan_mark(user, session).await else {
            return Ok((None, None));
        };
        let Some(then) = record.plan.as_ref().filter(|then| then.plan_hash == now.plan_hash) else {
            return Ok((
                None,
                Some(
                    "the plan approved after the target stays installed; /mode plan to replace it"
                        .into(),
                ),
            ));
        };
        if then.thread_id == now.thread_id
            && then.completed == now.completed
            && then.running == now.running
        {
            return Ok((None, Some("has not moved since the target".into())));
        }
        if record.kind == SnapshotKind::Rewind {
            return Ok((Some(PlanRewind::Restore { thread_id: then.thread_id.clone() }), None));
        }
        Ok((
            Some(match then.completed.last() {
                Some(step) => PlanRewind::AfterStep { step_id: step.clone() },
                None => PlanRewind::Restart,
            }),
            None,
        ))
    }

    async fn apply_plan_rewind(
        &self,
        user: &UserId,
        session: &SessionId,
        plan: Option<&PlanRewind>,
    ) -> Result<Option<String>> {
        let Some(plan) = plan else {
            return Ok(None);
        };
        #[cfg(feature = "graph")]
        if let Some(graphs) = self.agent().runtime().plan_graph() {
            let (app, user, session) = (self.app_name(), user.as_ref(), session.as_ref());
            let thread = match plan {
                PlanRewind::AfterStep { step_id } => {
                    graphs.rewind(app, user, session, step_id).await?
                }
                PlanRewind::Restart => graphs.restart(app, user, session).await?,
                PlanRewind::Restore { thread_id } => {
                    graphs.restore_thread(app, user, session, thread_id).await?
                }
            };
            return Ok(Some(thread));
        }
        let _ = (user, session, plan);
        Ok(None)
    }
}

fn identity_error(error: impl std::fmt::Display) -> AdkError {
    rewind_error("harness.rewind.identity", ErrorCategory::InvalidInput, error.to_string())
}

/// Marks the entries changed since the session's latest snapshot by something other than a
/// harness file tool — a command or the person — as conflicts, unchosen (spec 3.1 §7.27).
async fn mark_conflicts(
    snapshots: &Snapshots,
    latest: &SnapshotRecord,
    target: &SnapshotRecord,
    target_files: &[FileEntry],
    current: &[FileEntry],
    events: &[Event],
    entries: &mut [RewindEntry],
) -> Result<()> {
    let latest_files = if latest.id == target.id {
        target_files.to_vec()
    } else {
        snapshots.files(latest).await?
    };
    let changed = changed_paths(&latest_files, current);
    let written = harness_writes(&events[position_after(events, latest)..], snapshots.root());
    let now: HashMap<&str, &str> =
        current.iter().map(|file| (file.path.as_str(), file.sha256.as_str())).collect();
    let rewound = latest.rewound.as_ref();
    for entry in entries.iter_mut() {
        if !changed.contains(&entry.path) || written.contains(&entry.path) {
            continue;
        }
        // A file the last rewind wrote, still as it wrote it, is the harness's own write.
        let by_rewind = rewound
            .and_then(|map| map.get(&entry.path))
            .is_some_and(|written| written.as_deref() == now.get(entry.path.as_str()).copied());
        if !by_rewind {
            entry.conflict = true;
            entry.chosen = false;
        }
    }
    Ok(())
}

/// The transcript part of a rewind to `record`, and the index from which events are
/// removed.
fn transcript_plan(events: &[Event], record: &SnapshotRecord) -> Result<(TranscriptPlan, usize)> {
    if record.kind == SnapshotKind::Rewind
        && let Some(tail) = &record.transcript
    {
        let keep = match &tail.after {
            None => 0,
            Some(id) => events
                .iter()
                .position(|event| event.id == *id)
                .map(|index| index + 1)
                .ok_or_else(|| {
                    rewind_error(
                        "harness.rewind.transcript_changed",
                        ErrorCategory::InvalidInput,
                        "the transcript no longer holds the point that rewind forked at, so its \
                     events cannot be put back",
                    )
                })?,
        };
        let plan = TranscriptPlan {
            keep_through: tail.after.clone(),
            events_removed: events.len() - keep,
            events_restored: tail.events.len(),
            unchanged: keep == events.len() && tail.events.is_empty(),
        };
        return Ok((plan, keep));
    }
    Ok(match transcript_cut(events, record) {
        Some(cut) => (
            TranscriptPlan {
                keep_through: (cut > 0).then(|| events[cut - 1].id.clone()),
                events_removed: events.len() - cut,
                events_restored: 0,
                unchanged: cut == events.len(),
            },
            cut,
        ),
        None => (TranscriptPlan { unchanged: true, ..TranscriptPlan::default() }, events.len()),
    })
}

/// The extension fields of `harness.rewind` (spec 3.1 §9.3).
fn rewind_fields(plan: &RewindPlan, before: &str) -> Value {
    let paths = |change: ChangeKind| -> Vec<&str> {
        plan.entries
            .iter()
            .filter(|e| {
                e.change == change && e.chosen && (!e.protected || plan.protected_confirmed)
            })
            .map(|e| e.path.as_str())
            .take(AUDIT_PATHS)
            .collect()
    };
    json!({
        "target": plan.target.to_string(),
        "snapshotId": plan.snapshot_id,
        "beforeRewind": before,
        "restored": paths(ChangeKind::Restore),
        "recreated": paths(ChangeKind::Recreate),
        "deleted": paths(ChangeKind::Delete),
        "conflictsChosen": plan.conflicts().filter(|e| e.chosen).map(|e| e.path.as_str()).collect::<Vec<_>>(),
        "protectedConfirmed": plan.protected_confirmed,
        "effectsNotUndone": plan.not_undone.iter().map(|item| item.ledger_key.clone().unwrap_or_else(|| item.tool.clone())).collect::<Vec<_>>(),
        "transcriptForkedAt": plan.transcript.keep_through,
        "eventsRemoved": plan.transcript.events_removed,
        "eventsRestored": plan.transcript.events_restored,
        "planThread": plan.plan.as_ref().map(|plan| serde_json::to_value(plan).unwrap_or(Value::Null)),
    })
}

/// `relative` under `root`, refusing anything that leaves it (I68).
fn confined(root: &Path, relative: &str) -> std::result::Result<PathBuf, String> {
    let path = Path::new(relative);
    if relative.is_empty() || path.components().any(|part| !matches!(part, Component::Normal(_))) {
        return Err("not a path inside the workspace".into());
    }
    Ok(root.join(path))
}

/// Refuses a path whose current content is not what the plan saw.
fn check_unchanged(
    path: &Path,
    expected: Option<&str>,
    change: ChangeKind,
) -> std::result::Result<(), String> {
    let metadata = path.symlink_metadata();
    let now = match &metadata {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::read_link(path)
            .ok()
            .map(|target| super::object_id(target.to_string_lossy().as_bytes())),
        Ok(metadata) if metadata.is_file() => {
            // A file over the size limit was not hashed; its plan carries no hash.
            if expected.is_none() && change == ChangeKind::Restore {
                return Ok(());
            }
            std::fs::read(path).ok().map(|bytes| super::object_id(&bytes))
        }
        Ok(_) => return Err("a directory is there now".into()),
        Err(_) => None,
    };
    if now.as_deref() == expected {
        Ok(())
    } else {
        Err("it changed since the plan was made".into())
    }
}

/// Refuses a path with a symlinked directory between it and the root: a rewind never
/// follows a symlink (I68). Creates the missing directories.
fn prepare_parent(root: &Path, path: &Path) -> std::result::Result<(), String> {
    let parent = path.parent().ok_or("no parent directory")?;
    let relative = parent.strip_prefix(root).map_err(|_| "outside the workspace")?;
    let mut dir = root.to_path_buf();
    for part in relative.components() {
        dir.push(part);
        match dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("{} is a symlink; a rewind never follows one", dir.display()));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(format!("{} is a file, not a directory", dir.display())),
            Err(_) => std::fs::create_dir(&dir).map_err(|error| error.to_string())?,
        }
    }
    Ok(())
}

fn write_entry(
    root: &Path,
    path: &Path,
    file: &FileEntry,
    content: Option<&[u8]>,
) -> std::result::Result<(), String> {
    prepare_parent(root, path)?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.adk-rewind-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> std::io::Result<()> {
        match (&file.link, content) {
            (Some(target), _) => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &temp)?;
                #[cfg(not(unix))]
                {
                    let _ = target;
                    return Err(std::io::Error::other("symlinks are restored on Unix only"));
                }
            }
            (None, Some(bytes)) => {
                std::fs::write(&temp, bytes)?;
                #[cfg(unix)]
                if file.mode != 0 {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(file.mode))?;
                }
            }
            (None, None) => return Err(std::io::Error::other("no content")),
        }
        // A rename replaces a symlink at `path` itself, never what it points to.
        std::fs::rename(&temp, path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(error.to_string());
    }
    Ok(())
}

fn remove_entry(path: &Path) -> std::result::Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Removes the directories a deleted file leaves empty, up to the root.
fn prune_empty_parents(root: &Path, relative: &str) {
    let mut dir = Path::new(relative).parent().map(Path::to_path_buf);
    while let Some(current) = dir {
        if current.as_os_str().is_empty() {
            break;
        }
        // `remove_dir` removes only an empty directory and never follows a symlink.
        if std::fs::remove_dir(root.join(&current)).is_err() {
            break;
        }
        dir = current.parent().map(Path::to_path_buf);
    }
}
