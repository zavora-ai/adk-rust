//! Conformance cases for [`AutonomyStore`] backends (spec 3.5 §7.11, §11).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`unknown_job`] | A job never registered has no record, history or decisions |
//! | [`apply_registers_then_moves`] | Revision 1 registers a job; each next revision moves it and appends its change; `list` orders by job |
//! | [`apply_refuses_other_revisions`] | A revision that is not one past the stored one writes nothing |
//! | [`apply_refuses_a_mismatched_change`] | A change that does not describe its record is an error |
//! | [`decisions_once_and_in_order`] | A decision id is recorded once; decisions read back by time, then id, from `since` |
//! | [`concurrent_applies_move_once`] | Concurrent applies of one revision from two handles move the job once |
//! | [`records_survive_a_restart`] | Records, history and decisions read back through a new handle (durable backends only) |

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use awp_types::AutonomyLevel;
use chrono::{DateTime, Duration, TimeZone, Utc};
use uuid::Uuid;

use crate::KernelError;
use crate::charter::{
    AutonomyChange, AutonomyReason, AutonomyStore, DecisionOutcome, JobAutonomy, JobDecision,
};

/// Opens an [`AutonomyStore`] backend for the conformance suite (spec 3.5 §11).
#[async_trait]
pub trait AutonomyStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn AutonomyStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

fn at(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 20, 8, 0, 0).single().expect("valid")
        + Duration::minutes(minutes)
        + Duration::microseconds(17)
}

/// The record and change of `job`'s revision `revision` at `level`.
fn step(
    site: Uuid,
    job: &str,
    revision: u64,
    from: Option<AutonomyLevel>,
    level: AutonomyLevel,
) -> (JobAutonomy, AutonomyChange) {
    let when = at(i64::try_from(revision).unwrap_or(0));
    let mut record = JobAutonomy::new(site, job, level, when).with_area("serve");
    record.revision = revision;
    let reason = if revision == 1 {
        AutonomyReason::Initial { charter_version: Some(1) }
    } else {
        AutonomyReason::Lowered { reason: format!("step {revision}") }
    };
    let change = AutonomyChange {
        site,
        job: job.to_string(),
        revision,
        from,
        to: level,
        reason,
        by: SubjectRef::service("kernel:autonomy"),
        at: when,
    };
    (record, change)
}

/// A job never registered has no record, history or decisions.
pub async fn unknown_job(harness: &impl AutonomyStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    assert!(store.get(site, "seo").await.expect("get").is_none());
    assert!(store.list(site).await.expect("list").is_empty());
    assert!(store.history(site, "seo").await.expect("history").is_empty());
    assert!(store.decisions(site, "seo", at(0)).await.expect("decisions").is_empty());
}

/// Revision 1 registers a job; each next revision moves it and appends its change.
pub async fn apply_registers_then_moves(harness: &impl AutonomyStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let (first, registered) = step(site, "seo/ü", 1, None, AutonomyLevel::Propose);
    assert!(store.apply(first, registered.clone()).await.expect("apply"));
    let (second, raised) =
        step(site, "seo/ü", 2, Some(AutonomyLevel::Propose), AutonomyLevel::ActWithApproval);
    assert!(store.apply(second.clone(), raised.clone()).await.expect("apply"));
    assert_eq!(store.get(site, "seo/ü").await.expect("get"), Some(second.clone()));
    assert_eq!(store.history(site, "seo/ü").await.expect("history"), vec![registered, raised]);
    let (other, other_change) = step(site, "alarms", 1, None, AutonomyLevel::Observe);
    assert!(store.apply(other.clone(), other_change).await.expect("apply"));
    let jobs: Vec<String> =
        store.list(site).await.expect("list").into_iter().map(|j| j.job).collect();
    assert_eq!(jobs, ["alarms", "seo/ü"]);
    assert!(store.list(Uuid::new_v4()).await.expect("list").is_empty(), "sites are apart");
}

