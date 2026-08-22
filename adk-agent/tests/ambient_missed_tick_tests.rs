//! A cron schedule must not silently drop runs that came due while it was not watching.
//!
//! `CronTrigger::subscribe` computes the next tick from the moment it is called, so a trigger
//! that restarts after downtime resumes at the next future tick and every elapsed tick is lost
//! with no record. On a host that suspends — a laptop — that is most ticks.
//!
//! These tests simulate a restart by seeding a watermark in the past, then asserting what the
//! freshly subscribed trigger replays. They use a one-minute schedule so nothing fires on its own
//! within the test window: any event that arrives promptly is a catch-up event.

#![cfg(feature = "ambient")]

use std::sync::Arc;
use std::time::Duration;

use adk_agent::ambient::{
    CronTrigger, EventSource, FileTickWatermark, MissedTickPolicy, TickWatermark,
};
use adk_core::{AdkError, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use tokio::time::timeout;

/// Long enough that a catch-up event (emitted without waiting) arrives, short enough that a
/// genuine one-minute tick cannot.
const PROMPT: Duration = Duration::from_secs(5);

/// Seeds a watermark `minutes_ago` in the past, standing in for a process that was down that long.
async fn watermark_from_the_past(
    dir: &tempfile::TempDir,
    minutes_ago: i64,
) -> Arc<dyn TickWatermark> {
    let watermark: Arc<dyn TickWatermark> =
        Arc::new(FileTickWatermark::new(dir.path().join("sweep.tick")));
    let last_tick = Utc::now() - chrono::Duration::minutes(minutes_ago);
    watermark.write(last_tick).await.expect("seed watermark");
    watermark
}

#[tokio::test]
async fn coalesce_one_replays_a_gap_as_a_single_immediate_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let watermark = watermark_from_the_past(&dir, 5).await;

    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
        .with_watermark(watermark);

    let mut stream = trigger.subscribe().await.expect("subscribe");
    let event = timeout(PROMPT, stream.next())
        .await
        .expect("a catch-up event should not wait for the next scheduled tick")
        .expect("stream yielded an event");

    assert_eq!(
        event.payload["catch_up"], true,
        "the first event after a gap is a replay, not a fresh tick"
    );
    assert!(
        event.payload["missed_count"].as_u64().expect("missed_count") >= 4,
        "a five-minute gap on a one-minute schedule missed at least four ticks, got {}",
        event.payload["missed_count"]
    );
    assert_eq!(event.payload["missed_count_truncated"], false);
}

#[tokio::test]
async fn all_replays_one_event_per_missed_tick() {
    let dir = tempfile::tempdir().expect("tempdir");
    let watermark = watermark_from_the_past(&dir, 3).await;

    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::All)
        .with_watermark(watermark);

    let mut stream = trigger.subscribe().await.expect("subscribe");

    // A three-minute gap crosses at least two minute boundaries regardless of where in the
    // minute the test starts.
    for index in 0..2 {
        let event = timeout(PROMPT, stream.next())
            .await
            .unwrap_or_else(|_| panic!("catch-up event {index} should arrive promptly"))
            .expect("stream yielded an event");

        assert_eq!(event.payload["catch_up"], true);
        assert!(
            event.payload["scheduled_for"].is_string(),
            "each replayed tick reports the time it was scheduled for"
        );
    }
}

#[tokio::test]
async fn all_respects_the_catch_up_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let watermark = watermark_from_the_past(&dir, 90).await;

    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::All)
        .with_max_catch_up(3)
        .with_watermark(Arc::clone(&watermark));

    let mut stream = trigger.subscribe().await.expect("subscribe");

    for index in 0..3 {
        timeout(PROMPT, stream.next())
            .await
            .unwrap_or_else(|_| panic!("capped replay {index} should arrive promptly"))
            .expect("stream yielded an event");
    }

    // The cap is a hard bound: a ninety-minute gap must not replay a fourth event. If this test
    // happens to cross the next minute boundary, a fresh scheduled tick is still valid.
    if let Ok(Some(event)) = timeout(PROMPT, stream.next()).await {
        assert_eq!(
            event.payload["catch_up"], false,
            "replay must stop at the cap rather than draining the whole gap"
        );
    }

    let accounted_through = watermark.read().await.expect("read").expect("cursor");
    assert!(
        Utc::now() - accounted_through < chrono::Duration::minutes(1),
        "discarding a truncated remainder must also advance the durable cursor: {accounted_through}"
    );

    drop(stream);
    let restarted = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::All)
        .with_max_catch_up(3)
        .with_watermark(watermark);
    let mut restarted_stream = restarted.subscribe().await.expect("resubscribe");
    if let Ok(Some(event)) = timeout(Duration::from_millis(200), restarted_stream.next()).await {
        assert_eq!(
            event.payload["catch_up"], false,
            "a restart must not recover the deliberately discarded remainder"
        );
    }
}

#[tokio::test]
async fn skip_ignores_the_gap_entirely() {
    let dir = tempfile::tempdir().expect("tempdir");
    let watermark = watermark_from_the_past(&dir, 30).await;

    // Skip is the default; state it explicitly to pin the behaviour under test.
    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::Skip)
        .with_watermark(watermark);

    let mut stream = trigger.subscribe().await.expect("subscribe");

    if let Ok(Some(event)) = timeout(PROMPT, stream.next()).await {
        assert_eq!(
            event.payload["catch_up"], false,
            "Skip may emit a fresh scheduled tick, but never replays the stale gap"
        );
    }
}

#[tokio::test]
async fn a_policy_without_a_watermark_sees_no_gap_across_restarts() {
    // Nothing recorded where the schedule left off, so the trigger starts from now and there is
    // no gap to replay. This is why the policy and the watermark are documented as a pair.
    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::CoalesceOne);

    let mut stream = trigger.subscribe().await.expect("subscribe");

    if let Ok(Some(event)) = timeout(PROMPT, stream.next()).await {
        assert_eq!(
            event.payload["catch_up"], false,
            "without a watermark a fresh tick is valid, but no gap can be replayed"
        );
    }
}

#[tokio::test]
async fn a_replayed_tick_advances_the_watermark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let watermark = watermark_from_the_past(&dir, 5).await;
    let seeded = watermark.read().await.expect("read").expect("seeded");

    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
        .with_watermark(Arc::clone(&watermark));

    let mut stream = trigger.subscribe().await.expect("subscribe");
    timeout(PROMPT, stream.next()).await.expect("catch-up event").expect("stream yielded an event");

    let advanced = watermark.read().await.expect("read").expect("still present");
    assert!(
        advanced > seeded,
        "replaying a gap must move the watermark forward so a second restart does not replay it \
         again: seeded {seeded}, advanced {advanced}"
    );
}

struct FailingWriteWatermark;

#[async_trait]
impl TickWatermark for FailingWriteWatermark {
    async fn read(&self) -> Result<Option<chrono::DateTime<Utc>>> {
        Ok(Some(Utc::now() - chrono::Duration::minutes(5)))
    }

    async fn write(&self, _cursor: chrono::DateTime<Utc>) -> Result<()> {
        Err(AdkError::agent("simulated watermark failure"))
    }
}

#[tokio::test]
async fn persistence_failure_stops_before_emitting_a_tick() {
    let trigger = CronTrigger::new("0 * * * * *")
        .expect("valid expression")
        .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
        .with_watermark(Arc::new(FailingWriteWatermark));
    let mut stream = trigger.subscribe().await.expect("subscribe");

    assert!(
        timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("stream should stop promptly")
            .is_none(),
        "a tick must not be emitted after its durable cursor failed to persist"
    );
}
