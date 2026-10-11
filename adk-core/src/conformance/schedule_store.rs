//! Conformance cases for [`ScheduleStore`] backends.
//!
//! | Case | Contract | Backend primitive |
//! |------|----------|-------------------|
//! | [`upsert_is_idempotent`] | Insert sets revision 1; an identical upsert changes nothing; a change moves the revision; unknown ids fail loudly | Upsert keyed by id |
//! | [`due_orders_by_fire_time`] | `due` returns unpaused schedules at or before `now`, earliest first, up to `limit` | Index on the next fire time |
//! | [`advance_has_one_winner`] | 32 concurrent advances with one expected revision across two handles succeed exactly once | Compare-and-swap on the revision |
//! | [`paused_schedules_are_not_due`] | A paused schedule is never due; pausing moves the revision only on a change | Conditional write |
//! | [`schedules_survive_a_restart`] | Schedules and revisions read back after a restart | Durable storage |
//!
//! Each case deletes the schedules it creates, and its fire times lie far from other
//! cases' times, so a harness may point every case at one shared database.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;

use super::unique_unicode;
use crate::run_store::NewRun;
use crate::schedule::{
    MissedFirePolicy, SCHEDULE_NOT_FOUND_CODE, Schedule, ScheduleSpec, ScheduleStore,
};

/// Opens a [`ScheduleStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::ScheduleStoreHarness;
/// use adk_core::{InMemoryScheduleStore, ScheduleStore, async_trait};
///
/// struct InMemory;
///
/// #[async_trait]
/// impl ScheduleStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ScheduleStore> {
///         Arc::new(InMemoryScheduleStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait ScheduleStoreHarness: Send + Sync {
    /// Opens the store.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. Panics when the backend
    /// cannot be opened.
    async fn open(&self) -> Arc<dyn ScheduleStore>;

    /// Whether data written through one handle is visible through the next.
    ///
    /// `false` skips the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A distant year per case, so `due` in one case never reaches another case's schedules.
fn year(year: i32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, 6, 1, 12, 0, 0).single().expect("valid instant")
}

fn schedule(id: &str, next_fire_at: DateTime<Utc>) -> Schedule {
    Schedule::new(
        id,
        ScheduleSpec::Every(Duration::from_secs(3600)),
        NewRun::new(unique_unicode("queue"), "conformance-app", "system", "agent", json!({})),
        MissedFirePolicy::FireOnce,
    )
    .with_next_fire_at(next_fire_at)
}

async fn cleanup(store: &dyn ScheduleStore, ids: &[&str]) {
    for id in ids {
        store.delete(id).await.expect("delete");
    }
}

/// Inserting sets revision 1 and reads back unchanged; upserting the identical
/// definition changes nothing; changing it moves the revision; delete removes it; and
/// unknown ids fail with `schedule.not_found`.
pub async fn upsert_is_idempotent<H: ScheduleStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let id = unique_unicode("schedule");
    let definition = schedule(&id, year(2030));

    let stored = store.upsert(definition.clone()).await.expect("insert");
    assert_eq!(stored, Schedule { revision: 1, ..definition.clone() });
    assert_eq!(store.get(&id).await.expect("get"), Some(stored.clone()));
    let again = store.upsert(definition.clone()).await.expect("identical upsert");
    assert_eq!(again, stored, "an identical upsert changes nothing");

    let changed = Schedule {
        spec: ScheduleSpec::Cron { expr: "0 9 * * *".into(), timezone: "Europe/Berlin".into() },
        ..definition.clone()
    };
    let updated = store.upsert(changed.clone()).await.expect("update");
    assert_eq!(updated, Schedule { revision: 2, ..changed });
    let listed = store.list().await.expect("list");
    assert_eq!(listed.iter().filter(|s| s.id == id).count(), 1);

    let invalid = store.upsert(schedule("", year(2030))).await;
    assert!(invalid.is_err(), "an empty id is refused");

    store.delete(&id).await.expect("delete");
    assert_eq!(store.get(&id).await.expect("get"), None);
    let error = store.delete(&id).await.expect_err("a second delete");
    assert_eq!(error.code, SCHEDULE_NOT_FOUND_CODE, "{error}");
    let error = store.set_paused(&id, true).await.expect_err("unknown id");
    assert_eq!(error.code, SCHEDULE_NOT_FOUND_CODE, "{error}");
    let error = store.advance(&id, 1, None).await.expect_err("unknown id");
    assert_eq!(error.code, SCHEDULE_NOT_FOUND_CODE, "{error}");
}

