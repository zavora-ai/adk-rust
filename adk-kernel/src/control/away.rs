//! "While you were away" over the `/proc` ledger, with a cursor per device (spec 3.3 §10
//! K4, §5.1 Today, §8.1; feature `proc`).
//!
//! | Route (under `/api`) | Caller | Does |
//! |---|---|---|
//! | `GET /kernel/away` | operator, or a device with `read` | The rows after the viewer's cursor: counts, spend, whether quiet time was respected, one line per event ([`AwayResponse`], [`AwayQuery`]) |
//! | `POST /kernel/away/seen` | operator, or a device with `read` | Moves the viewer's cursor to a row it was shown ([`SeenRequest`]) |
//! | `GET /kernel/away/lines/{seq}` | operator, or a device with `read` | One line with its ledger row: the line's record ([`AwayLineResponse`]) |
//!
//! The viewer is the paired device a token admitted (`device:<id>`), else the name the
//! surface gives (`viewer`, default `home`). Its cursor is a ledger sequence number kept in
//! the [`ControlStore`](super::ControlStore), so the digest is exact across a restart: a
//! page covers the rows after the cursor in sequence order, and only a row the viewer was
//! shown is ever marked seen, so marking `through` and reading again skips no row and repeats
//! none. A page stops before a missing sequence number younger than a few seconds, which a
//! store that numbers rows before it commits them (PostgreSQL) may still fill. A viewer
//! with no cursor reads the last day.
//!
//! | Line | Ledger row |
//! |---|---|
//! | `done`, `failed`, `stopped` | `process` of an agent, ending `completed`, `failed` or `cancelled` |
//! | `suggestion` | `proposal` with effect `proposed` |
//! | `question` | `observation` |
//! | `decided` | `approval` or `rejection`: a person's answer to a held call, from its audit record; `decided` |
//! | `failed` | `loop_guard` |
//! | `stoppedAll`, `resumedAll` | `governance`, `frozen` or `unfrozen` |
//!
//! `spentMicroUsd` sums the `spend` rows of the page. Quiet time was respected when no
//! suggestion's work started (`proposal` with effect `running`) inside a quiet window of its
//! area, as admission keeps it (spec 3.2 §7.9) with the windows in force now.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{ApprovalService, InboxQuery};
use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::routing::{get, post};
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};

use super::paths;
use super::routes::{Caller, ControlBackend};
use super::store::AwayCursor;
use crate::error::{KernelError, NOT_FOUND_CODE, Result};
use crate::kernel::Kernel;
use crate::partition::PartitionId;
use crate::proc::{KERNEL_AGENT, Ledger, LedgerEntry, LedgerQuery};
use crate::sched::QuietHours;

/// The most rows one page covers.
pub const MAX_PAGE_ROWS: usize = 2_000;
/// How far back a viewer with no cursor reads.
const NEW_VIEWER_WINDOW: chrono::Duration = chrono::Duration::hours(24);
/// How long a missing sequence number may still be filled by a commit in flight.
const SETTLE: chrono::Duration = chrono::Duration::seconds(5);
/// The viewer a surface that names none reads as.
pub const DEFAULT_VIEWER: &str = "home";

/// What a line of "While you were away" says happened (spec 3.3 §8.1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::AwayKind;
///
/// assert_eq!(serde_json::to_value(AwayKind::StoppedAll).unwrap(), "stoppedAll");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AwayKind {
    /// An agent finished its work.
    Done,
    /// An agent's work failed, or the loop guard ended a turn.
    Failed,
    /// An agent's work was stopped.
    Stopped,
    /// An agent suggested something to do.
    Suggestion,
    /// The kernel asks whether something it noticed is right.
    Question,
    /// A decision was answered.
    Decided,
    /// Everything was stopped.
    StoppedAll,
    /// Everything was resumed.
    ResumedAll,
}

