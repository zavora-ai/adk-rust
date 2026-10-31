//! What a rewind will do, and what it did (spec 3.1 §7.27): the difference between a
//! snapshot and the current tree, with conflicts, protected paths, files not covered,
//! effects not undone, the transcript fork and the plan.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::store::{FileEntry, SnapshotKind};
use super::walk::{NotCovered, SkipReason};

// The rewind half of the plan — the file diff, the transcript cut and the writes the
// transcript records — runs only with `runner`, which has the sessions it reads.
#[cfg(feature = "runner")]
use std::collections::{BTreeMap, BTreeSet, HashMap};
#[cfg(feature = "runner")]
use std::path::{Component, Path};

#[cfg(feature = "runner")]
use adk_core::{Event, Part};
#[cfg(feature = "runner")]
use serde_json::Value;

#[cfg(feature = "runner")]
use super::runtime::TreeScan;
#[cfg(feature = "runner")]
use super::store::SnapshotRecord;

/// Most paths a rendered plan or report lists per section.
const RENDER_PATHS: usize = 12;

/// Where to rewind to (spec 3.1 §7.27).
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::RewindTarget;
///
/// assert_eq!(RewindTarget::Turn(2).to_string(), "before turn 2");
/// assert_eq!(RewindTarget::PlanStep("3".into()).to_string(), "before plan step 3");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RewindTarget {
    /// Before the session's user turn `n` (from 1), as `/rewind` numbers them.
    Turn(u32),
    /// Before the plan step with this id ran: the step and the ones after it run again.
    PlanStep(String),
    /// The snapshot with this id; `/redo` names the one taken before the last rewind.
    Snapshot(String),
}

impl std::fmt::Display for RewindTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Turn(n) => write!(f, "before turn {n}"),
            Self::PlanStep(step) => write!(f, "before plan step {step}"),
            Self::Snapshot(id) => write!(f, "snapshot {id}"),
        }
    }
}

/// What a rewind does to one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    /// Modified since the target: its content is put back.
    Restore,
    /// Deleted since the target: it is written again.
    Recreate,
    /// Created since the target: it is removed.
    Delete,
}

impl ChangeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Restore => "restore",
            Self::Recreate => "recreate",
            Self::Delete => "delete",
        }
    }
}

/// One path a rewind changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindEntry {
    /// Relative to the workspace root.
    pub path: String,
    /// What happens to it.
    pub change: ChangeKind,
    /// Its content's hash when the plan was made; `None` when absent or not hashed. A path
    /// that changed since is skipped.
    pub current_sha256: Option<String>,
    /// Its entry in the target snapshot; `None` for a delete.
    pub target: Option<FileEntry>,
    /// Changed since the latest snapshot by something other than a harness file tool — a
    /// command or the person: restored only when chosen.
    pub conflict: bool,
    /// A protected path (I29): restored only under the rewind's confirmation.
    pub protected: bool,
    /// Whether the rewind changes it; a conflict starts unchosen.
    pub chosen: bool,
}

/// An effect after the target that a rewind cannot take back (spec 3.1 §7.27, 3.3 D-8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotUndone {
    /// The tool that ran.
    pub tool: String,
    /// What it did, in one line.
    pub summary: String,
    /// Its action-ledger key, when the ledger recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger_key: Option<String>,
}

/// What a rewind does to the session's transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPlan {
    /// The id of the last event kept; `None` keeps none.
    pub keep_through: Option<String>,
    /// Events removed from the end.
    pub events_removed: usize,
    /// Events a redo puts back after `keep_through`.
    pub events_restored: usize,
    /// Whether the transcript stays as it is.
    pub unchanged: bool,
}

/// What a rewind does to the session's plan graph (spec 3.1 §7.11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum PlanRewind {
    /// `PlanGraphs::rewind` to just after this step completed.
    #[serde(rename_all = "camelCase")]
    AfterStep {
        /// The step.
        step_id: String,
    },
    /// No step had completed: the plan runs from its first step again.
    Restart,
    /// The plan goes back to the thread it ran on before a rewind (`/redo`).
    #[serde(rename_all = "camelCase")]
    Restore {
        /// The thread.
        thread_id: String,
    },
}

