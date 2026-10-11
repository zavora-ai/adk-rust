//! Conformance cases for [`DurableRunStore`] backends.
//!
//! | Case | Contract | Backend primitive |
//! |------|----------|-------------------|
//! | [`enqueue_claim_finish_round_trip`] | A run reads back as enqueued, is claimed at attempt 1 and epoch 1, and completes; filters and unknown ids behave | Plain reads and writes |
//! | [`idempotent_enqueue_under_concurrency`] | 32 concurrent enqueues of one key across two handles create one run; keys are scoped per queue | Unique index or create-if-absent |
//! | [`on_conflict_policies`] | `Reject`, `Enqueue`, and `Interrupt` treat an unfinished run holding the concurrency key as documented | Read of key holders inside the enqueue transaction |
//! | [`concurrent_claims_have_one_winner`] | 32 concurrent claimers across two handles claim each run exactly once | Row lock, Lua, `findOneAndUpdate`, or a transaction |
//! | [`not_before_is_respected`] | A delayed run is not claimed before its time | Indexed `not_before` filter |
//! | [`expired_lease_recovery`] | A run whose lease expired is reclaimed at the next attempt and epoch; exhausted attempts fail it | Expiry compared inside the claim |
//! | [`stale_epoch_writes_are_refused`] | `heartbeat`, `set_checkpoint`, `wait`, and `finish` with a stale epoch fail with `run_store.stale_epoch`; unknown runs fail with `run_store.not_found` | Epoch compared inside the conditional write |
//! | [`retryable_failure_requeues_until_max_attempts`] | A retryable failure requeues while attempts remain; others are terminal | Conditional write |
//! | [`wait_and_wake`] | `wait` parks a run until its timer or `wake`; a wake that arrives first is not lost | Conditional write |
//! | [`cancel_is_visible_through_heartbeat`] | Cancelling a running run is reported by the next heartbeat; queued runs cancel at once | Conditional write |
//! | [`prune_removes_old_finished_runs`] | Prune deletes finished runs older than the cut-off, keeps the rest, and frees their idempotency keys | Range delete |
//! | [`runs_survive_a_restart`] | Runs, leases, and checkpoints read back after a restart, and fencing still holds | Durable storage |
//!
//! Queue names, keys, and identifiers contain `:`, `/`, and non-ASCII characters, so a
//! backend that builds keys from them is checked for escaping.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;

use super::{base_time, unique_unicode};
use crate::principal::{ActingContext, AgentId, AgentPrincipal, SubjectRef};
use crate::run_store::{
    ClaimedRun, DurableRunStore, Enqueued, NewRun, OnConflict, RUN_NOT_FOUND_CODE,
    RUN_STALE_EPOCH_CODE, RunCheckpointRef, RunFilter, RunOutcome, RunRecord, RunState,
};
use crate::{Clock, ManualClock};

/// Opens a [`DurableRunStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::RunStoreHarness;
/// use adk_core::{Clock, DurableRunStore, InMemoryRunStore, async_trait};
///
/// struct InMemory;
///
/// #[async_trait]
/// impl RunStoreHarness for InMemory {
///     async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
///         Arc::new(InMemoryRunStore::new().with_clock(clock))
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait RunStoreHarness: Send + Sync {
    /// Opens a store that reads `clock`.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. Panics when the backend
    /// cannot be opened.
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore>;

    /// Whether data written through one handle is visible through the next.
    ///
    /// `false` skips the restart case and runs the concurrency cases on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

const TTL: Duration = Duration::from_secs(30);

fn new_run(queue: &str) -> NewRun {
    NewRun::new(queue, "conformance-app", "user:ü/1", "agent", json!({ "text": "héllo" }))
}

async fn two_handles<H: RunStoreHarness + ?Sized>(
    harness: &H,
    clock: &ManualClock,
) -> (Arc<dyn DurableRunStore>, Arc<dyn DurableRunStore>) {
    let first = harness.open(Arc::new(clock.clone())).await;
    let second = if harness.is_durable() {
        harness.open(Arc::new(clock.clone())).await
    } else {
        first.clone()
    };
    (first, second)
}

async fn created(store: &dyn DurableRunStore, run: NewRun) -> RunRecord {
    match store.enqueue(run).await.expect("enqueue") {
        Enqueued::Created(record) => record,
        other => panic!("expected a new run, got {other:?}"),
    }
}

async fn claim(store: &dyn DurableRunStore, queue: &str, worker: &str) -> Option<ClaimedRun> {
    store.claim(queue, worker, TTL).await.expect("claim")
}

async fn get(store: &dyn DurableRunStore, run_id: &str) -> RunRecord {
    store.get(run_id).await.expect("get").expect("the run exists")
}

#[track_caller]
fn assert_code<T: std::fmt::Debug>(result: crate::Result<T>, code: &str, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.code, code, "{context}: {error}");
    let category = if code == RUN_STALE_EPOCH_CODE {
        crate::ErrorCategory::Conflict
    } else {
        crate::ErrorCategory::NotFound
    };
    assert_eq!(error.category, category, "{context}: {error}");
}

