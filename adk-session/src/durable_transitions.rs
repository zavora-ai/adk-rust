//! Run state transitions shared by the MongoDB, Neo4j, and Firestore run stores.
//!
//! Those stores read a [`RunRecord`], apply one of these transitions in process, and
//! write the result back under their own atomic primitive (a revision-filtered replace,
//! or a transaction). Keeping the transitions in one place makes every backend follow
//! `adk_core::InMemoryRunStore` exactly.

use adk_core::Result;
use adk_core::run_store::{
    RunCheckpointRef, RunOutcome, RunRecord, RunState, run_stale_epoch_error,
};
use chrono::{DateTime, Utc};

use crate::durable_common::stored_now;

/// When the run next becomes claimable, in microseconds: `i64::MIN` for a queued run
/// without a delay, `i64::MAX` for a finished run or one waiting for a signal.
pub(crate) fn ready_micros(record: &RunRecord) -> i64 {
    let micros = |at: Option<DateTime<Utc>>, none: i64| at.map_or(none, |at| at.timestamp_micros());
    match record.state {
        RunState::Queued => micros(record.not_before, i64::MIN),
        RunState::Waiting => micros(record.not_before, i64::MAX),
        RunState::Running => micros(record.lease_expires_at, i64::MIN),
        _ => i64::MAX,
    }
}

/// What a claim does with a candidate, before the concurrency key is considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimStep {
    /// Not claimable at this time.
    NotReady,
    /// Claimable; `started` is 1 when the claim starts a new attempt.
    Claim { started: u32 },
    /// A recovery past `max_attempts`: the run fails instead.
    Exhaust,
}

/// Decides what a claim at `now` does with `record`, ignoring its concurrency key.
pub(crate) fn claim_step(record: &RunRecord, now: DateTime<Utc>) -> ClaimStep {
    match record.state {
        RunState::Queued if record.not_before.is_none_or(|at| at <= now) => {
            ClaimStep::Claim { started: 1 }
        }
        RunState::Waiting if record.not_before.is_some_and(|at| at <= now) => {
            ClaimStep::Claim { started: 0 }
        }
        RunState::Running if record.lease_expires_at.is_none_or(|at| at <= now) => {
            if record.attempt >= record.max_attempts {
                ClaimStep::Exhaust
            } else {
                ClaimStep::Claim { started: 1 }
            }
        }
        _ => ClaimStep::NotReady,
    }
}

fn touch(record: &mut RunRecord, now: DateTime<Utc>) {
    record.updated_at = now;
    record.revision += 1;
}

fn release_lease(record: &mut RunRecord) {
    record.lease_owner = None;
    record.lease_expires_at = None;
}