/// The difference between a snapshot and the current tree, shown and confirmed once before
/// [`Harness::rewind`](crate::Harness::rewind) applies it (spec 3.1 §7.27).
///
/// Conflicts start unchosen ([`choose_conflicts`](Self::choose_conflicts)); protected paths
/// are restored only after [`confirm_protected`](Self::confirm_protected), which a surface
/// calls when the person confirms a plan that lists them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindPlan {
    /// The application.
    pub app_name: String,
    /// The user.
    pub user_id: String,
    /// The session.
    pub session_id: String,
    /// What was asked for.
    pub target: RewindTarget,
    /// The snapshot the files come from.
    pub snapshot_id: String,
    /// Its kind.
    pub snapshot_kind: SnapshotKind,
    /// When it was taken.
    pub taken_at: DateTime<Utc>,
    /// The turn it precedes, numbered as `/rewind` numbers them.
    pub turn: Option<u32>,
    /// Paths that change.
    pub entries: Vec<RewindEntry>,
    /// Files the snapshot left out, never touched.
    pub not_covered: Vec<NotCovered>,
    /// How many more it left out than listed.
    pub not_covered_more: usize,
    /// Whether the snapshot stopped at `max_files`: a file it did not reach is never
    /// deleted.
    pub partial: bool,
    /// Effects after the target a rewind cannot take back.
    pub not_undone: Vec<NotUndone>,
    /// The transcript fork.
    pub transcript: TranscriptPlan,
    /// The plan graph's rewind, when the session has a plan.
    pub plan: Option<PlanRewind>,
    /// Why the plan does not move, when it does not.
    pub plan_note: Option<String>,
    /// Whether the person confirmed the protected paths the plan lists.
    pub protected_confirmed: bool,
}

impl RewindPlan {
    /// The paths of `change`.
    pub fn paths(&self, change: ChangeKind) -> Vec<&str> {
        self.entries.iter().filter(|e| e.change == change).map(|e| e.path.as_str()).collect()
    }

    /// The conflicting entries.
    pub fn conflicts(&self) -> impl Iterator<Item = &RewindEntry> {
        self.entries.iter().filter(|entry| entry.conflict)
    }

    /// The protected entries.
    pub fn protected(&self) -> impl Iterator<Item = &RewindEntry> {
        self.entries.iter().filter(|entry| entry.protected)
    }

    /// Chooses, or leaves out, every conflicting path.
    pub fn choose_conflicts(&mut self, chosen: bool) {
        for entry in self.entries.iter_mut().filter(|entry| entry.conflict) {
            entry.chosen = chosen;
        }
    }

    /// Chooses the entry for `path`; returns whether the plan has one.
    pub fn choose(&mut self, path: &str) -> bool {
        match self.entries.iter_mut().find(|entry| entry.path == path) {
            Some(entry) => {
                entry.chosen = true;
                true
            }
            None => false,
        }
    }

    /// Records that the person confirmed the protected paths listed (the non-waivable
    /// approval the rewind's confirmation carries, I29).
    pub fn confirm_protected(&mut self) {
        self.protected_confirmed = true;
    }

