//! The frame governor (spec 3.7 §7.9, scenario S11).

use std::collections::VecDeque;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, VisionError};

/// The billed minimum of one `sam-3.1` video request, in frames (spec 3.7 X5).
const SAM_VIDEO_MIN_FRAMES: u64 = 50;

/// What a stream may spend: a local sampling rate, escalations per minute and frame quotas
/// per period (spec 3.7 §7.9).
///
/// | Field | Caps |
/// |-------|------|
/// | `local_fps`, `min_local_fps` | The local rate, and the lowest rate the governor steps down to before it pauses the source |
/// | `local_frames_per_period` | Frames processed locally per period |
/// | `escalations_per_minute` | Escalation requests in any 60 seconds |
/// | `escalated_frames_per_period` | Frames billed for escalations per period, each request at least `min_frames_per_escalation` |
/// | `period_secs` | The quota period; periods are aligned to the Unix epoch, so a day starts at 00:00 UTC |
///
/// # Example
///
/// ```
/// use adk_vision::stream::FrameBudget;
///
/// // S11: local detection at 5 fps, escalations to sam-3.1 video, a daily frame quota.
/// let budget = FrameBudget::new(5.0)
///     .with_min_local_fps(1.0)
///     .with_local_quota(200_000)
///     .with_escalations_per_minute(2)
///     .with_escalation_quota(5_000);
/// assert_eq!(budget.min_frames_per_escalation, 50);
/// assert_eq!(budget.period_secs, 86_400);
/// assert!(budget.validate().is_ok());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameBudget {
    /// The local sampling rate, in frames a second.
    pub local_fps: f32,
    /// The lowest local rate the governor steps down to before it pauses the source.
    pub min_local_fps: f32,
    /// Frames processed locally per period; unlimited when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_frames_per_period: Option<u64>,
    /// Escalation requests in any 60 seconds; unlimited when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalations_per_minute: Option<u32>,
    /// Frames billed for escalations per period; unlimited when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated_frames_per_period: Option<u64>,
    /// The frames one escalation request is billed at least: 50 for `sam-3.1` video.
    pub min_frames_per_escalation: u64,
    /// The quota period in seconds: 86,400, a day, by default.
    pub period_secs: u64,
}

impl FrameBudget {
    /// A budget of `local_fps` with no quotas, a floor of a tenth of that rate, the
    /// `sam-3.1` video minimum of 50 frames per escalation and a one-day period.
    pub fn new(local_fps: f32) -> Self {
        Self {
            local_fps,
            min_local_fps: local_fps / 10.0,
            local_frames_per_period: None,
            escalations_per_minute: None,
            escalated_frames_per_period: None,
            min_frames_per_escalation: SAM_VIDEO_MIN_FRAMES,
            period_secs: 86_400,
        }
    }

    /// Sets the lowest local rate before a pause.
    pub fn with_min_local_fps(mut self, fps: f32) -> Self {
        self.min_local_fps = fps;
        self
    }

    /// Caps the frames processed locally per period.
    pub fn with_local_quota(mut self, frames: u64) -> Self {
        self.local_frames_per_period = Some(frames);
        self
    }

    /// Caps escalation requests per minute.
    pub fn with_escalations_per_minute(mut self, requests: u32) -> Self {
        self.escalations_per_minute = Some(requests);
        self
    }

    /// Caps the frames billed for escalations per period.
    pub fn with_escalation_quota(mut self, frames: u64) -> Self {
        self.escalated_frames_per_period = Some(frames);
        self
    }

    /// Sets the billed minimum of one escalation request.
    pub fn with_min_frames_per_escalation(mut self, frames: u64) -> Self {
        self.min_frames_per_escalation = frames;
        self
    }

    /// Sets the quota period.
    pub fn with_period_secs(mut self, secs: u64) -> Self {
        self.period_secs = secs;
        self
    }