/// A run reads back exactly as enqueued, is claimed at attempt 1 and epoch 1 by one
/// worker, completes with its result, and is listed by every matching filter.
pub async fn enqueue_claim_finish_round_trip<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    assert!(claim(&*store, &queue, "w1").await.is_none(), "an empty queue has nothing to claim");

    let principal = ActingContext::new(
        AgentPrincipal::new(AgentId::new("urn:adk:agent:conformance/ü").expect("valid id"))
            .with_tenant("tenant:1"),
        "inv-1",
    )
    .on_behalf_of(SubjectRef::human("alice@example.com"));
    let run = new_run(&queue)
        .with_session_id(unique_unicode("session"))
        .with_principal(principal)
        .with_max_attempts(4);
    let record = created(&*store, run.clone()).await;
    let expected = RunRecord::queued(record.run_id.clone(), run, base_time());
    assert_eq!(record, expected);
    assert_eq!(get(&*store, &record.run_id).await, expected);

    clock.advance(Duration::from_secs(1));
    let claimed = claim(&*store, &queue, "w1").await.expect("the run is claimable");
    let running = RunRecord {
        state: RunState::Running,
        attempt: 1,
        lease_owner: Some("w1".into()),
        lease_epoch: 1,
        lease_expires_at: Some(clock.now() + chrono::Duration::seconds(30)),
        heartbeat_at: Some(clock.now()),
        updated_at: clock.now(),
        revision: claimed.record.revision,
        ..expected.clone()
    };
    assert_eq!(claimed, ClaimedRun { record: running.clone(), epoch: 1 });
    assert!(claimed.record.revision > expected.revision, "every change moves the revision");
    assert!(claim(&*store, &queue, "w2").await.is_none(), "a held run is not claimed again");

    let done = store
        .finish(&record.run_id, 1, RunOutcome::Completed(json!({ "answer": 42 })))
        .await
        .expect("finish");
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(done.result, Some(json!({ "answer": 42 })));
    assert_eq!((done.lease_owner, done.lease_expires_at), (None, None));
    assert_eq!(get(&*store, &record.run_id).await.state, RunState::Completed);

    let listed = store
        .list(RunFilter::default().queue(&queue).states([RunState::Completed]))
        .await
        .expect("list");
    assert_eq!(
        listed.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
        [record.run_id.as_str()]
    );
    let none = store
        .list(RunFilter::default().queue(&queue).states([RunState::Queued]))
        .await
        .expect("list");
    assert!(none.is_empty());
    assert_eq!(store.get(&unique_unicode("missing")).await.expect("get"), None);
}

/// Concurrent enqueues of one idempotency key, split across two handles, create exactly
/// one run and return it to every caller; the same key in another queue is another run.
pub async fn idempotent_enqueue_under_concurrency<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let (first, second) = two_handles(harness, &clock).await;
    let queue = unique_unicode("queue");
    let key = unique_unicode("idempotency");

    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let run = new_run(&queue).with_idempotency_key(&key);
            tokio::spawn(async move { store.enqueue(run).await })
        })
        .collect();
    let (mut created_ids, mut existing_ids) = (Vec::new(), Vec::new());
    for task in tasks {
        match task.await.expect("enqueue task completes").expect("enqueue") {
            Enqueued::Created(record) => created_ids.push(record.run_id),
            Enqueued::Existing(record) => existing_ids.push(record.run_id),
            Enqueued::Rejected { blocking_run_id } => panic!("rejected by {blocking_run_id}"),
        }
    }
    assert_eq!(created_ids.len(), 1, "exactly one enqueue creates the run");
    assert_eq!(existing_ids.len(), 31);
    assert!(existing_ids.iter().all(|id| *id == created_ids[0]), "every caller gets the same run");
    let queued = first.list(RunFilter::default().queue(&queue)).await.expect("list");
    assert_eq!(queued.len(), 1);

    let other_queue = unique_unicode("queue");
    let other = created(&*first, new_run(&other_queue).with_idempotency_key(&key)).await;
    assert_ne!(other.run_id, created_ids[0], "keys are unique per queue");
}