    /// Whether the rewind would change nothing: no path, the transcript as it is, no plan.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.transcript.unchanged && self.plan.is_none()
    }

    /// The plan as every surface shows it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let plan = harness.rewind_plan(&user, &session, RewindTarget::Turn(1)).await?;
    /// println!("{}", plan.render());
    /// ```
    pub fn render(&self) -> String {
        let mut out = String::new();
        let when = self.taken_at.format("%Y-%m-%d %H:%M:%S UTC");
        let _ = match self.snapshot_kind {
            SnapshotKind::Rewind => writeln!(
                out,
                "Redo: put back what the rewind at {when} replaced (snapshot {})",
                self.snapshot_id
            ),
            _ => writeln!(
                out,
                "Rewind {} — snapshot {} ({}, {when})",
                self.target, self.snapshot_id, self.snapshot_kind
            ),
        };
        for (change, heading) in [
            (ChangeKind::Restore, "modified since, put back"),
            (ChangeKind::Recreate, "deleted since, written again"),
            (ChangeKind::Delete, "created since, removed"),
        ] {
            let paths: Vec<&str> = self
                .entries
                .iter()
                .filter(|e| e.change == change && !e.conflict)
                .map(|e| e.path.as_str())
                .collect();
            if !paths.is_empty() {
                let _ = writeln!(
                    out,
                    "  {:<9} {} {heading}: {}",
                    change.label(),
                    count(paths.len(), "file"),
                    list(&paths)
                );
            }
        }
        let conflicts: Vec<String> =
            self.conflicts().map(|e| format!("{} ({})", e.path, e.change.label())).collect();
        if !conflicts.is_empty() {
            let chosen = self.conflicts().filter(|e| e.chosen).count();
            let _ = writeln!(
                out,
                "  conflicts {} changed since the latest snapshot by a command or by you, {}: {}",
                count(conflicts.len(), "file"),
                if chosen == conflicts.len() {
                    "restored as chosen"
                } else {
                    "kept unless you choose them"
                },
                list(&conflicts.iter().map(String::as_str).collect::<Vec<_>>())
            );
        }
        let protected: Vec<&str> = self.protected().map(|e| e.path.as_str()).collect();
        if !protected.is_empty() {
            let _ = writeln!(
                out,
                "  protected {} changed only with your confirmation: {}",
                count(protected.len(), "path"),
                list(&protected)
            );
        }
        if !self.not_covered.is_empty() || self.partial {
            let reasons: Vec<String> = self
                .not_covered
                .iter()
                .map(|item| match &item.reason {
                    SkipReason::TooLarge { len } => {
                        format!("{} (over the size limit, {len} bytes)", item.path)
                    }
                    SkipReason::Excluded => format!("{} (excluded)", item.path),
                })
                .collect();
            let mut line = format!(
                "  not covered, never touched: {}",
                list(&reasons.iter().map(String::as_str).collect::<Vec<_>>())
            );
            if self.not_covered_more > 0 {
                let _ = write!(line, " (+{} more)", self.not_covered_more);
            }
            if self.partial {
                line.push_str("; the snapshot was partial (file cap reached): files it did not reach are kept");
            }
            let _ = writeln!(out, "{line}");
        }
        if !self.not_undone.is_empty() {
            let items: Vec<String> = self
                .not_undone
                .iter()
                .map(|item| format!("{}: {}", item.tool, item.summary))
                .collect();
            let _ = writeln!(
                out,
                "  not undone (effects outside the workspace stay): {}",
                list(&items.iter().map(String::as_str).collect::<Vec<_>>())
            );
        }
        let transcript = &self.transcript;
        if transcript.unchanged {
            let _ = writeln!(out, "  transcript stays as it is");
        } else if transcript.events_restored > 0 {
            let _ = writeln!(
                out,
                "  transcript {} removed, {} put back",
                count(transcript.events_removed, "event"),
                count(transcript.events_restored, "event")
            );
        } else {
            let at = match self.turn {
                Some(turn) => format!("before turn {turn}"),
                None => "at the snapshot".to_string(),
            };
            let _ = writeln!(
                out,
                "  transcript forks {at}: {} removed",
                count(transcript.events_removed, "event")
            );
        }
        match (&self.plan, &self.plan_note) {
            (Some(PlanRewind::AfterStep { step_id }), _) => {
                let _ = writeln!(
                    out,
                    "  plan      rewinds to just after step {step_id}; the steps after it run again"
                );
            }
            (Some(PlanRewind::Restart), _) => {
                let _ = writeln!(out, "  plan      runs again from its first step");
            }
            (Some(PlanRewind::Restore { .. }), _) => {
                let _ = writeln!(out, "  plan      goes back to where it stood before the rewind");
            }
            (None, Some(note)) => {
                let _ = writeln!(out, "  plan      {note}");
            }
            (None, None) => {}
        }
        if self.entries.is_empty() {
            let _ = writeln!(out, "  files     no file differs from the snapshot");
        }
        out
    }
}