/// A revision that is not one past the stored one writes nothing.
pub async fn apply_refuses_other_revisions(harness: &impl AutonomyStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let (skip, skip_change) =
        step(site, "seo", 2, Some(AutonomyLevel::Observe), AutonomyLevel::Propose);
    assert!(!store.apply(skip, skip_change).await.expect("apply"), "no revision 1 yet");
    let (first, change) = step(site, "seo", 1, None, AutonomyLevel::Propose);
    assert!(store.apply(first.clone(), change.clone()).await.expect("apply"));
    assert!(!store.apply(first.clone(), change).await.expect("apply"), "revision 1 again");
    let (third, third_change) =
        step(site, "seo", 3, Some(AutonomyLevel::Propose), AutonomyLevel::Observe);
    assert!(!store.apply(third, third_change).await.expect("apply"), "a gap");
    assert_eq!(store.get(site, "seo").await.expect("get"), Some(first));
    assert_eq!(store.history(site, "seo").await.expect("history").len(), 1);
}

/// A change that does not describe its record is an error.
pub async fn apply_refuses_a_mismatched_change(harness: &impl AutonomyStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let (record, mut change) = step(site, "seo", 1, None, AutonomyLevel::Propose);
    change.to = AutonomyLevel::ActWithinBudget;
    let error = store.apply(record, change).await.expect_err("a mismatch");
    assert!(matches!(error, KernelError::Config { .. }), "{error}");
    assert!(store.get(site, "seo").await.expect("get").is_none());
}

/// A decision id is recorded once; decisions read back by time, then id, from `since`.
pub async fn decisions_once_and_in_order(harness: &impl AutonomyStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    let decided = |id: &str, minutes, outcome| {
        JobDecision::new(site, "replies", id, outcome, SubjectRef::human("amina"), at(minutes))
    };
    for decision in [
        decided("c", 5, DecisionOutcome::Rejected),
        decided("a", 1, DecisionOutcome::ApprovedUnchanged),
        decided("b", 5, DecisionOutcome::ApprovedWithEdits),
    ] {
        assert!(store.record_decision(decision).await.expect("record"));
    }
    assert!(
        !store.record_decision(decided("a", 9, DecisionOutcome::Rejected)).await.expect("record"),
        "an id is recorded once"
    );
    let all = store.decisions(site, "replies", at(0)).await.expect("decisions");
    let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(all[0], decided("a", 1, DecisionOutcome::ApprovedUnchanged));
    let later = store.decisions(site, "replies", at(5)).await.expect("decisions");
    assert_eq!(later.len(), 2, "since is inclusive");
    assert!(store.decisions(site, "other", at(0)).await.expect("decisions").is_empty());
}

/// Concurrent applies of one revision from two handles move the job once.
pub async fn concurrent_applies_move_once(harness: &impl AutonomyStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    let site = Uuid::new_v4();
    let (record, change) = step(site, "seo", 1, None, AutonomyLevel::Propose);
    assert!(first.apply(record, change).await.expect("apply"));
    let mut tasks = Vec::new();
    for index in 0..16 {
        let store = if index % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        let level =
            if index % 3 == 0 { AutonomyLevel::Observe } else { AutonomyLevel::ActWithApproval };
        let (record, change) = step(site, "seo", 2, Some(AutonomyLevel::Propose), level);
        tasks.push(tokio::spawn(async move { store.apply(record, change).await }));
    }
    let mut moved = 0;
    for task in tasks {
        if task.await.expect("task").expect("apply") {
            moved += 1;
        }
    }
    assert_eq!(moved, 1);
    assert_eq!(first.history(site, "seo").await.expect("history").len(), 2);
    assert_eq!(second.get(site, "seo").await.expect("get").map(|r| r.revision), Some(2));
}

/// Records, history and decisions read back through a new handle.
pub async fn records_survive_a_restart(harness: &impl AutonomyStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let site = Uuid::new_v4();
    let writer = harness.open().await;
    let (record, change) = step(site, "seo", 1, None, AutonomyLevel::Propose);
    writer.apply(record.clone(), change).await.expect("apply");
    let decision = JobDecision::new(
        site,
        "seo",
        "d-1",
        DecisionOutcome::ApprovedUnchanged,
        SubjectRef::human("amina"),
        at(3),
    );
    writer.record_decision(decision.clone()).await.expect("record");
    let reader = harness.open().await;
    assert_eq!(reader.get(site, "seo").await.expect("get"), Some(record));
    assert_eq!(reader.history(site, "seo").await.expect("history").len(), 1);
    assert_eq!(reader.decisions(site, "seo", at(0)).await.expect("decisions"), vec![decision]);
}