/// Claims `record` for `worker` until `expires_at`, returning the new epoch.
pub(crate) fn apply_claim(
    record: &mut RunRecord,
    started: u32,
    worker: &str,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> u64 {
    record.state = RunState::Running;
    record.attempt += started;
    record.lease_epoch += 1;
    record.lease_owner = Some(worker.to_string());
    record.lease_expires_at = Some(expires_at);
    record.heartbeat_at = Some(now);
    record.not_before = None;
    touch(record, now);
    record.lease_epoch
}

/// Fails a run whose lease expired after its last allowed attempt.
pub(crate) fn apply_exhaust(record: &mut RunRecord, now: DateTime<Utc>) {
    record.state = RunState::Failed;
    record.error = Some(format!(
        "attempts exhausted: the lease of attempt {} of {} expired",
        record.attempt, record.max_attempts
    ));
    release_lease(record);
    touch(record, now);
}

/// Refuses a worker write unless `record` is `Running` at `epoch`.
pub(crate) fn check_fence(record: &RunRecord, epoch: u64) -> Result<()> {
    if record.state != RunState::Running || record.lease_epoch != epoch {
        return Err(run_stale_epoch_error(&record.run_id, epoch));
    }
    Ok(())
}

/// Extends the lease; the caller has checked the fence.
pub(crate) fn apply_heartbeat(
    record: &mut RunRecord,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) {
    record.lease_expires_at = Some(expires_at);
    record.heartbeat_at = Some(now);
    touch(record, now);
}

/// Records a checkpoint; the caller has checked the fence.
pub(crate) fn apply_checkpoint(
    record: &mut RunRecord,
    now: DateTime<Utc>,
    checkpoint: RunCheckpointRef,
) {
    record.checkpoint = Some(checkpoint);
    touch(record, now);
}

/// Parks the run; the caller has checked the fence.
pub(crate) fn apply_wait(record: &mut RunRecord, now: DateTime<Utc>, until: Option<DateTime<Utc>>) {
    record.state = RunState::Waiting;
    record.not_before = if record.wake_pending { Some(now) } else { until.map(stored_now) };
    record.wake_pending = false;
    release_lease(record);
    touch(record, now);
}

/// Wakes a waiting run or records a pending wake; returns whether anything changed.
pub(crate) fn apply_wake(record: &mut RunRecord, now: DateTime<Utc>) -> bool {
    match record.state {
        RunState::Waiting => record.not_before = Some(now),
        RunState::Running => record.wake_pending = true,
        _ => return false,
    }
    touch(record, now);
    true
}

/// Ends the attempt; the caller has checked the fence.
pub(crate) fn apply_finish(record: &mut RunRecord, now: DateTime<Utc>, outcome: RunOutcome) {
    match outcome {
        RunOutcome::Completed(result) => {
            record.state = RunState::Completed;
            record.result = Some(result);
        }
        RunOutcome::Failed { error, retryable } => {
            let retry =
                retryable && record.attempt < record.max_attempts && !record.cancel_requested;
            record.state = if retry { RunState::Queued } else { RunState::Failed };
            record.error = Some(error);
        }
        RunOutcome::Cancelled => record.state = RunState::Cancelled,
    }
    record.wake_pending = false;
    release_lease(record);
    touch(record, now);
}

/// Cancels a queued or waiting run, or flags a running one; returns whether anything
/// changed. `Interrupt` applies the same change to every unfinished key holder.
pub(crate) fn apply_cancel(record: &mut RunRecord, now: DateTime<Utc>) -> bool {
    match record.state {
        RunState::Queued | RunState::Waiting => {
            record.state = RunState::Cancelled;
            release_lease(record);
        }
        RunState::Running => record.cancel_requested = true,
        _ => return false,
    }
    touch(record, now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::run_store::NewRun;
    use chrono::TimeZone;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).single().unwrap()
    }

    fn record() -> RunRecord {
        RunRecord::queued("r1", NewRun::new("q", "a", "u", "t", serde_json::Value::Null), at(0))
    }

    #[test]
    fn claims_follow_the_in_memory_lifecycle() {
        let mut run = record();
        assert_eq!(claim_step(&run, at(0)), ClaimStep::Claim { started: 1 });
        let epoch = apply_claim(&mut run, 1, "w", at(0), at(30));
        assert_eq!((epoch, run.attempt, run.revision), (1, 1, 2));
        assert_eq!(claim_step(&run, at(29)), ClaimStep::NotReady);
        assert_eq!(claim_step(&run, at(30)), ClaimStep::Claim { started: 1 });
        run.attempt = run.max_attempts;
        assert_eq!(claim_step(&run, at(30)), ClaimStep::Exhaust);
        assert_eq!(ready_micros(&run), at(30).timestamp_micros());
    }

    #[test]
    fn waits_keep_an_early_wake() {
        let mut run = record();
        apply_claim(&mut run, 1, "w", at(0), at(30));
        assert!(apply_wake(&mut run, at(1)));
        apply_wait(&mut run, at(2), None);
        assert_eq!((run.state, run.not_before), (RunState::Waiting, Some(at(2))));
        assert_eq!(claim_step(&run, at(2)), ClaimStep::Claim { started: 0 });
        apply_wait(&mut run, at(3), None);
        assert_eq!(ready_micros(&run), i64::MAX);
    }
}