/// `Reject` names the oldest unfinished run holding the key; `Enqueue` queues behind it
/// and is claimed only after it finishes; `Interrupt` cancels queued and waiting
/// holders, flags running ones, and queues behind them.
pub async fn on_conflict_policies<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    let key = unique_unicode("thread");
    let keyed = |policy| new_run(&queue).with_concurrency_key(&key, policy);

    let holder = created(&*store, keyed(OnConflict::Enqueue)).await;
    let running = claim(&*store, &queue, "w1").await.expect("holder is claimable");
    assert_eq!(running.record.run_id, holder.run_id);

    clock.advance(Duration::from_secs(1));
    let rejected = store.enqueue(keyed(OnConflict::Reject)).await.expect("enqueue");
    assert_eq!(rejected, Enqueued::Rejected { blocking_run_id: holder.run_id.clone() });

    let behind = created(&*store, keyed(OnConflict::Enqueue)).await;
    let free = created(&*store, new_run(&queue)).await;
    let next = claim(&*store, &queue, "w2").await.expect("an unkeyed run is claimable");
    assert_eq!(next.record.run_id, free.run_id, "the keyed run waits behind its holder");
    assert!(claim(&*store, &queue, "w2").await.is_none(), "the keyed run is still blocked");
    store
        .finish(&free.run_id, next.epoch, RunOutcome::Completed(json!(null)))
        .await
        .expect("finish");

    clock.advance(Duration::from_secs(1));
    let interrupting = created(&*store, keyed(OnConflict::Interrupt)).await;
    assert_eq!(get(&*store, &behind.run_id).await.state, RunState::Cancelled, "queued holder");
    let flagged = get(&*store, &holder.run_id).await;
    assert_eq!((flagged.state, flagged.cancel_requested), (RunState::Running, true));
    assert!(
        store.heartbeat(&holder.run_id, running.epoch, TTL).await.expect("heartbeat"),
        "the running holder sees the cancellation"
    );
    assert!(claim(&*store, &queue, "w2").await.is_none(), "the new run waits for the holder");
    store.finish(&holder.run_id, running.epoch, RunOutcome::Cancelled).await.expect("finish");
    let claimed = claim(&*store, &queue, "w2").await.expect("the interrupting run is next");
    assert_eq!(claimed.record.run_id, interrupting.run_id);

    let after = store.enqueue(keyed(OnConflict::Reject)).await.expect("enqueue");
    assert_eq!(after, Enqueued::Rejected { blocking_run_id: interrupting.run_id.clone() });
}

/// 32 concurrent claimers across two handles claim each of 8 runs exactly once.
pub async fn concurrent_claims_have_one_winner<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let (first, second) = two_handles(harness, &clock).await;
    let queue = unique_unicode("queue");
    let mut enqueued = Vec::new();
    for _ in 0..8 {
        enqueued.push(created(&*first, new_run(&queue)).await.run_id);
    }

    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let queue = queue.clone();
            tokio::spawn(async move { store.claim(&queue, &format!("worker-{i}"), TTL).await })
        })
        .collect();
    let mut claimed = Vec::new();
    for task in tasks {
        if let Some(run) = task.await.expect("claim task completes").expect("claim") {
            assert_eq!(run.epoch, 1);
            claimed.push(run.record.run_id);
        }
    }
    claimed.sort();
    enqueued.sort();
    assert_eq!(claimed, enqueued, "every run is claimed exactly once");
}

/// A run delayed with `not_before` is not claimed before that time and is claimed at it.
pub async fn not_before_is_respected<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    let at = base_time() + chrono::Duration::minutes(5);
    let delayed = created(&*store, new_run(&queue).with_not_before(at)).await;

    assert!(claim(&*store, &queue, "w1").await.is_none(), "before not_before");
    clock.set(at - chrono::Duration::seconds(1));
    assert!(claim(&*store, &queue, "w1").await.is_none(), "a second before not_before");
    clock.set(at);
    let claimed = claim(&*store, &queue, "w1").await.expect("claimable at not_before");
    assert_eq!(claimed.record.run_id, delayed.run_id);
}