impl AwayKind {
    /// The line a ledger row makes, or `None` for a row that is no event of its own.
    pub fn of(entry: &LedgerEntry) -> Option<Self> {
        let agent = entry.agent != KERNEL_AGENT;
        match (entry.kind.as_str(), entry.effect.as_deref()) {
            ("process", Some("completed")) if agent => Some(Self::Done),
            ("process", Some("failed")) if agent => Some(Self::Failed),
            ("process", Some("cancelled")) if agent => Some(Self::Stopped),
            ("proposal", Some("proposed")) => Some(Self::Suggestion),
            ("observation", _) => Some(Self::Question),
            // The answer's audit record is the ledger's row of a decision (spec 3.3 §12
            // phase 2): the kernel publishes no `decided` event of its own.
            ("approval" | "rejection" | "decided", _) => Some(Self::Decided),
            ("loop_guard", _) => Some(Self::Failed),
            ("governance", Some("frozen")) => Some(Self::StoppedAll),
            ("governance", Some("unfrozen")) => Some(Self::ResumedAll),
            _ => None,
        }
    }
}

/// One line of "While you were away", from one ledger row (spec 3.3 §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AwayLine {
    /// The ledger row's sequence number.
    pub seq: u64,
    /// When, in the person's local offset.
    pub at: DateTime<FixedOffset>,
    /// The area.
    pub partition: PartitionId,
    /// The agent.
    pub agent: String,
    /// What happened.
    pub kind: AwayKind,
    /// The row's effect, such as `failed` or `approve`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// The line's record: `GET` it for the ledger row.
    pub record: String,
}

impl AwayLine {
    /// The line `entry` makes, or `None`.
    pub fn of(entry: &LedgerEntry) -> Option<Self> {
        let kind = AwayKind::of(entry)?;
        // An answer says which it was, as a `decided` row's effect does.
        let effect = entry.effect.clone().or_else(|| match entry.kind.as_str() {
            "approval" => Some("approve".to_string()),
            "rejection" => Some("deny".to_string()),
            _ => None,
        });
        Some(Self {
            seq: entry.seq,
            at: entry.ts,
            partition: entry.partition.clone(),
            agent: entry.agent.clone(),
            kind,
            effect,
            record: format!(
                "{}{}",
                paths::API_PREFIX,
                paths::AWAY_LINE.replace("{seq}", &entry.seq.to_string())
            ),
        })
    }
}

/// How many lines of each kind a page holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AwayCounts {
    /// Work finished.
    pub done: usize,
    /// Work failed.
    pub failed: usize,
    /// Work stopped.
    pub stopped: usize,
    /// Suggestions made.
    pub suggestions: usize,
    /// Questions asked.
    pub questions: usize,
    /// Decisions answered.
    pub decided: usize,
}

impl AwayCounts {
    fn add(&mut self, kind: AwayKind) {
        match kind {
            AwayKind::Done => self.done += 1,
            AwayKind::Failed => self.failed += 1,
            AwayKind::Stopped => self.stopped += 1,
            AwayKind::Suggestion => self.suggestions += 1,
            AwayKind::Question => self.questions += 1,
            AwayKind::Decided => self.decided += 1,
            AwayKind::StoppedAll | AwayKind::ResumedAll => {}
        }
    }
}

/// Whether quiet time was respected over a page (spec 3.3 §5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct QuietTimeReport {
    /// No suggestion's work started inside quiet time.
    pub respected: bool,
    /// The ledger rows of work that started inside it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breaches: Vec<u64>,
}