/// A path a rewind left as it was, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedPath {
    /// Relative to the workspace root.
    pub path: String,
    /// Why.
    pub reason: String,
}

/// What [`Harness::rewind`](crate::Harness::rewind) did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindReport {
    /// The snapshot taken before the rewind, which `/redo` restores.
    pub before_rewind: String,
    /// Paths put back.
    pub restored: Vec<String>,
    /// Paths written again.
    pub recreated: Vec<String>,
    /// Paths removed.
    pub deleted: Vec<String>,
    /// Paths left as they were.
    pub skipped: Vec<SkippedPath>,
    /// Events removed from the transcript.
    pub events_removed: usize,
    /// Events put back (`/redo`).
    pub events_restored: usize,
    /// The plan's thread after the rewind, when the plan moved.
    pub plan_thread: Option<String>,
    /// Effects the rewind could not take back.
    pub not_undone: Vec<NotUndone>,
}

impl RewindReport {
    /// The report as every surface shows it.
    pub fn render(&self) -> String {
        let mut out = format!(
            "Rewound: {} restored, {} recreated, {} deleted",
            self.restored.len(),
            self.recreated.len(),
            self.deleted.len()
        );
        if self.events_removed > 0 || self.events_restored > 0 {
            let _ = write!(
                out,
                "; transcript: {} removed, {} put back",
                count(self.events_removed, "event"),
                count(self.events_restored, "event")
            );
        }
        if let Some(thread) = &self.plan_thread {
            let _ = write!(out, "; plan now on {thread}");
        }
        out.push('.');
        for skipped in &self.skipped {
            let _ = write!(out, "\n  kept {}: {}", skipped.path, skipped.reason);
        }
        if !self.not_undone.is_empty() {
            let _ = write!(out, "\n  not undone: {}", self.not_undone.len());
        }
        let _ = write!(out, "\n/redo restores what was there before this rewind.");
        out
    }
}

/// One user turn of a session, as `/rewind` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindTurn {
    /// From 1.
    pub number: u32,
    /// Its run.
    pub invocation_id: String,
    /// The start of what the person typed.
    pub preview: String,
    /// The snapshot taken before it, if any.
    pub snapshot_id: Option<String>,
    /// Files that differ between its snapshot and the next one (or the current tree).
    pub changed_files: Option<usize>,
    /// Whether its snapshot was partial.
    pub partial: bool,
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn list(items: &[&str]) -> String {
    let mut shown: Vec<String> =
        items.iter().take(RENDER_PATHS).map(|item| adk_core_escape(item)).collect();
    if items.len() > RENDER_PATHS {
        shown.push(format!("+{} more", items.len() - RENDER_PATHS));
    }
    shown.join(", ")
}

/// Paths and summaries come from the workspace and the model; control characters never
/// reach the terminal.
fn adk_core_escape(text: &str) -> String {
    crate::approvals::escape_control(text)
}