/// A run whose lease expired is reclaimed by another worker at the next attempt and
/// epoch, and a recovery past `max_attempts` fails the run instead of claiming it.
pub async fn expired_lease_recovery<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let (first, second) = two_handles(harness, &clock).await;
    let queue = unique_unicode("queue");
    let run = created(&*first, new_run(&queue).with_max_attempts(2)).await;

    let lost = claim(&*first, &queue, "w1").await.expect("claim");
    clock.advance(Duration::from_secs(29));
    assert!(claim(&*second, &queue, "w2").await.is_none(), "the lease is still live");
    clock.advance(Duration::from_secs(2));
    let recovered = claim(&*second, &queue, "w2").await.expect("the expired run is recovered");
    assert_eq!(recovered.record.run_id, run.run_id);
    assert_eq!(
        (recovered.epoch, recovered.record.attempt, recovered.record.lease_owner.as_deref()),
        (lost.epoch + 1, 2, Some("w2"))
    );
    assert_code(
        first.heartbeat(&run.run_id, lost.epoch, TTL).await,
        RUN_STALE_EPOCH_CODE,
        "the first worker lost the run",
    );

    clock.advance(Duration::from_secs(31));
    assert!(claim(&*first, &queue, "w3").await.is_none(), "attempts are exhausted");
    let failed = get(&*first, &run.run_id).await;
    assert_eq!((failed.state, failed.attempt), (RunState::Failed, 2));
    assert!(failed.error.is_some(), "the failure says why");
}

/// Every worker write carrying a stale epoch fails with `run_store.stale_epoch` and
/// changes nothing; every operation on an unknown run fails with `run_store.not_found`.
pub async fn stale_epoch_writes_are_refused<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    let run = created(&*store, new_run(&queue)).await;
    let id = run.run_id.as_str();
    assert_code(store.heartbeat(id, 0, TTL).await, RUN_STALE_EPOCH_CODE, "a queued run");

    let claimed = claim(&*store, &queue, "w1").await.expect("claim");
    let stale = claimed.epoch + 1;
    let checkpoint = RunCheckpointRef::Session { session_id: "s/1".into(), invocation_id: None };
    assert_code(store.heartbeat(id, stale, TTL).await, RUN_STALE_EPOCH_CODE, "heartbeat");
    assert_code(
        store.set_checkpoint(id, stale, checkpoint.clone()).await,
        RUN_STALE_EPOCH_CODE,
        "set_checkpoint",
    );
    assert_code(store.wait(id, stale, None).await, RUN_STALE_EPOCH_CODE, "wait");
    assert_code(
        store.finish(id, stale, RunOutcome::Completed(json!(1))).await,
        RUN_STALE_EPOCH_CODE,
        "finish",
    );
    let unchanged = get(&*store, id).await;
    assert_eq!(unchanged, claimed.record, "refused writes change nothing");

    store.set_checkpoint(id, claimed.epoch, checkpoint.clone()).await.expect("current epoch");
    assert_eq!(get(&*store, id).await.checkpoint, Some(checkpoint));
    store.finish(id, claimed.epoch, RunOutcome::Completed(json!(1))).await.expect("finish");
    assert_code(
        store.finish(id, claimed.epoch, RunOutcome::Completed(json!(2))).await,
        RUN_STALE_EPOCH_CODE,
        "a finished run",
    );
    assert_eq!(get(&*store, id).await.result, Some(json!(1)), "the first finish stands");

    let missing = unique_unicode("missing");
    assert_code(store.heartbeat(&missing, 1, TTL).await, RUN_NOT_FOUND_CODE, "heartbeat");
    assert_code(
        store.finish(&missing, 1, RunOutcome::Cancelled).await,
        RUN_NOT_FOUND_CODE,
        "finish",
    );
    assert_code(store.wake(&missing).await, RUN_NOT_FOUND_CODE, "wake");
    assert_code(store.request_cancel(&missing).await, RUN_NOT_FOUND_CODE, "request_cancel");
}