/// `GET /kernel/away` query (spec 3.3 §10 K4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::AwayQuery;
///
/// let query: AwayQuery = serde_json::from_value(serde_json::json!({ "viewer": "terminal" })).unwrap();
/// assert_eq!(query.viewer.as_deref(), Some("terminal"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwayQuery {
    /// The surface reading, when no device token names it: letters, digits, `.`, `_`
    /// and `-`, at most 64. Defaults to `home`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    /// Read after this row instead of the viewer's cursor, to page on without marking seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// At most this many rows; at most [`MAX_PAGE_ROWS`], the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// `GET /kernel/away` answer (spec 3.3 §10 K4, §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AwayResponse {
    /// The viewer the page was read for.
    pub viewer: String,
    /// When the viewer last marked the digest seen; for a viewer with no cursor, the start
    /// of the day it reads.
    pub since: DateTime<Utc>,
    /// The row the page starts after, when it starts after one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// The last row the page covers: mark it seen to move on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<u64>,
    /// Rows after `through` are left for the next page.
    pub more: bool,
    /// How many lines of each kind.
    pub counts: AwayCounts,
    /// What the page's rows spent, in micro-USD.
    pub spent_micro_usd: u64,
    /// Whether quiet time was respected.
    pub quiet_time: QuietTimeReport,
    /// Decisions waiting for the caller now.
    pub waiting: usize,
    /// One line per event, oldest first.
    pub lines: Vec<AwayLine>,
}

/// `POST /kernel/away/seen` body (spec 3.3 §10 K4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::SeenRequest;
///
/// let body = SeenRequest { viewer: None, through: 120 };
/// assert_eq!(serde_json::to_value(&body).unwrap(), serde_json::json!({ "through": 120 }));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenRequest {
    /// The surface marking, when no device token names it. Defaults to `home`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    /// The last row the viewer was shown: a page's `through`.
    pub through: u64,
}

/// `POST /kernel/away/seen` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenResponse {
    /// The cursor after the call; never behind where it was.
    pub cursor: AwayCursor,
}

/// `GET /kernel/away/lines/{seq}` answer: a line's record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwayLineResponse {
    /// The line, when the row makes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<AwayLine>,
    /// The ledger row: content-free, as `/proc` keeps it.
    pub entry: LedgerEntry,
}

type Backend = State<Arc<dyn ControlBackend>>;

/// The routes, relative to `/api`, over the backend in their state.
pub(super) fn routes() -> Router<Arc<dyn ControlBackend>> {
    Router::new()
        .route(paths::AWAY, get(away))
        .route(paths::AWAY_SEEN, post(seen))
        .route(paths::AWAY_LINE, get(line))
}

/// The kernel and its ledger behind `backend`, or a `Config` error naming why there is
/// none.
fn ledger_of(backend: &dyn ControlBackend) -> Result<(&Kernel, Ledger)> {
    let kernel = backend.kernel().ok_or_else(|| KernelError::Config {
        reason: "\"While you were away\" reads a kernel's ledger; serve it from a Kernel".into(),
    })?;
    let ledger = kernel.ledger().cloned().ok_or_else(|| KernelError::Config {
        reason: "this kernel keeps no activity ledger: build it with KernelBuilder::ledger, as \
                 `adk-rust up` does"
            .into(),
    })?;
    Ok((kernel, ledger))
}

/// The viewer of `caller`: its paired device, else `named`, else `home`.
fn viewer_of(caller: &Caller, named: Option<&str>) -> Result<String> {
    if let Some(device) = caller.context.metadata.get(crate::pairing::DEVICE_ID_METADATA) {
        return Ok(format!("device:{device}"));
    }
    let viewer = named.unwrap_or(DEFAULT_VIEWER);
    let valid = (1..=64).contains(&viewer.len())
        && viewer.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(KernelError::Config {
            reason: format!(
                "`{viewer}` is not a viewer name: use up to 64 letters, digits, `.`, `_` or `-`"
            ),
        });
    }
    Ok(viewer.to_string())
}

/// The longest prefix of `rows` with no missing sequence number younger than [`SETTLE`]
/// after `after`, and whether rows were held back.
fn settled(
    mut rows: Vec<LedgerEntry>,
    after: Option<u64>,
    now: DateTime<Utc>,
) -> (Vec<LedgerEntry>, bool) {
    let mut expected = after.map(|after| after + 1);
    for (index, row) in rows.iter().enumerate() {
        if let Some(expected) = expected
            && row.seq != expected
            && now - row.utc() < SETTLE
        {
            rows.truncate(index);
            return (rows, true);
        }
        expected = Some(row.seq + 1);
    }
    (rows, false)
}

