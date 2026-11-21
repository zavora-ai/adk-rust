//! Property tests for frame ingestion (spec 3.7 §11): sampler timing, the frame governor
//! never exceeding its caps and stepping down in order, and retention expiry.

#![cfg(feature = "stream")]

use std::sync::Arc;
use std::time::Duration;

use adk_artifact::InMemoryArtifactService;
use adk_vision::stream::{
    EscalationVerdict, FrameBudget, FrameGovernor, FrameKeeper, FramePrivacy, FrameVerdict,
    GovernorChange, KeepReason, NoRedaction, RetentionPolicy, Sampler,
};
use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, MediaScope, Sampling, VisionCx};
use chrono::{DateTime, TimeDelta, Utc};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Kept frames are at least seven eighths of an interval apart, and a source faster than
    /// the rate yields one frame per interval.
    #[test]
    fn the_sampler_keeps_its_rate(
        fps in 0.5f32..60.0,
        source_ms in 1u64..400,
        jitter in proptest::collection::vec(0u64..3, 1..600),
    ) {
        let mut sampler = Sampler::new(Sampling { fps, max_long_edge: None }).unwrap();
        let interval = sampler.interval();
        let mut pts = Duration::ZERO;
        let mut kept = Vec::new();
        for j in &jitter {
            pts += Duration::from_millis(source_ms + j);
            if sampler.admit(pts) {
                kept.push(pts);
            }
        }
        prop_assert!(!kept.is_empty());
        for pair in kept.windows(2) {
            prop_assert!(pair[1] - pair[0] + interval / 8 >= interval, "{:?} apart at {:?}", pair[1] - pair[0], interval);
        }
        // A dense source (frames at most an eighth of an interval apart) fills every slot.
        if Duration::from_millis(source_ms + 2) * 8 <= interval {
            let span = (*kept.last().unwrap() - kept[0]).as_secs_f64();
            let slots = span / interval.as_secs_f64();
            prop_assert!((kept.len() as f64 - 1.0 - slots).abs() <= 1.0, "{} kept over {slots} slots", kept.len());
        }
    }

    /// Fitting only ever scales down, to the long edge asked for, with the aspect ratio kept.
    #[test]
    fn fitting_keeps_the_aspect_ratio(w in 1u32..5_000, h in 1u32..5_000, max in 1u32..2_000) {
        let sampler = Sampler::new(Sampling { fps: 1.0, max_long_edge: Some(max) }).unwrap();
        let (fw, fh) = sampler.fitted_size(w, h);
        prop_assert!(fw <= w && fh <= h);
        prop_assert!(fw.max(fh) <= max.max(1));
        if w.max(h) <= max {
            prop_assert_eq!((fw, fh), (w, h));
        } else {
            let scale = f64::from(max) / f64::from(w.max(h));
            prop_assert!((f64::from(fw) - f64::from(w) * scale).abs() <= 1.0);
            prop_assert!((f64::from(fh) - f64::from(h) * scale).abs() <= 1.0);
        }
    }
}

#[derive(Clone, Debug)]
enum Event {
    Frame { gap_ms: u64 },
    Escalate { gap_ms: u64, frames: u64 },
}

fn events() -> impl Strategy<Value = Vec<Event>> {
    proptest::collection::vec(
        prop_oneof![
            4 => (1u64..2_000).prop_map(|gap_ms| Event::Frame { gap_ms }),
            1 => ((1u64..30_000), (1u64..200)).prop_map(|(gap_ms, frames)| Event::Escalate { gap_ms, frames }),
        ],
        1..400,
    )
}