/// A retryable failure requeues the run while attempts remain, the next claim starts the
/// next attempt, and the last attempt's failure is terminal; a non-retryable failure is
/// terminal at once.
pub async fn retryable_failure_requeues_until_max_attempts<H: RunStoreHarness + ?Sized>(
    harness: &H,
) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    let run = created(&*store, new_run(&queue).with_max_attempts(2)).await;
    let failure = |error: &str, retryable| RunOutcome::Failed { error: error.into(), retryable };

    let first = claim(&*store, &queue, "w1").await.expect("claim");
    let requeued =
        store.finish(&run.run_id, first.epoch, failure("timeout", true)).await.expect("finish");
    assert_eq!(
        (requeued.state, requeued.attempt, requeued.error.as_deref()),
        (RunState::Queued, 1, Some("timeout"))
    );
    let second = claim(&*store, &queue, "w2").await.expect("the retry is claimable");
    assert_eq!((second.record.attempt, second.epoch), (2, first.epoch + 1));
    let failed =
        store.finish(&run.run_id, second.epoch, failure("timeout", true)).await.expect("finish");
    assert_eq!(failed.state, RunState::Failed, "no attempts left");
    assert!(claim(&*store, &queue, "w3").await.is_none());

    let fatal = created(&*store, new_run(&queue).with_max_attempts(5)).await;
    let claimed = claim(&*store, &queue, "w1").await.expect("claim");
    let failed = store
        .finish(&fatal.run_id, claimed.epoch, failure("bad input", false))
        .await
        .expect("finish");
    assert_eq!(failed.state, RunState::Failed, "non-retryable failures are terminal");
}

/// `wait` releases the lease and parks the run until its timer passes, at the same
/// attempt and the next epoch; `wake` makes a parked run claimable at once; a wake that
/// arrives while the run is still running is kept until it parks.
pub async fn wait_and_wake<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    let run = created(&*store, new_run(&queue)).await;
    let id = run.run_id.as_str();

    let claimed = claim(&*store, &queue, "w1").await.expect("claim");
    let until = base_time() + chrono::Duration::minutes(10);
    store.wait(id, claimed.epoch, Some(until)).await.expect("wait");
    let parked = get(&*store, id).await;
    assert_eq!((parked.state, parked.lease_owner.as_deref()), (RunState::Waiting, None));
    assert_code(store.heartbeat(id, claimed.epoch, TTL).await, RUN_STALE_EPOCH_CODE, "parked");
    clock.advance(Duration::from_secs(60));
    assert!(claim(&*store, &queue, "w2").await.is_none(), "the timer has not passed");
    clock.set(until);
    let resumed = claim(&*store, &queue, "w2").await.expect("the timer passed");
    assert_eq!((resumed.record.attempt, resumed.epoch), (1, claimed.epoch + 1));

    store.wait(id, resumed.epoch, None).await.expect("wait for a signal");
    clock.advance(Duration::from_secs(3600));
    assert!(claim(&*store, &queue, "w3").await.is_none(), "parked until woken");
    store.wake(id).await.expect("wake");
    let woken = claim(&*store, &queue, "w3").await.expect("woken");
    assert_eq!((woken.record.attempt, woken.epoch), (1, resumed.epoch + 1));

    store.wake(id).await.expect("a wake while running is kept");
    store.wait(id, woken.epoch, None).await.expect("wait");
    let early = claim(&*store, &queue, "w4").await.expect("the earlier wake applies");
    assert_eq!(early.record.run_id, run.run_id);
    store.finish(id, early.epoch, RunOutcome::Completed(json!(null))).await.expect("finish");
    store.wake(id).await.expect("waking a finished run is a no-op");
    assert_eq!(get(&*store, id).await.state, RunState::Completed);
}

/// Cancelling a running run is reported by its next heartbeat and blocks a retry;
/// queued and waiting runs cancel at once; a finished run is unchanged.
pub async fn cancel_is_visible_through_heartbeat<H: RunStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");

    let running = created(&*store, new_run(&queue)).await;
    let claimed = claim(&*store, &queue, "w1").await.expect("claim");
    assert!(!store.heartbeat(&running.run_id, claimed.epoch, TTL).await.expect("heartbeat"));
    store.request_cancel(&running.run_id).await.expect("request_cancel");
    assert!(store.heartbeat(&running.run_id, claimed.epoch, TTL).await.expect("heartbeat"));
    let failed = store
        .finish(
            &running.run_id,
            claimed.epoch,
            RunOutcome::Failed { error: "stopped".into(), retryable: true },
        )
        .await
        .expect("finish");
    assert_eq!(failed.state, RunState::Failed, "a cancelled run is not retried");

    let queued = created(&*store, new_run(&queue)).await;
    store.request_cancel(&queued.run_id).await.expect("request_cancel");
    assert_eq!(get(&*store, &queued.run_id).await.state, RunState::Cancelled);
    assert!(claim(&*store, &queue, "w2").await.is_none(), "a cancelled run is not claimed");

    let waiting = created(&*store, new_run(&queue)).await;
    let claimed = claim(&*store, &queue, "w2").await.expect("claim");
    store.wait(&waiting.run_id, claimed.epoch, None).await.expect("wait");
    store.request_cancel(&waiting.run_id).await.expect("request_cancel");
    assert_eq!(get(&*store, &waiting.run_id).await.state, RunState::Cancelled);

    let finished = get(&*store, &running.run_id).await;
    store.request_cancel(&running.run_id).await.expect("cancelling a finished run is a no-op");
    assert_eq!(get(&*store, &running.run_id).await, finished);
}