/// The decisions waiting for `caller` now; zero on a kernel without an inbox.
async fn waiting(
    approvals: Option<&Arc<dyn ApprovalService>>,
    caller: &Caller,
    app: &str,
) -> Result<usize> {
    let Some(approvals) = approvals else { return Ok(0) };
    let query = InboxQuery::for_approver(caller.subject(), caller.context.scopes.clone())
        .with_app_name(app)
        .with_limit(500);
    Ok(approvals.inbox(query).await?.items.len())
}

async fn away(
    State(backend): Backend,
    caller: Caller,
    Query(query): Query<AwayQuery>,
) -> Result<Json<AwayResponse>> {
    caller.require_operator("read what happened while you were away")?;
    let (kernel, ledger) = ledger_of(backend.as_ref())?;
    let viewer = viewer_of(&caller, query.viewer.as_deref())?;
    let store = kernel.config().control.control_store.clone();
    let cursor = store.cursor(&caller.context.user_id, &viewer).await?;
    let now = ledger.clock().now();
    let limit = query.limit.unwrap_or(MAX_PAGE_ROWS).clamp(1, MAX_PAGE_ROWS);
    let after = query.after.or(cursor.as_ref().map(|cursor| cursor.seq));
    let since = cursor.as_ref().map_or(now - NEW_VIEWER_WINDOW, |cursor| cursor.seen_at);
    let rows_query = match after {
        Some(after) => LedgerQuery::default().after(after),
        None => LedgerQuery::default().from(since),
    };
    let mut rows = ledger.store().list(&rows_query.limit(limit + 1)).await?;
    let full = rows.len() > limit;
    rows.truncate(limit);
    let (rows, held_back) = settled(rows, after, now);

    let mut counts = AwayCounts::default();
    let mut spent: u64 = 0;
    let mut breaches = Vec::new();
    let mut windows: BTreeMap<PartitionId, Vec<QuietHours>> = BTreeMap::new();
    let mut lines = Vec::new();
    for row in &rows {
        if row.kind == "spend" {
            let micro = row.meta.get("micro_usd").and_then(serde_json::Value::as_u64);
            spent = spent.saturating_add(micro.unwrap_or_default());
        }
        if row.kind == "proposal" && row.effect.as_deref() == Some("running") {
            if !windows.contains_key(&row.partition) {
                let quiet = kernel.supervision().quiet_windows(&row.partition).await;
                windows.insert(row.partition.clone(), quiet);
            }
            let at = row.utc();
            if windows[&row.partition].iter().any(|window| window.contains(at)) {
                breaches.push(row.seq);
            }
        }
        if let Some(line) = AwayLine::of(row) {
            counts.add(line.kind);
            lines.push(line);
        }
    }
    let waiting = waiting(backend.approvals(), &caller, backend.app_name()).await?;
    Ok(Json(AwayResponse {
        viewer,
        since,
        after,
        through: rows.last().map(|row| row.seq).or(after),
        more: full || held_back,
        counts,
        spent_micro_usd: spent,
        quiet_time: QuietTimeReport { respected: breaches.is_empty(), breaches },
        waiting,
        lines,
    }))
}

async fn seen(
    State(backend): Backend,
    caller: Caller,
    Json(request): Json<SeenRequest>,
) -> Result<Json<SeenResponse>> {
    caller.require_operator("mark what happened while you were away seen")?;
    let (kernel, ledger) = ledger_of(backend.as_ref())?;
    let viewer = viewer_of(&caller, request.viewer.as_deref())?;
    // A cursor never passes the last row: rows written after it would be skipped.
    let query = LedgerQuery::default().after(request.through.saturating_sub(1)).limit(1);
    if ledger.store().list(&query).await?.is_empty() {
        return Err(KernelError::Config {
            reason: format!(
                "the ledger has no row {} yet: mark seen a page's `through`",
                request.through
            ),
        });
    }
    let cursor = AwayCursor::new(
        caller.context.user_id.clone(),
        viewer,
        request.through,
        ledger.clock().now(),
    );
    let cursor = kernel.config().control.control_store.advance_cursor(cursor).await?;
    Ok(Json(SeenResponse { cursor }))
}