    /// Checks the budget.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when a rate is not a positive number, the floor exceeds
    /// the rate, the period is zero, or the per-escalation minimum is zero.
    pub fn validate(&self) -> Result<()> {
        let positive = |v: f32| v.is_finite() && v > 0.0;
        if !positive(self.local_fps) || !positive(self.min_local_fps) {
            return Err(VisionError::InvalidInput(format!(
                "frame budget rates must be positive numbers; got local_fps {} and min_local_fps {}",
                self.local_fps, self.min_local_fps
            )));
        }
        if self.min_local_fps > self.local_fps {
            return Err(VisionError::InvalidInput(format!(
                "min_local_fps {} is above local_fps {}; the floor must not exceed the rate",
                self.min_local_fps, self.local_fps
            )));
        }
        if self.period_secs == 0 {
            return Err(VisionError::InvalidInput(
                "a frame budget period must be at least 1 second".into(),
            ));
        }
        if self.min_frames_per_escalation == 0 {
            return Err(VisionError::InvalidInput(
                "min_frames_per_escalation must be at least 1; sam-3.1 video bills 50".into(),
            ));
        }
        Ok(())
    }
}

/// Why the governor stopped escalation or paused the source.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// The local quota cannot last the period even at the lowest rate.
    LocalQuotaPressure,
    /// The local quota is spent.
    LocalQuotaReached,
    /// The escalation quota cannot cover another request.
    EscalationQuotaReached,
}

/// One change of the governor's state, each written as a `vision.governor.step` record.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum GovernorChange {
    /// The local rate went down.
    RateLowered {
        /// The rate before.
        from_fps: f32,
        /// The rate after.
        to_fps: f32,
    },
    /// Escalation stopped.
    EscalationStopped {
        /// Why.
        reason: StopReason,
    },
    /// The source paused: no frame is processed until the next period.
    Paused {
        /// Why.
        reason: StopReason,
    },
    /// A new period began and the paused source resumed.
    Resumed,
    /// A new period began and the local rate went back up.
    RateRestored {
        /// The rate before.
        from_fps: f32,
        /// The rate after.
        to_fps: f32,
    },
    /// A new period began and escalation resumed.
    EscalationResumed,
}

/// The governor's counters and settings at one moment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernorState {
    /// The period's index: whole periods since the Unix epoch.
    pub period: u64,
    /// When the period ends.
    pub period_ends_at: DateTime<Utc>,
    /// Frames processed locally this period.
    pub local_frames: u64,
    /// The local quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_quota: Option<u64>,
    /// Frames billed for escalations this period.
    pub escalated_frames: u64,
    /// The escalation quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_quota: Option<u64>,
    /// The current local rate.
    pub fps: f32,
    /// Why escalation is stopped, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_stopped: Option<StopReason>,
    /// Why the source is paused, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<StopReason>,
}

/// A change and the state it left, at the time it happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernorStep {
    /// When.
    pub at: DateTime<Utc>,
    /// What changed.
    pub change: GovernorChange,
    /// The state after the change.
    pub state: GovernorState,
}

/// What to do with one offered frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FrameVerdict {
    /// Process it.
    Process,
    /// Skip it: it came sooner than the current local rate allows.
    Thinned,
    /// Skip it: the source is paused until the next period.
    Paused,
}

/// A frame's verdict and the steps taken on the way.
#[derive(Clone, Debug, PartialEq)]
pub struct Admission {
    /// What to do with the frame.
    pub verdict: FrameVerdict,
    /// The governor's changes, in order.
    pub steps: Vec<GovernorStep>,
}

/// Why an escalation was refused.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum EscalationRefusal {
    /// The source is paused.
    SourcePaused,
    /// Escalation is stopped until the next period.
    Stopped {
        /// Why.
        reason: StopReason,
    },
    /// The per-minute limit is reached; a later request may pass.
    PerMinute {
        /// The limit.
        limit: u32,
    },
    /// This request bills more frames than the quota has left; a smaller one may pass.
    OverRemaining {
        /// Frames the escalation quota has left.
        remaining: u64,
    },
}

/// Whether an escalation may go ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum EscalationVerdict {
    /// It may, billed at this many frames.
    Granted {
        /// The frames the request is billed: at least the per-request minimum.
        billed_frames: u64,
    },
    /// It may not.
    Refused {
        /// Why.
        refusal: EscalationRefusal,
    },
}