/// The rewind of the files: the target's entries against the current tree. A path the
/// target did not cover — left out by size or exclusion, or past a partial walk — is never
/// deleted.
#[cfg(feature = "runner")]
pub(crate) fn diff(
    target: &[FileEntry],
    target_partial: bool,
    target_not_covered: &[NotCovered],
    current: &TreeScan,
) -> Vec<RewindEntry> {
    let now: HashMap<&str, &FileEntry> =
        current.files.iter().map(|file| (file.path.as_str(), file)).collect();
    let now_not_covered: BTreeSet<&str> =
        current.not_covered.iter().map(|item| item.path.as_str()).collect();
    let then: HashMap<&str, &FileEntry> =
        target.iter().map(|file| (file.path.as_str(), file)).collect();
    let then_not_covered: BTreeSet<&str> =
        target_not_covered.iter().map(|item| item.path.as_str()).collect();
    let mut entries = Vec::new();
    for file in target {
        match now.get(file.path.as_str()) {
            Some(current) if current.same_content(file) => {}
            Some(current) => {
                entries.push(entry(&file.path, ChangeKind::Restore, Some(current), Some(file)))
            }
            // Left out of this walk (now too large or excluded): it exists, so it is restored.
            None if now_not_covered.contains(file.path.as_str()) => {
                entries.push(entry(&file.path, ChangeKind::Restore, None, Some(file)));
            }
            None => entries.push(entry(&file.path, ChangeKind::Recreate, None, Some(file))),
        }
    }
    for file in &current.files {
        if then.contains_key(file.path.as_str())
            || then_not_covered.contains(file.path.as_str())
            || target_partial
        {
            continue;
        }
        entries.push(entry(&file.path, ChangeKind::Delete, Some(file), None));
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

#[cfg(feature = "runner")]
fn entry(
    path: &str,
    change: ChangeKind,
    current: Option<&FileEntry>,
    target: Option<&FileEntry>,
) -> RewindEntry {
    RewindEntry {
        path: path.to_string(),
        change,
        current_sha256: current.map(|file| file.sha256.clone()),
        target: target.cloned(),
        conflict: false,
        protected: crate::approvals::matcher::is_protected_path(path),
        chosen: true,
    }
}

/// The paths that differ between two file lists.
#[cfg(feature = "runner")]
pub(crate) fn changed_paths(before: &[FileEntry], after: &[FileEntry]) -> BTreeSet<String> {
    let mut by_path: BTreeMap<&str, (Option<&FileEntry>, Option<&FileEntry>)> = BTreeMap::new();
    for file in before {
        by_path.entry(file.path.as_str()).or_default().0 = Some(file);
    }
    for file in after {
        by_path.entry(file.path.as_str()).or_default().1 = Some(file);
    }
    by_path
        .into_iter()
        .filter(|(_, pair)| match pair {
            (Some(a), Some(b)) => !a.same_content(b),
            _ => true,
        })
        .map(|(path, _)| path.to_string())
        .collect()
}

/// The user turns of a session's events: a user message that starts a run, numbered from 1,
/// with the index of its first event and the start of its text.
#[cfg(feature = "runner")]
pub(crate) fn turns_of(events: &[Event]) -> Vec<(u32, String, usize, String)> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut turns = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let first = seen.insert(event.invocation_id.as_str());
        if !first || event.author != "user" {
            continue;
        }
        let text: String = event
            .llm_response
            .content
            .iter()
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .collect::<Vec<_>>()
            .join(" ");
        if text.trim().is_empty() {
            continue;
        }
        let mut preview: String =
            text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
        if text.chars().count() > 60 {
            preview.push('…');
        }
        turns.push((turns.len() as u32 + 1, event.invocation_id.clone(), index, preview));
    }
    turns
}

/// Where the transcript forks for `record`: the index of the first event a rewind to it
/// removes, or `None` when it removes none. A turn snapshot forks before the turn's user
/// message; a step or `Destructive` snapshot before the next user message that followed it.
#[cfg(feature = "runner")]
pub(crate) fn transcript_cut(events: &[Event], record: &SnapshotRecord) -> Option<usize> {
    match record.kind {
        SnapshotKind::Turn => events
            .iter()
            .position(|event| event.invocation_id == record.invocation_id)
            .or_else(|| after(events, record, true)),
        SnapshotKind::Step | SnapshotKind::Destructive | SnapshotKind::Rewind => {
            after(events, record, true)
        }
    }
}

/// The index of the first event at or after `record` (a user message when `user_only`).
#[cfg(feature = "runner")]
fn after(events: &[Event], record: &SnapshotRecord, user_only: bool) -> Option<usize> {
    events.iter().position(|event| {
        event.timestamp >= record.created_at && (!user_only || event.author == "user")
    })
}