async fn line(
    State(backend): Backend,
    caller: Caller,
    UrlPath(seq): UrlPath<u64>,
) -> Result<Json<AwayLineResponse>> {
    caller.require_operator("read what happened while you were away")?;
    let (_, ledger) = ledger_of(backend.as_ref())?;
    let query = LedgerQuery::default().after(seq.saturating_sub(1)).limit(1);
    let entry = ledger.store().list(&query).await?.into_iter().find(|entry| entry.seq == seq);
    let Some(entry) = entry else {
        return Err(KernelError::Core(adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::NotFound,
            NOT_FOUND_CODE,
            format!("the ledger has no row {seq}"),
        )));
    };
    Ok(Json(AwayLineResponse { line: AwayLine::of(&entry), entry }))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn row(seq: u64, at: DateTime<Utc>) -> LedgerEntry {
        let mut entry: LedgerEntry = serde_json::from_value(serde_json::json!({
            "ts": at.fixed_offset(), "partition": "default", "agent": "kernel", "kind": "wake",
        }))
        .unwrap();
        entry.seq = seq;
        entry
    }

    #[test]
    fn a_young_gap_holds_the_page_back_and_an_old_one_does_not() {
        let now = Utc.with_ymd_and_hms(2026, 10, 24, 7, 0, 0).unwrap();
        let old = now - chrono::Duration::minutes(5);
        let young = now - chrono::Duration::seconds(1);
        let rows = vec![row(11, old), row(12, old), row(14, young), row(15, young)];
        let (kept, held) = settled(rows.clone(), Some(10), now);
        assert_eq!((kept.iter().map(|r| r.seq).collect::<Vec<_>>(), held), (vec![11, 12], true));
        let rows = vec![row(11, old), row(13, old)];
        let (kept, held) = settled(rows, Some(10), now);
        assert_eq!((kept.len(), held), (2, false), "a gap older than the settle time stays a gap");
        let (kept, held) = settled(vec![row(12, young)], Some(10), now);
        assert!(kept.is_empty() && held, "the first row counts too");
        let (kept, held) = settled(vec![row(40, young), row(41, young)], None, now);
        assert_eq!((kept.len(), held), (2, false), "a viewer with no cursor starts anywhere");
    }

    #[test]
    fn rows_make_the_lines_the_home_shows() {
        let mut entry = row(3, Utc::now());
        assert_eq!(AwayKind::of(&entry), None, "a wake is no event of its own");
        entry.kind = "process".into();
        entry.effect = Some("failed".into());
        assert_eq!(AwayKind::of(&entry), None, "the kernel's own processes are not lines");
        entry.agent = "urn:adk:agent:adk-rust/finance".into();
        assert_eq!(AwayKind::of(&entry), Some(AwayKind::Failed));
        let line = AwayLine::of(&entry).unwrap();
        assert_eq!(line.record, "/api/kernel/away/lines/3");
        entry.kind = "governance".into();
        entry.effect = Some("frozen".into());
        assert_eq!(AwayKind::of(&entry), Some(AwayKind::StoppedAll));
    }

    /// A person's answer reaches the ledger as the `approval` or `rejection` row of its audit
    /// record, and is a decided line saying which it was (spec 3.3 §12 phase 2).
    #[test]
    fn an_answer_is_a_decided_line() {
        let mut entry = row(7, Utc::now());
        entry.agent = "urn:adk:agent:adk-rust/adk-rust".into();
        for (kind, effect) in [("approval", "approve"), ("rejection", "deny")] {
            entry.kind = kind.into();
            entry.effect = None;
            let line = AwayLine::of(&entry).expect("an answer is a line");
            assert_eq!((line.kind, line.effect.as_deref()), (AwayKind::Decided, Some(effect)));
        }
        entry.kind = "held".into();
        assert_eq!(AwayKind::of(&entry), None, "a held call is no answer");
    }
}