/// Prune deletes finished runs last changed before the cut-off and keeps newer and
/// unfinished ones; a pruned run's idempotency key creates a new run.
pub async fn prune_removes_old_finished_runs<H: RunStoreHarness + ?Sized>(harness: &H) {
    // Long before every other case's base time, so the cut-off never reaches their runs.
    let start = Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).single().expect("valid instant");
    let clock = ManualClock::new(start);
    let store = harness.open(Arc::new(clock.clone())).await;
    let queue = unique_unicode("queue");
    let key = unique_unicode("idempotency");

    let old = created(&*store, new_run(&queue).with_idempotency_key(&key)).await;
    let claimed = claim(&*store, &queue, "w1").await.expect("claim");
    store
        .finish(&old.run_id, claimed.epoch, RunOutcome::Completed(json!(1)))
        .await
        .expect("finish");
    let unfinished = created(&*store, new_run(&queue)).await;

    clock.advance(Duration::from_secs(3600));
    let cutoff: DateTime<Utc> = clock.now();
    let recent = created(&*store, new_run(&queue)).await;
    store.request_cancel(&recent.run_id).await.expect("cancel");

    let pruned = store.prune(cutoff).await.expect("prune");
    assert!(pruned >= 1, "the old finished run is pruned");
    assert_eq!(store.get(&old.run_id).await.expect("get"), None);
    assert!(store.get(&unfinished.run_id).await.expect("get").is_some(), "unfinished runs stay");
    assert!(store.get(&recent.run_id).await.expect("get").is_some(), "newer finished runs stay");

    let again = store.enqueue(new_run(&queue).with_idempotency_key(&key)).await.expect("enqueue");
    assert!(matches!(again, Enqueued::Created(_)), "the pruned run's key is free: {again:?}");
}

/// Runs, leases, and checkpoints written before a restart read back after it, the old
/// epoch still works, and a stale one is still refused.
pub async fn runs_survive_a_restart<H: RunStoreHarness + ?Sized>(harness: &H) {
    if !harness.is_durable() {
        return;
    }
    let clock = ManualClock::new(base_time());
    let queue = unique_unicode("queue");
    let checkpoint = RunCheckpointRef::Graph {
        thread_id: "thread/ü".into(),
        checkpoint_id: Some("c:1".into()),
    };
    let (record, claimed) = {
        let before = harness.open(Arc::new(clock.clone())).await;
        let record = created(&*before, new_run(&queue).with_idempotency_key("restart")).await;
        let claimed = claim(&*before, &queue, "w1").await.expect("claim");
        before
            .set_checkpoint(&record.run_id, claimed.epoch, checkpoint.clone())
            .await
            .expect("checkpoint");
        (record, claimed)
    };

    let after = harness.open(Arc::new(clock.clone())).await;
    let read = get(&*after, &record.run_id).await;
    assert_eq!(read.checkpoint, Some(checkpoint));
    assert_eq!((read.state, read.lease_epoch), (RunState::Running, claimed.epoch));
    let existing =
        after.enqueue(new_run(&queue).with_idempotency_key("restart")).await.expect("enqueue");
    assert_eq!(existing.record().map(|r| r.run_id.as_str()), Some(record.run_id.as_str()));
    assert_code(
        after.heartbeat(&record.run_id, claimed.epoch + 1, TTL).await,
        RUN_STALE_EPOCH_CODE,
        "fencing holds after a restart",
    );
    after.heartbeat(&record.run_id, claimed.epoch, TTL).await.expect("the epoch survives");
}