/// `due` returns unpaused schedules whose next fire time is at or before `now`, earliest
/// first, and never more than `limit`; future schedules and schedules without a next
/// fire time are not due.
pub async fn due_orders_by_fire_time<H: ScheduleStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let base = year(1980);
    let [late, early, middle, future, done] = [0, 1, 2, 3, 4].map(|_| unique_unicode("schedule"));
    store.upsert(schedule(&late, base + chrono::Duration::minutes(30))).await.expect("upsert");
    store.upsert(schedule(&early, base)).await.expect("upsert");
    store.upsert(schedule(&middle, base + chrono::Duration::minutes(10))).await.expect("upsert");
    store.upsert(schedule(&future, base + chrono::Duration::days(1))).await.expect("upsert");
    let without_next = Schedule { next_fire_at: None, ..schedule(&done, base) };
    store.upsert(without_next).await.expect("upsert");

    let now = base + chrono::Duration::hours(1);
    let ours = [&late, &early, &middle, &future, &done];
    let due: Vec<String> = store
        .due(now, 10_000)
        .await
        .expect("due")
        .into_iter()
        .map(|s| s.id)
        .filter(|id| ours.contains(&id))
        .collect();
    assert_eq!(due, [early.clone(), middle.clone(), late.clone()], "earliest first");
    assert_eq!(store.due(now, 1).await.expect("due").len(), 1, "limit applies");
    let none: Vec<_> = store
        .due(base - chrono::Duration::seconds(1), 10_000)
        .await
        .expect("due")
        .into_iter()
        .filter(|s| ours.contains(&&s.id))
        .collect();
    assert!(none.is_empty(), "nothing is due before its time");

    cleanup(&*store, &[&late, &early, &middle, &future, &done]).await;
}

/// Concurrent advances with the same expected revision, across two handles, succeed
/// exactly once; an advance with a stale revision returns `false` and changes nothing.
pub async fn advance_has_one_winner<H: ScheduleStoreHarness + ?Sized>(harness: &H) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { first.clone() };
    let id = unique_unicode("schedule");
    let stored = first.upsert(schedule(&id, year(1985))).await.expect("upsert");

    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let id = id.clone();
            let next = year(1985) + chrono::Duration::minutes(i64::from(i) + 1);
            tokio::spawn(async move { store.advance(&id, stored.revision, Some(next)).await })
        })
        .collect();
    let mut winners = 0;
    for task in tasks {
        if task.await.expect("advance task completes").expect("advance") {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "exactly one replica advances the schedule");

    let advanced = first.get(&id).await.expect("get").expect("exists");
    assert_eq!(advanced.revision, stored.revision + 1);
    assert!(advanced.next_fire_at > stored.next_fire_at);
    assert!(!first.advance(&id, stored.revision, None).await.expect("advance"), "stale revision");
    assert_eq!(first.get(&id).await.expect("get"), Some(advanced.clone()));
    assert!(first.advance(&id, advanced.revision, None).await.expect("advance"));
    assert_eq!(first.get(&id).await.expect("get").and_then(|s| s.next_fire_at), None);

    cleanup(&*first, &[&id]).await;
}

/// A paused schedule is never due; pausing and resuming move the revision only when the
/// flag changes.
pub async fn paused_schedules_are_not_due<H: ScheduleStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let base = year(1990);
    let id = unique_unicode("schedule");
    let stored = store.upsert(schedule(&id, base)).await.expect("upsert");
    let is_due = |due: Vec<Schedule>| due.iter().any(|s| s.id == id);
    let now = base + chrono::Duration::minutes(1);
    assert!(is_due(store.due(now, 10_000).await.expect("due")));

    let paused = store.set_paused(&id, true).await.expect("pause");
    assert_eq!(paused, Schedule { paused: true, revision: stored.revision + 1, ..stored.clone() });
    assert!(!is_due(store.due(now, 10_000).await.expect("due")), "paused is not due");
    let unchanged = store.set_paused(&id, true).await.expect("pause again");
    assert_eq!(unchanged.revision, paused.revision, "no change, no new revision");

    let resumed = store.set_paused(&id, false).await.expect("resume");
    assert_eq!(resumed.revision, paused.revision + 1);
    assert!(is_due(store.due(now, 10_000).await.expect("due")));

    cleanup(&*store, &[&id]).await;
}

/// Schedules and their revisions read back unchanged after a restart, and the advance
/// CAS still applies.
pub async fn schedules_survive_a_restart<H: ScheduleStoreHarness + ?Sized>(harness: &H) {
    if !harness.is_durable() {
        return;
    }
    let id = unique_unicode("schedule");
    let stored = {
        let before = harness.open().await;
        before.upsert(schedule(&id, year(1995)).with_paused(true)).await.expect("upsert")
    };
    let after = harness.open().await;
    assert_eq!(after.get(&id).await.expect("get"), Some(stored.clone()));
    assert!(!after.advance(&id, stored.revision + 1, None).await.expect("advance"));
    assert!(after.advance(&id, stored.revision, None).await.expect("advance"));
    cleanup(&*after, &[&id]).await;
}
