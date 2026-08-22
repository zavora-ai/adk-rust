use std::str::FromStr;
use std::sync::Arc;

use adk_core::{AdkError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cron::Schedule;
use futures::stream::BoxStream;
use tokio::time::sleep;

use super::event_source::{EventSource, TriggerEvent};
use super::watermark::TickWatermark;

/// Ticks a catch-up pass will replay before the remainder of the gap is discarded.
const DEFAULT_MAX_CATCH_UP: usize = 64;

/// What a [`CronTrigger`] does about ticks that came due while it was not watching.
///
/// A schedule can fall behind for three reasons: the process was not running, the host
/// suspended, or the consumer took longer than the interval to accept the previous event. In
/// every case the trigger resumes at the next future tick, so without a policy the elapsed ticks
/// are dropped with no record that anything was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissedTickPolicy {
    /// Discard elapsed ticks and wait for the next scheduled one.
    ///
    /// The default, and the behaviour of every release before this policy existed.
    #[default]
    Skip,
    /// Emit one event covering the whole elapsed span, then resume.
    ///
    /// Suits sweeps where only the current state matters — a health check does not need to run
    /// once per missed minute, it needs to run once, now.
    CoalesceOne,
    /// Emit one event per elapsed tick, oldest first, then resume.
    ///
    /// Suits schedules where each occurrence has its own work. Bounded by
    /// [`CronTrigger::with_max_catch_up`].
    All,
}

/// Emits trigger events on a cron schedule.
///
/// Uses the `cron` crate for expression parsing and next-tick calculation, which expects a
/// six-field expression (seconds first).
///
/// # Missed ticks
///
/// By default the trigger resumes at the next future tick and elapsed ticks are discarded. Pair
/// [`MissedTickPolicy`] with a [`TickWatermark`] to replay them; the watermark is what makes a
/// gap visible across a process restart.
///
/// # Delivery contract
///
/// The watermark advances when the trigger *emits* a tick, not when the consumer finishes acting
/// on it. A crash between emission and completion therefore drops that run rather than repeating
/// it — at-most-once, not at-least-once. This keeps a consumer that stops polling from replaying
/// the same gap on every restart. Consumers whose work must survive a mid-run crash should record
/// their own completion state. When a configured watermark cannot be persisted, the stream stops
/// before emitting the affected tick rather than violating this contract.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_agent::ambient::{CronTrigger, FileTickWatermark, MissedTickPolicy};
///
/// // Fire every minute, and on restart run one catch-up sweep if ticks were missed.
/// let trigger = CronTrigger::new("0 * * * * *")?
///     .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
///     .with_watermark(Arc::new(FileTickWatermark::new("/var/lib/my-agent/sweep.tick")));
/// ```
pub struct CronTrigger {
    expression: String,
    schedule: Schedule,
    name: String,
    missed_tick_policy: MissedTickPolicy,
    watermark: Option<Arc<dyn TickWatermark>>,
    max_catch_up: usize,
}

impl CronTrigger {
    /// Create a new cron trigger from a cron expression.
    ///
    /// Returns an error if the expression is invalid.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Agent` with the parse error if the expression is invalid.
    pub fn new(expression: &str) -> Result<Self> {
        let schedule = Schedule::from_str(expression)
            .map_err(|e| AdkError::agent(format!("invalid cron expression: {e}")))?;

        Ok(Self {
            expression: expression.to_string(),
            schedule,
            name: format!("cron:{expression}"),
            missed_tick_policy: MissedTickPolicy::default(),
            watermark: None,
            max_catch_up: DEFAULT_MAX_CATCH_UP,
        })
    }

    /// Sets what happens to ticks that came due while the trigger was not watching.
    ///
    /// Defaults to [`MissedTickPolicy::Skip`]. Without a [`TickWatermark`] this only covers gaps
    /// within a single subscription, because nothing records where the schedule left off.
    pub fn with_missed_tick_policy(mut self, policy: MissedTickPolicy) -> Self {
        self.missed_tick_policy = policy;
        self
    }

    /// Stores the most recent tick, so a restarted process can see which runs it missed.
    ///
    /// Has no effect under [`MissedTickPolicy::Skip`].
    pub fn with_watermark(mut self, watermark: Arc<dyn TickWatermark>) -> Self {
        self.watermark = Some(watermark);
        self
    }