/// An escalation's verdict and the steps taken on the way.
#[derive(Clone, Debug, PartialEq)]
pub struct EscalationDecision {
    /// Whether it may go ahead.
    pub verdict: EscalationVerdict,
    /// The governor's changes, in order.
    pub steps: Vec<GovernorStep>,
}

/// Holds a stream to its [`FrameBudget`] (spec 3.7 §7.9, S11).
///
/// The governor is a pure state machine over the times it is handed — frame capture times
/// and escalation request times — so it behaves the same for a live camera and a replayed
/// file. Under pressure it steps down in a fixed order and reports every change as a
/// [`GovernorStep`]:
///
/// 1. **Lower the local rate.** When the local quota would not last the rest of the period
///    at the current rate, the rate halves until it would, down to `min_local_fps`.
/// 2. **Stop escalating.** At the floor with the quota still short, or when the escalation
///    quota cannot cover another request.
/// 3. **Pause the source.** When the local quota is spent. A source is never paused at a
///    rate above the floor or with escalation still on: reaching the quota steps through
///    both first, in that order.
///
/// Rates only go down within a period. When the next period starts the counters reset and
/// the governor resumes the source, restores the rate and resumes escalation, each as its
/// own step.
///
/// # Example
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use adk_vision::stream::{FrameBudget, FrameGovernor, FrameVerdict, GovernorChange};
///
/// // Ten frames an hour-long period at 1 fps cannot last: the rate drops to the floor,
/// // escalation stops, and the source pauses at the tenth frame.
/// let budget = FrameBudget::new(1.0).with_min_local_fps(0.5).with_local_quota(10).with_period_secs(3_600);
/// let mut governor = FrameGovernor::new(budget).unwrap();
/// let start = Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap();
/// let first = governor.offer(start);
/// assert_eq!(first.verdict, FrameVerdict::Process);
/// assert!(matches!(first.steps[0].change, GovernorChange::RateLowered { to_fps: 0.5, .. }));
/// let mut paused = false;
/// for s in 1..60 {
///     let admission = governor.offer(start + chrono::TimeDelta::seconds(s * 2));
///     paused |= admission.steps.iter().any(|s| matches!(s.change, GovernorChange::Paused { .. }));
/// }
/// assert!(paused);
/// assert_eq!(governor.state().local_frames, 10);
/// ```
#[derive(Clone, Debug)]
pub struct FrameGovernor {
    budget: FrameBudget,
    period: Option<u64>,
    local_frames: u64,
    escalated_frames: u64,
    fps: f32,
    escalation_stopped: Option<StopReason>,
    paused: Option<StopReason>,
    last_admitted: Option<DateTime<Utc>>,
    grants: VecDeque<DateTime<Utc>>,
}

impl FrameGovernor {
    /// A governor for `budget`, at its full local rate.
    ///
    /// # Errors
    ///
    /// As [`FrameBudget::validate`].
    pub fn new(budget: FrameBudget) -> Result<Self> {
        budget.validate()?;
        Ok(Self {
            fps: budget.local_fps,
            budget,
            period: None,
            local_frames: 0,
            escalated_frames: 0,
            escalation_stopped: None,
            paused: None,
            last_admitted: None,
            grants: VecDeque::new(),
        })
    }

    /// The budget.
    pub fn budget(&self) -> &FrameBudget {
        &self.budget
    }

    /// The state now.
    pub fn state(&self) -> GovernorState {
        let period = self.period.unwrap_or(0);
        GovernorState {
            period,
            period_ends_at: self.period_end(period),
            local_frames: self.local_frames,
            local_quota: self.budget.local_frames_per_period,
            escalated_frames: self.escalated_frames,
            escalation_quota: self.budget.escalated_frames_per_period,
            fps: self.fps,
            escalation_stopped: self.escalation_stopped,
            paused: self.paused,
        }
    }