/// The index of the first event after `record` was taken: where "since this snapshot"
/// starts in the transcript.
#[cfg(feature = "runner")]
pub(crate) fn position_after(events: &[Event], record: &SnapshotRecord) -> usize {
    let found = match record.kind {
        SnapshotKind::Turn => {
            events.iter().position(|event| event.invocation_id == record.invocation_id)
        }
        _ => None,
    };
    found.or_else(|| after(events, record, false)).unwrap_or(events.len())
}

/// A tool call in the transcript and whether it ran without an error.
#[cfg(feature = "runner")]
#[derive(Clone, Debug)]
pub(crate) struct RanCall {
    pub(crate) name: String,
    pub(crate) args: Value,
    pub(crate) call_id: Option<String>,
    pub(crate) invocation_id: String,
    pub(crate) succeeded: bool,
    /// The background shell the call started, which outlives a rewind (spec 3.1 §7.29).
    pub(crate) shell_id: Option<String>,
}

/// The tool calls in `events` that have a response, with whether it was a success.
#[cfg(feature = "runner")]
pub(crate) fn calls_in(events: &[Event]) -> Vec<RanCall> {
    let mut calls: Vec<RanCall> = Vec::new();
    let mut answered: HashMap<String, bool> = HashMap::new();
    let mut shells: HashMap<String, String> = HashMap::new();
    for event in events {
        for part in event.llm_response.content.iter().flat_map(|content| content.parts.iter()) {
            match part {
                Part::FunctionCall { name, args, id, .. } => calls.push(RanCall {
                    name: name.clone(),
                    args: args.clone(),
                    call_id: id.clone(),
                    invocation_id: event.invocation_id.clone(),
                    succeeded: false,
                    shell_id: None,
                }),
                Part::FunctionResponse { function_response, id, .. } if id.is_some() => {
                    let failed = function_response.response.get("error").is_some()
                        || function_response
                            .response
                            .get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|status| {
                                matches!(status, "denied" | "not_run" | "result_withheld" | "held")
                            });
                    let response = &function_response.response;
                    if response.get("status").and_then(Value::as_str) == Some("running")
                        && let Some(shell) = response.get("shell_id").and_then(Value::as_str)
                    {
                        shells.insert(id.clone().unwrap_or_default(), shell.to_string());
                    }
                    answered.insert(id.clone().unwrap_or_default(), !failed);
                }
                _ => {}
            }
        }
    }
    calls
        .into_iter()
        .filter_map(|mut call| {
            let id = call.call_id.as_deref()?;
            call.succeeded = *answered.get(id)?;
            call.shell_id = shells.get(id).cloned();
            Some(call)
        })
        .collect()
}

/// The harness file tools whose effect is the workspace files a snapshot restores.
#[cfg(feature = "runner")]
pub(crate) const FILE_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch"];

/// The paths the harness's file tools wrote in `events`: a file a command or the person
/// changed is a conflict, one a file tool wrote is not (spec 3.1 §7.27). The transcript is
/// the write record here; the digest records of §7.26 tighten it to the content written.
#[cfg(feature = "runner")]
pub(crate) fn harness_writes(events: &[Event], root: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for call in calls_in(events) {
        if !call.succeeded || !FILE_TOOLS.contains(&call.name.as_str()) {
            continue;
        }
        if let Some(path) = call.args.get("path").and_then(Value::as_str) {
            paths.extend(normalize(path, root));
        }
        // `apply_patch` names its files in the patch headers.
        for text in call
            .args
            .as_object()
            .into_iter()
            .flat_map(|args| args.values())
            .filter_map(Value::as_str)
        {
            for line in text.lines() {
                for marker in
                    ["*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "]
                {
                    if let Some(path) = line.strip_prefix(marker) {
                        paths.extend(normalize(path.trim(), root));
                    }
                }
            }
        }
    }
    paths
}

