//! Property tests of `ExperimentAnalyzer` (spec 3.5 §7.12): the boundary and the always-valid
//! p-value agree, p-values never rise, final decisions stick, the arms are symmetric, and the
//! interval excludes zero exactly when the test rejects.

#![cfg(feature = "experiments")]

use adk_eval::experiments::{
    Decision, Evidence, ExperimentAnalyzer, ExperimentConfig, ExperimentReport, GuardrailSpec,
    Look, MetricDirection, MetricSpec, Sample, SequentialBoundary, Tolerance,
};
use proptest::prelude::*;

fn config() -> ExperimentConfig {
    ExperimentConfig::new(
        "prop",
        MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
    )
    .with_guardrail(GuardrailSpec::new(
        MetricSpec::proportion("refunds", MetricDirection::LowerIsBetter),
        Tolerance::Relative(0.2),
    ))
}

/// One arm's increment for a day: units, sign-ups and refunds.
type Arm = (u64, u64, u64);

/// One day's increments for the control and the variant.
fn increments() -> impl Strategy<Value = Vec<(Arm, Arm)>> {
    let arm = (100u64..2_000).prop_flat_map(|units| (Just(units), 0..=units / 5, 0..=units / 20));
    prop::collection::vec((arm.clone(), arm), 1..12)
}

fn looks(days: &[(Arm, Arm)]) -> Vec<Look> {
    let (mut control, mut variant) = ((0, 0, 0), (0, 0, 0));
    days.iter()
        .map(|&(c, v)| {
            control = (control.0 + c.0, control.1 + c.1, control.2 + c.2);
            variant = (variant.0 + v.0, variant.1 + v.1, variant.2 + v.2);
            Look::new()
                .with_metric(
                    "signups",
                    Sample::proportion(control.0, control.1),
                    Sample::proportion(variant.0, variant.1),
                )
                .with_metric(
                    "refunds",
                    Sample::proportion(control.0, control.2),
                    Sample::proportion(variant.0, variant.2),
                )
        })
        .collect()
}