    /// Decides what to do with a frame captured at `at`.
    pub fn offer(&mut self, at: DateTime<Utc>) -> Admission {
        let mut steps = Vec::new();
        self.evaluate(at, &mut steps);
        if self.paused.is_some() {
            return Admission { verdict: FrameVerdict::Paused, steps };
        }
        if let Some(last) = self.last_admitted.filter(|last| at >= *last) {
            let interval = 1.0 / f64::from(self.fps);
            let since = (at - last).as_seconds_f64();
            // The same eighth-of-an-interval slack as the sampler, for capture jitter.
            if since + interval / 8.0 < interval {
                return Admission { verdict: FrameVerdict::Thinned, steps };
            }
        }
        self.local_frames += 1;
        self.last_admitted = Some(at);
        self.pressure(at, &mut steps);
        Admission { verdict: FrameVerdict::Process, steps }
    }

    /// Decides whether an escalation of `frames` frames, requested at `at`, may go ahead,
    /// and bills it when it may.
    pub fn request_escalation(&mut self, at: DateTime<Utc>, frames: u64) -> EscalationDecision {
        let mut steps = Vec::new();
        self.evaluate(at, &mut steps);
        let refuse = |refusal, steps| EscalationDecision {
            verdict: EscalationVerdict::Refused { refusal },
            steps,
        };
        if self.paused.is_some() {
            return refuse(EscalationRefusal::SourcePaused, steps);
        }
        if let Some(reason) = self.escalation_stopped {
            return refuse(EscalationRefusal::Stopped { reason }, steps);
        }
        if let Some(limit) = self.budget.escalations_per_minute {
            let window_start = at - TimeDelta::seconds(60);
            while self.grants.front().is_some_and(|granted| *granted <= window_start) {
                self.grants.pop_front();
            }
            if self.grants.len() >= limit as usize {
                return refuse(EscalationRefusal::PerMinute { limit }, steps);
            }
        }
        let billed = frames.max(self.budget.min_frames_per_escalation);
        if let Some(quota) = self.budget.escalated_frames_per_period {
            let remaining = quota.saturating_sub(self.escalated_frames);
            if billed > remaining {
                if self.budget.min_frames_per_escalation > remaining {
                    self.stop_escalation(at, StopReason::EscalationQuotaReached, &mut steps);
                    return refuse(
                        EscalationRefusal::Stopped { reason: StopReason::EscalationQuotaReached },
                        steps,
                    );
                }
                return refuse(EscalationRefusal::OverRemaining { remaining }, steps);
            }
        }
        self.escalated_frames += billed;
        self.grants.push_back(at);
        self.pressure(at, &mut steps);
        EscalationDecision { verdict: EscalationVerdict::Granted { billed_frames: billed }, steps }
    }

    fn period_ms(&self) -> i64 {
        i64::try_from(self.budget.period_secs.saturating_mul(1_000)).unwrap_or(i64::MAX)
    }

    fn period_of(&self, at: DateTime<Utc>) -> u64 {
        u64::try_from(at.timestamp_millis().div_euclid(self.period_ms())).unwrap_or(0)
    }

    fn period_end(&self, period: u64) -> DateTime<Utc> {
        let end = i64::try_from(period.saturating_add(1))
            .unwrap_or(i64::MAX)
            .saturating_mul(self.period_ms());
        DateTime::from_timestamp_millis(end).unwrap_or(DateTime::<Utc>::MAX_UTC)
    }

    fn step(&self, at: DateTime<Utc>, change: GovernorChange, steps: &mut Vec<GovernorStep>) {
        steps.push(GovernorStep { at, change, state: self.state() });
    }