/// A tool's path argument as a workspace-relative path with `/` separators.
#[cfg(feature = "runner")]
fn normalize(path: &str, root: &Path) -> Option<String> {
    let path = Path::new(path);
    let relative = if path.is_absolute() { path.strip_prefix(root).ok()? } else { path };
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

// The tests exercise the rewind half, which needs `runner`.
#[cfg(all(test, feature = "runner"))]
mod tests {
    use super::*;
    use adk_core::{Content, FunctionResponseData};
    use serde_json::json;

    fn file(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            sha256: sha.into(),
            len: 1,
            mode: 0o644,
            mtime_ns: 0,
            inode: 0,
            link: None,
        }
    }

    #[test]
    fn the_diff_restores_recreates_and_deletes_but_never_what_the_target_did_not_cover() {
        let target = vec![file("a", "1"), file("b", "2"), file("AGENTS.md", "3")];
        let current = TreeScan {
            files: vec![file("a", "1"), file("AGENTS.md", "4"), file("new", "5"), file("big", "6")],
            partial: false,
            not_covered: Vec::new(),
        };
        let skipped =
            vec![NotCovered { path: "big".into(), reason: SkipReason::TooLarge { len: 9 } }];
        let entries = diff(&target, false, &skipped, &current);
        let summary: Vec<(&str, ChangeKind, bool)> =
            entries.iter().map(|e| (e.path.as_str(), e.change, e.protected)).collect();
        assert_eq!(
            summary,
            [
                ("AGENTS.md", ChangeKind::Restore, true),
                ("b", ChangeKind::Recreate, false),
                ("new", ChangeKind::Delete, false),
            ]
        );
        // A partial target deletes nothing it may not have reached.
        let partial = diff(&target, true, &[], &current);
        assert!(partial.iter().all(|e| e.change != ChangeKind::Delete));
    }

    fn event(invocation: &str, author: &str, parts: Vec<Part>) -> Event {
        let mut event = Event::new(invocation);
        event.author = author.into();
        event.llm_response.content = Some(Content { role: author.into(), parts });
        event
    }

    #[test]
    fn turns_are_user_messages_that_start_a_run() {
        let events = vec![
            event("i1", "user", vec![Part::Text { text: "make two files".into() }]),
            event("i1", "harness", vec![Part::Text { text: "done".into() }]),
            event("i1", "user", vec![Part::Text { text: "[plan step 1] ...".into() }]),
            event("i2", "user", vec![Part::Text { text: "now delete one".into() }]),
        ];
        let turns = turns_of(&events);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[1].0, turns[1].1.as_str(), turns[1].2), (2, "i2", 3));
        assert_eq!(turns[0].3, "make two files");
    }

    #[test]
    fn file_tool_writes_are_known_and_command_writes_are_not() {
        let call = |id: &str, name: &str, args: Value| Part::FunctionCall {
            name: name.into(),
            args,
            id: Some(id.into()),
            thought_signature: None,
        };
        let answer = |id: &str, name: &str, response: Value| Part::FunctionResponse {
            function_response: FunctionResponseData::new(name, response),
            id: Some(id.into()),
            annotations: None,
        };
        // `/work` has no drive letter, so it is not absolute on Windows.
        let root = &std::env::temp_dir().join("work");
        let absolute = root.join("src").join("c.rs");
        let events = vec![
            event(
                "i1",
                "harness",
                vec![
                    call("c1", "write_file", json!({"path": "./a.txt", "content": "x"})),
                    call("c2", "bash", json!({"command": "echo b > b.txt"})),
                    call("c3", "edit_file", json!({"path": absolute.to_string_lossy()})),
                    call("c4", "write_file", json!({"path": "denied.txt"})),
                ],
            ),
            event(
                "i1",
                "harness",
                vec![
                    answer("c1", "write_file", json!({"path": "a.txt"})),
                    answer("c2", "bash", json!({"exit_code": 0})),
                    answer("c3", "edit_file", json!({"path": "src/c.rs"})),
                    answer("c4", "write_file", json!({"error": "denied"})),
                ],
            ),
        ];
        let written = harness_writes(&events, root);
        assert_eq!(written.into_iter().collect::<Vec<_>>(), ["a.txt", "src/c.rs"]);
        assert_eq!(normalize("../outside", root), None);
    }
}