fn budgets() -> impl Strategy<Value = FrameBudget> {
    (
        0.5f32..30.0,
        0.05f32..1.0,
        proptest::option::of(1u64..400),
        proptest::option::of(1u32..6),
        proptest::option::of(1u64..600),
        1u64..80,
        30u64..400,
    )
        .prop_map(|(fps, floor, local, per_minute, escalated, min_frames, period)| {
            let mut budget = FrameBudget::new(fps)
                .with_min_local_fps(fps * floor)
                .with_min_frames_per_escalation(min_frames)
                .with_period_secs(period);
            budget.local_frames_per_period = local;
            budget.escalations_per_minute = per_minute;
            budget.escalated_frames_per_period = escalated;
            budget
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Under any stream, the governor never exceeds a cap, bills every escalation at least
    /// its minimum, and never pauses a source before it has lowered the rate to the floor
    /// and stopped escalation.
    #[test]
    fn the_governor_never_exceeds_its_caps(budget in budgets(), events in events(), offset in 0i64..86_400) {
        let mut governor = FrameGovernor::new(budget.clone()).unwrap();
        let mut at: DateTime<Utc> = DateTime::from_timestamp(1_700_000_000 + offset, 0).unwrap();
        let period_ms = (budget.period_secs * 1_000) as i64;
        let period_of = |t: DateTime<Utc>| t.timestamp_millis().div_euclid(period_ms);
        let mut period = period_of(at);
        let (mut local, mut escalated) = (0u64, 0u64);
        let mut grants: Vec<DateTime<Utc>> = Vec::new();
        let mut last_processed: Option<DateTime<Utc>> = None;
        let mut steps_this_period: Vec<GovernorChange> = Vec::new();
        for event in events {
            let (gap_ms, escalation) = match event {
                Event::Frame { gap_ms } => (gap_ms, None),
                Event::Escalate { gap_ms, frames } => (gap_ms, Some(frames)),
            };
            at += TimeDelta::milliseconds(gap_ms as i64);
            if period_of(at) != period {
                period = period_of(at);
                local = 0;
                escalated = 0;
                steps_this_period.clear();
            }
            let steps = match escalation {
                None => {
                    let admission = governor.offer(at);
                    if admission.verdict == FrameVerdict::Process {
                        local += 1;
                        if let Some(last) = last_processed {
                            // Never faster than the budget's top rate.
                            let gap = (at - last).as_seconds_f64();
                            prop_assert!(gap + 1.0 / f64::from(budget.local_fps) / 8.0 + 1e-9 >= 1.0 / f64::from(budget.local_fps));
                        }
                        last_processed = Some(at);
                    }
                    admission.steps
                }
                Some(frames) => {
                    let decision = governor.request_escalation(at, frames);
                    if let EscalationVerdict::Granted { billed_frames } = decision.verdict {
                        prop_assert!(billed_frames >= frames.max(budget.min_frames_per_escalation));
                        escalated += billed_frames;
                        grants.push(at);
                    }
                    decision.steps
                }
            };
            for step in steps {
                if matches!(step.change, GovernorChange::Resumed | GovernorChange::RateRestored { .. } | GovernorChange::EscalationResumed) {
                    steps_this_period.clear();
                }
                if let GovernorChange::Paused { .. } = step.change {
                    prop_assert_eq!(step.state.fps, budget.min_local_fps, "paused above the floor");
                    prop_assert!(step.state.escalation_stopped.is_some(), "paused with escalation on");
                    let stopped = steps_this_period.iter().any(|c| matches!(c, GovernorChange::EscalationStopped { .. }));
                    prop_assert!(stopped, "paused before stopping escalation: {:?}", steps_this_period);
                    if budget.local_fps > budget.min_local_fps {
                        let lowered = steps_this_period.iter().any(|c| matches!(c, GovernorChange::RateLowered { .. }));
                        prop_assert!(lowered, "paused before lowering the rate: {:?}", steps_this_period);
                    }
                }
                steps_this_period.push(step.change);
            }
            if let Some(quota) = budget.local_frames_per_period {
                prop_assert!(local <= quota, "{local} frames over a quota of {quota}");
            }
            if let Some(quota) = budget.escalated_frames_per_period {
                prop_assert!(escalated <= quota, "{escalated} frames escalated over {quota}");
            }
            if let Some(limit) = budget.escalations_per_minute {
                let in_window = grants.iter().filter(|g| at - **g < TimeDelta::seconds(60)).count();
                prop_assert!(in_window <= limit as usize, "{in_window} escalations in a minute over {limit}");
            }
        }
    }
}

fn frame(index: u64, shade: u8) -> Frame {
    Frame {
        source: FrameSourceId("cam".into()),
        index,
        pts: Duration::from_secs(index),
        captured_at: Utc::now(),
        pixels: FramePixels::Ephemeral(Arc::new(DecodedImage::filled(4, 4, [shade, 0, 0, 255]))),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// A purge at `day` deletes exactly the frames whose latest expiry is at or before it.
    #[test]
    fn purges_delete_exactly_what_has_expired(
        keeps in proptest::collection::vec((0u8..6, 0usize..3, 0i64..60), 1..12),
        retention_days in (1u64..60, 1u64..60, 1u64..60),
        day in 0i64..130,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let reasons = [KeepReason::WakePayload, KeepReason::CardEvidence, KeepReason::PersonKept];
            let days = |d: u64| Duration::from_secs(d * 86_400);
            let policy = RetentionPolicy::default()
                .with_reason(KeepReason::WakePayload, days(retention_days.0))
                .with_reason(KeepReason::CardEvidence, days(retention_days.1))
                .with_reason(KeepReason::PersonKept, days(retention_days.2));
            let keeper = FrameKeeper::new(
                Arc::new(InMemoryArtifactService::new()),
                MediaScope::new("app", "u", "s"),
                FrameSourceId("cam".into()),
            )
            .with_policy(policy);
            let cx = VisionCx::new();
            let privacy = FramePrivacy::new(Arc::new(NoRedaction));
            let t0 = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
            let mut expiry: std::collections::BTreeMap<u8, DateTime<Utc>> = Default::default();
            for (shade, reason, kept_day) in &keeps {
                let redacted = privacy.apply(&cx, frame(u64::from(*shade), *shade)).await.unwrap();
                let at = t0 + TimeDelta::days(*kept_day);
                keeper.keep_at(&cx, &redacted, reasons[*reason], at).await.unwrap();
                let until = at + TimeDelta::from_std(policy.for_reason(reasons[*reason])).unwrap();
                let entry = expiry.entry(*shade).or_insert(until);
                *entry = (*entry).max(until);
            }
            let now = t0 + TimeDelta::days(day);
            let purged = keeper.purge_expired(&cx, now).await.unwrap();
            let expected = expiry.values().filter(|until| **until <= now).count();
            assert_eq!(purged.len(), expected);
            assert_eq!(keeper.kept().await.unwrap().len(), expiry.len() - expected);
            assert!(purged.iter().all(|p| p.entry.expires_at <= now));
            assert!(keeper.kept().await.unwrap().iter().all(|e| e.expires_at > now));
        });
    }
}