    /// Starts a new period when `at` is in one, then applies the quota pressure.
    fn evaluate(&mut self, at: DateTime<Utc>, steps: &mut Vec<GovernorStep>) {
        let period = self.period_of(at);
        match self.period {
            None => self.period = Some(period),
            Some(current) if period > current => {
                self.period = Some(period);
                self.local_frames = 0;
                self.escalated_frames = 0;
                if self.paused.take().is_some() {
                    self.step(at, GovernorChange::Resumed, steps);
                }
                if self.fps < self.budget.local_fps {
                    let from_fps = self.fps;
                    self.fps = self.budget.local_fps;
                    self.step(
                        at,
                        GovernorChange::RateRestored { from_fps, to_fps: self.fps },
                        steps,
                    );
                }
                if self.escalation_stopped.take().is_some() {
                    self.step(at, GovernorChange::EscalationResumed, steps);
                }
            }
            // A time in the current period, or one before it (a clock stepped back): the
            // current period's counters keep counting.
            Some(_) => {}
        }
        self.pressure(at, steps);
    }

    fn lower_rate(&mut self, at: DateTime<Utc>, to_fps: f32, steps: &mut Vec<GovernorStep>) {
        if to_fps < self.fps {
            let from_fps = self.fps;
            self.fps = to_fps;
            self.step(at, GovernorChange::RateLowered { from_fps, to_fps }, steps);
        }
    }

    fn stop_escalation(
        &mut self,
        at: DateTime<Utc>,
        reason: StopReason,
        steps: &mut Vec<GovernorStep>,
    ) {
        if self.escalation_stopped.is_none() {
            self.escalation_stopped = Some(reason);
            self.step(at, GovernorChange::EscalationStopped { reason }, steps);
        }
    }

    /// Steps down under the quotas, in the fixed order.
    fn pressure(&mut self, at: DateTime<Utc>, steps: &mut Vec<GovernorStep>) {
        if self.paused.is_some() {
            return;
        }
        let floor = self.budget.min_local_fps;
        if let Some(quota) = self.budget.local_frames_per_period {
            if self.local_frames >= quota {
                self.lower_rate(at, floor, steps);
                self.stop_escalation(at, StopReason::LocalQuotaReached, steps);
                self.paused = Some(StopReason::LocalQuotaReached);
                self.step(
                    at,
                    GovernorChange::Paused { reason: StopReason::LocalQuotaReached },
                    steps,
                );
                return;
            }
            let period = self.period.unwrap_or_else(|| self.period_of(at));
            let remaining = (self.period_end(period) - at).as_seconds_f64().max(0.0);
            // A frame admitted at `at` holds the slot at `at`, which `used` already counts.
            let taken = if self.last_admitted == Some(at) { 1.0 } else { 0.0 };
            let used = self.local_frames as f64;
            let projected = |fps: f32| used + (f64::from(fps) * remaining - taken).max(0.0);
            let mut target = self.fps;
            while projected(target) > quota as f64 && target > floor {
                target = (target / 2.0).max(floor);
            }
            self.lower_rate(at, target, steps);
            if projected(self.fps) > quota as f64 {
                self.stop_escalation(at, StopReason::LocalQuotaPressure, steps);
            }
        }
        if let Some(quota) = self.budget.escalated_frames_per_period
            && self.escalated_frames + self.budget.min_frames_per_escalation > quota
        {
            self.stop_escalation(at, StopReason::EscalationQuotaReached, steps);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 21, 0, 0, 0).unwrap()
    }