    /// Caps how many ticks one catch-up pass replays. Defaults to 64.
    ///
    /// A long outage on a frequent schedule can leave thousands of ticks outstanding; replaying
    /// them all would flood the handler at subscribe time. Once the cap is reached the rest of
    /// the gap is discarded, the durable cursor advances past it, and the trigger resumes at the
    /// next future tick, logging how many ticks were dropped. A cap of zero is treated as one.
    pub fn with_max_catch_up(mut self, max_catch_up: usize) -> Self {
        self.max_catch_up = max_catch_up.max(1);
        self
    }

    /// The configured missed-tick policy.
    pub fn missed_tick_policy(&self) -> MissedTickPolicy {
        self.missed_tick_policy
    }
}

/// Ticks in `(cursor, now]`, capped so a long outage cannot enumerate an unbounded span.
///
/// Returns the ticks found and whether the cap truncated them.
fn elapsed_ticks(
    schedule: &Schedule,
    cursor: DateTime<Utc>,
    now: DateTime<Utc>,
    cap: usize,
) -> (Vec<DateTime<Utc>>, bool) {
    let mut ticks = Vec::new();
    let mut truncated = false;

    for tick in schedule.after(&cursor).take_while(|tick| *tick <= now) {
        if ticks.len() == cap {
            truncated = true;
            break;
        }
        ticks.push(tick);
    }

    (ticks, truncated)
}

#[async_trait]
impl EventSource for CronTrigger {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>> {
        let schedule = self.schedule.clone();
        let source_name = self.name.clone();
        let expression = self.expression.clone();
        let policy = self.missed_tick_policy;
        let watermark = self.watermark.clone();
        let cap = self.max_catch_up;

        // Where the schedule left off. A persisted watermark makes ticks missed while the process
        // was down visible; without one, only drift inside this subscription is observable.
        let restored = match (&watermark, policy) {
            (Some(store), policy) if policy != MissedTickPolicy::Skip => store.read().await?,
            _ => None,
        };

        let stream = async_stream::stream! {
            let mut cursor = restored.unwrap_or_else(Utc::now);

            loop {
                // Account for everything already due before waiting on the next tick. Under
                // `Skip` the span is abandoned deliberately; under the other policies it is
                // replayed. Either way it is never silently unaccounted.
                if policy != MissedTickPolicy::Skip {
                    let now = Utc::now();
                    let (missed, truncated) = elapsed_ticks(&schedule, cursor, now, cap);

                    if !missed.is_empty() {
                        let missed_count = missed.len();

                        if truncated {
                            tracing::warn!(
                                source = %source_name,
                                replayed = missed_count,
                                "catch-up cap reached; discarding the remainder of the missed span"
                            );
                        }

                        match policy {
                            MissedTickPolicy::Skip => unreachable!("guarded above"),
                            MissedTickPolicy::CoalesceOne => {
                                let scheduled_for = missed[missed_count - 1];
                                tracing::info!(
                                    source = %source_name,
                                    missed = missed_count,
                                    "replaying missed ticks as one coalesced event"
                                );
                                // Persisted before the yield: `yield` suspends until the consumer
                                // polls again, so writing afterwards would leave the watermark a
                                // tick behind for as long as a consumer holds an event — and
                                // permanently behind if it drops the stream, replaying the same
                                // gap on every restart.
                                let accounted_through =
                                    if truncated { now } else { scheduled_for };
                                if let Some(ref store) = watermark
                                    && let Err(error) = store.write(accounted_through).await
                                {
                                    tracing::error!(%error, "failed to persist tick watermark; stopping cron stream before emission");
                                    return;
                                }
                                yield TriggerEvent {
                                    source: source_name.clone(),
                                    payload: serde_json::json!({
                                        "expression": expression,
                                        "tick": Utc::now().to_rfc3339(),
                                        "scheduled_for": scheduled_for.to_rfc3339(),
                                        "catch_up": true,
                                        "missed_count": missed_count,
                                        "missed_count_truncated": truncated,
                                    }),
                                    // A schedule has no caller.
                                    principal: None,
                                };
                            }
                            MissedTickPolicy::All => {
                                for (index, scheduled_for) in missed.iter().enumerate() {
                                    let accounted_through = if truncated && index + 1 == missed_count {
                                        now
                                    } else {
                                        *scheduled_for
                                    };
                                    if let Some(ref store) = watermark
                                        && let Err(error) = store.write(accounted_through).await
                                    {
                                        tracing::error!(%error, "failed to persist tick watermark; stopping cron stream before emission");
                                        return;
                                    }
                                    yield TriggerEvent {
                                        source: source_name.clone(),
                                        payload: serde_json::json!({
                                            "expression": expression,
                                            "tick": Utc::now().to_rfc3339(),
                                            "scheduled_for": scheduled_for.to_rfc3339(),
                                            "catch_up": true,
                                        }),
                                        principal: None,
                                    };
                                }
                            }
                        }
                    }

                    // Truncating abandons the rest of the gap, so resume from now rather than
                    // re-enumerating it on the next pass.
                    cursor = if truncated { now } else { missed.last().copied().unwrap_or(cursor) };
                } else {
                    cursor = Utc::now();
                }

                let Some(next_tick) = schedule.after(&cursor).next() else {
                    // No more upcoming ticks — schedule is exhausted
                    break;
                };

                let duration = (next_tick - Utc::now()).to_std().unwrap_or_default();
                sleep(duration).await;

                cursor = next_tick;

                if policy != MissedTickPolicy::Skip
                    && let Some(ref store) = watermark
                    && let Err(error) = store.write(next_tick).await
                {
                    tracing::error!(%error, "failed to persist tick watermark; stopping cron stream before emission");
                    return;
                }

                yield TriggerEvent {
                    source: source_name.clone(),
                    payload: serde_json::json!({
                        "expression": expression,
                        "tick": Utc::now().to_rfc3339(),
                        "scheduled_for": next_tick.to_rfc3339(),
                        "catch_up": false,
                    }),
                    // A schedule has no caller.
                    principal: None,
                };
            }
        };