fn sequential(report: &ExperimentReport) -> (f64, f64, Option<f64>, f64) {
    match report.primary.evidence {
        Evidence::Sequential { p_value, z, critical_z, alpha } => (p_value, z, critical_z, alpha),
        other => panic!("expected sequential evidence, got {other:?}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_boundary_and_the_p_value_agree(
        alpha in 0.001f64..0.5,
        planned in 10.0f64..1e6,
        units in 1.0f64..1e7,
        z in -12.0f64..12.0,
    ) {
        let boundary = SequentialBoundary::new(alpha, planned).unwrap();
        let critical = boundary.critical_z(units);
        prop_assert!(critical > 0.0 && critical.is_finite());
        prop_assert!((boundary.p_value(critical, units) - alpha).abs() <= 1e-9 * alpha.max(1e-3));
        let p_value = boundary.p_value(z, units);
        prop_assert!((0.0..=1.0).contains(&p_value));
        if (z.abs() - critical).abs() > 1e-6 {
            prop_assert_eq!(p_value <= alpha, z.abs() >= critical);
        }
    }

    #[test]
    fn p_values_never_rise_and_final_decisions_stick(days in increments()) {
        let mut analyzer = ExperimentAnalyzer::new(config()).unwrap();
        let (mut primary, mut guardrail) = (1.0, 1.0);
        let mut stopped: Option<ExperimentReport> = None;
        for look in looks(&days) {
            let report = analyzer.observe(&look).unwrap();
            if let Some(stop) = &stopped {
                prop_assert_eq!(&report, stop);
                continue;
            }
            let (p_value, ..) = sequential(&report);
            prop_assert!(p_value <= primary);
            prop_assert!(report.guardrails[0].p_value <= guardrail);
            primary = p_value;
            guardrail = report.guardrails[0].p_value;
            if report.decision.is_final() {
                stopped = Some(report);
            }
        }
    }

    #[test]
    fn swapping_the_arms_mirrors_the_effect(
        control in (100u64..50_000).prop_flat_map(|units| (Just(units), 0..=units)),
        variant in (100u64..50_000).prop_flat_map(|units| (Just(units), 0..=units)),
    ) {
        let spec = MetricSpec::proportion("signups", MetricDirection::HigherIsBetter);
        let analyse = |a: (u64, u64), b: (u64, u64)| {
            let mut analyzer =
                ExperimentAnalyzer::new(ExperimentConfig::new("prop", spec.clone())).unwrap();
            analyzer
                .observe(&Look::new().with_metric(
                    "signups",
                    Sample::proportion(a.0, a.1),
                    Sample::proportion(b.0, b.1),
                ))
                .unwrap()
        };
        let forward = analyse(control, variant);
        let backward = analyse(variant, control);
        let (forward_p, forward_z, ..) = sequential(&forward);
        let (backward_p, backward_z, ..) = sequential(&backward);
        prop_assert_eq!(forward.primary.effect.difference, -backward.primary.effect.difference);
        prop_assert_eq!(forward_z, -backward_z);
        prop_assert_eq!(forward_p, backward_p);
        match (
            forward.primary.effect.difference_interval,
            backward.primary.effect.difference_interval,
        ) {
            (Some(forward_interval), Some(backward_interval)) => {
                prop_assert!((forward_interval.lower + backward_interval.upper).abs() < 1e-12);
            }
            (forward_interval, backward_interval) => {
                // Only data with no variance in either arm has no interval.
                prop_assert_eq!(forward_interval, None);
                prop_assert_eq!(backward_interval, None);
            }
        }
        let mirrored = match forward.decision {
            Decision::AdoptVariant => backward.decision.keeps_control(),
            Decision::Continue => backward.decision == Decision::Continue,
            _ => backward.decision == Decision::AdoptVariant,
        };
        prop_assert!(mirrored, "{:?} against {:?}", forward.decision, backward.decision);
    }

    #[test]
    fn the_interval_excludes_zero_exactly_when_the_test_rejects(
        control in (100u64..50_000).prop_flat_map(|units| (Just(units), 0..=units)),
        variant in (100u64..50_000).prop_flat_map(|units| (Just(units), 0..=units)),
    ) {
        let mut analyzer = ExperimentAnalyzer::new(ExperimentConfig::new(
            "prop",
            MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
        ))
        .unwrap();
        let report = analyzer
            .observe(&Look::new().with_metric(
                "signups",
                Sample::proportion(control.0, control.1),
                Sample::proportion(variant.0, variant.1),
            ))
            .unwrap();
        let (p_value, z, critical, alpha) = sequential(&report);
        let Some(interval) = report.primary.effect.difference_interval else {
            // No variance in either arm (every unit or none converted): no evidence either way.
            prop_assert_eq!(p_value, 1.0);
            prop_assert_eq!(report.decision, Decision::Continue);
            return Ok(());
        };
        prop_assert!(interval.contains(report.primary.effect.difference));
        if (z.abs() - critical.unwrap()).abs() > 1e-9 {
            prop_assert_eq!(p_value <= alpha, !interval.contains(0.0));
            prop_assert_eq!(p_value <= alpha, report.decision.is_final());
        }
    }

    #[test]
    fn merged_summaries_match_summarising_every_value(
        values in prop::collection::vec(-1e4f64..1e4, 2..200),
        split in 0usize..200,
    ) {
        let split = split.min(values.len());
        let merged = Sample::from_values(&values[..split])
            .merge(&Sample::from_values(&values[split..]))
            .unwrap();
        let whole = Sample::from_values(&values);
        let (Sample::Mean { units, mean, variance }, Sample::Mean {
            units: whole_units,
            mean: whole_mean,
            variance: whole_variance,
        }) = (merged, whole) else {
            panic!("mean samples");
        };
        prop_assert_eq!(units, whole_units);
        prop_assert!((mean - whole_mean).abs() <= 1e-9 * (1.0 + whole_mean.abs()));
        prop_assert!((variance - whole_variance).abs() <= 1e-7 * (1.0 + whole_variance));
    }
}