    fn kinds(steps: &[GovernorStep]) -> Vec<&'static str> {
        steps
            .iter()
            .map(|s| match s.change {
                GovernorChange::RateLowered { .. } => "lowered",
                GovernorChange::EscalationStopped { .. } => "escalation-stopped",
                GovernorChange::Paused { .. } => "paused",
                GovernorChange::Resumed => "resumed",
                GovernorChange::RateRestored { .. } => "restored",
                GovernorChange::EscalationResumed => "escalation-resumed",
            })
            .collect()
    }

    #[test]
    fn invalid_budgets_are_refused() {
        assert!(FrameGovernor::new(FrameBudget::new(0.0)).is_err());
        assert!(FrameGovernor::new(FrameBudget::new(5.0).with_min_local_fps(6.0)).is_err());
        assert!(FrameGovernor::new(FrameBudget::new(5.0).with_period_secs(0)).is_err());
        assert!(
            FrameGovernor::new(FrameBudget::new(5.0).with_min_frames_per_escalation(0)).is_err()
        );
    }

    #[test]
    fn an_unlimited_budget_only_thins() {
        let mut g = FrameGovernor::new(FrameBudget::new(5.0)).unwrap();
        let verdicts: Vec<_> =
            (0..10).map(|i| g.offer(t0() + TimeDelta::milliseconds(i * 100)).verdict).collect();
        assert_eq!(verdicts.iter().filter(|v| **v == FrameVerdict::Process).count(), 5);
        assert!(!verdicts.contains(&FrameVerdict::Paused));
    }

    #[test]
    fn pressure_lowers_the_rate_then_stops_escalation_then_pauses() {
        // 1,000 frames a 1,000-second period at 4 fps cannot last: 2 fps would use 2,000,
        // so the rate goes to the 1 fps floor (1,000 fits exactly).
        let budget = FrameBudget::new(4.0)
            .with_min_local_fps(1.0)
            .with_local_quota(1_000)
            .with_period_secs(1_000);
        let mut g = FrameGovernor::new(budget).unwrap();
        let start = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let first = g.offer(start);
        assert_eq!(kinds(&first.steps), ["lowered"]);
        assert_eq!(g.state().fps, 1.0);
        let mut all = first.steps;
        for s in 1..2_000 {
            all.extend(g.offer(start + TimeDelta::seconds(s) / 2).steps);
        }
        assert_eq!(g.state().local_frames, 1_000);
        assert_eq!(kinds(&all), ["lowered", "escalation-stopped", "paused"]);
        // The next period resumes everything, each as its own step.
        let next = g.offer(start + TimeDelta::seconds(1_000));
        assert_eq!(kinds(&next.steps), ["resumed", "restored", "escalation-resumed", "lowered"]);
        assert_eq!(next.verdict, FrameVerdict::Process);
    }

    #[test]
    fn the_escalation_quota_stops_escalation_and_the_next_period_resumes_it() {
        let budget = FrameBudget::new(5.0).with_escalation_quota(120).with_period_secs(3_600);
        let mut g = FrameGovernor::new(budget).unwrap();
        let at = t0();
        assert_eq!(
            g.request_escalation(at, 10).verdict,
            EscalationVerdict::Granted { billed_frames: 50 }
        );
        // 70 left would cover a minimum request, but not the 80 asked.
        let over = g.request_escalation(at, 80);
        assert_eq!(
            over.verdict,
            EscalationVerdict::Refused {
                refusal: EscalationRefusal::OverRemaining { remaining: 70 }
            }
        );
        let second = g.request_escalation(at, 50);
        assert_eq!(second.verdict, EscalationVerdict::Granted { billed_frames: 50 });
        assert_eq!(kinds(&second.steps), ["escalation-stopped"]);
        assert!(matches!(
            g.request_escalation(at, 1).verdict,
            EscalationVerdict::Refused { refusal: EscalationRefusal::Stopped { .. } }
        ));
        let next = g.request_escalation(at + TimeDelta::hours(1), 1);
        assert_eq!(kinds(&next.steps), ["escalation-resumed"]);
        assert_eq!(next.verdict, EscalationVerdict::Granted { billed_frames: 50 });
    }

    #[test]
    fn escalations_per_minute_are_capped_in_a_sliding_window() {
        let mut g =
            FrameGovernor::new(FrameBudget::new(5.0).with_escalations_per_minute(2)).unwrap();
        let at = t0();
        assert!(matches!(g.request_escalation(at, 50).verdict, EscalationVerdict::Granted { .. }));
        assert!(matches!(
            g.request_escalation(at + TimeDelta::seconds(10), 50).verdict,
            EscalationVerdict::Granted { .. }
        ));
        assert_eq!(
            g.request_escalation(at + TimeDelta::seconds(59), 50).verdict,
            EscalationVerdict::Refused { refusal: EscalationRefusal::PerMinute { limit: 2 } }
        );
        assert!(matches!(
            g.request_escalation(at + TimeDelta::seconds(61), 50).verdict,
            EscalationVerdict::Granted { .. }
        ));
    }
}