        Ok(Box::pin(stream))
    }
}

impl std::fmt::Debug for CronTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronTrigger")
            .field("expression", &self.expression)
            .field("missed_tick_policy", &self.missed_tick_policy)
            .field("watermark", &self.watermark.is_some())
            .field("max_catch_up", &self.max_catch_up)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn every_minute() -> Schedule {
        Schedule::from_str("0 * * * * *").expect("valid expression")
    }

    #[test]
    fn policy_defaults_to_skip() {
        let trigger = CronTrigger::new("0 * * * * *").expect("valid expression");

        assert_eq!(trigger.missed_tick_policy(), MissedTickPolicy::Skip);
    }

    #[test]
    fn invalid_expressions_are_rejected() {
        assert!(CronTrigger::new("not a cron expression").is_err());
    }

    #[test]
    fn max_catch_up_of_zero_is_treated_as_one() {
        let trigger =
            CronTrigger::new("0 * * * * *").expect("valid expression").with_max_catch_up(0);

        assert_eq!(trigger.max_catch_up, 1);
    }

    #[test]
    fn elapsed_ticks_finds_every_tick_in_the_gap() {
        let cursor = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 10, 5, 0).unwrap();

        let (ticks, truncated) = elapsed_ticks(&every_minute(), cursor, now, 64);

        assert_eq!(ticks.len(), 5, "10:01 through 10:05 inclusive");
        assert!(!truncated);
        assert_eq!(ticks[0], Utc.with_ymd_and_hms(2026, 8, 22, 10, 1, 0).unwrap());
        assert_eq!(ticks[4], now);
    }

    #[test]
    fn elapsed_ticks_is_empty_when_no_tick_has_come_due() {
        let cursor = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 30).unwrap();

        let (ticks, truncated) = elapsed_ticks(&every_minute(), cursor, now, 64);

        assert!(ticks.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn elapsed_ticks_reports_truncation_at_the_cap() {
        let cursor = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 20, 0, 0).unwrap();

        let (ticks, truncated) = elapsed_ticks(&every_minute(), cursor, now, 10);

        assert_eq!(ticks.len(), 10, "capped rather than replaying ten hours");
        assert!(truncated);
    }

    #[test]
    fn elapsed_ticks_excludes_the_cursor_itself() {
        let cursor = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 10, 1, 0).unwrap();

        let (ticks, _) = elapsed_ticks(&every_minute(), cursor, now, 64);

        assert_eq!(ticks, vec![now], "the cursor tick already fired");
    }
}
