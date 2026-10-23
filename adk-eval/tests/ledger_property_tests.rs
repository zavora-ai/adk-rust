//! Property tests for the ledger statistics (spec 3.2 §7.10, §10 E1).

use std::collections::BTreeMap;

use adk_eval::ledger::{
    DailyValue, DeviationConfig, DimensionDeviation, assess, check_dimension, mad, median,
};
use chrono::{Days, NaiveDate};
use proptest::prelude::*;

fn start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

/// A daily series of whole numbers over up to ten weeks: a level, per-day noise, an optional
/// step change, and missing days.
fn arb_series() -> impl Strategy<Value = Vec<DailyValue>> {
    (
        0i32..1000,
        prop::collection::vec((-20i32..=20, prop::bool::weighted(0.9)), 1..70),
        prop::option::of((0usize..70, -300i32..=300)),
    )
        .prop_map(|(level, days, step)| {
            days.into_iter()
                .enumerate()
                .filter(|(_, (_, present))| *present)
                .map(|(offset, (noise, _))| {
                    let shift = match step {
                        Some((from, delta)) if offset >= from => delta,
                        Some(_) | None => 0,
                    };
                    DailyValue::new(
                        start() + Days::new(offset as u64),
                        f64::from(level + noise + shift),
                    )
                })
                .collect()
        })
}

/// Every day from the series' first day to a week past its last, so the windows slide out.
fn test_days(series: &[DailyValue]) -> Vec<NaiveDate> {
    let last = series.iter().map(|point| point.date).max().unwrap_or_else(start);
    start().iter_days().take_while(|day| *day <= last + Days::new(7)).collect()
}

/// Each class's and each window's decision, in order.
fn decisions(test: &DimensionDeviation) -> Vec<bool> {
    let mut flags = vec![test.deviating];
    for class in &test.classes {
        flags.push(class.deviating);
        flags.extend(class.windows.iter().map(|window| window.deviates));
    }
    flags
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The median and the MAD do not depend on the order of the values.
    #[test]
    fn median_and_mad_are_permutation_invariant(
        (values, shuffled) in prop::collection::vec(-1.0e6..1.0e6f64, 1..60)
            .prop_flat_map(|values| (Just(values.clone()), Just(values).prop_shuffle()))
    ) {
        prop_assert_eq!(median(&values), median(&shuffled));
        prop_assert_eq!(mad(&values), mad(&shuffled));
    }

    /// A constant series never deviates, whatever its gaps, floor or k, on any day, so the
    /// gate over it is never meaningful.
    #[test]
    fn a_constant_series_never_deviates(
        level in -1.0e6..1.0e6f64,
        present in prop::collection::vec(prop::bool::weighted(0.8), 1..70),
        floor in 0.0..100.0f64,
        k in 0.0..10.0f64,
    ) {
        let series: Vec<DailyValue> = present
            .iter()
            .enumerate()
            .filter(|(_, present)| **present)
            .map(|(offset, _)| DailyValue::new(start() + Days::new(offset as u64), level))
            .collect();
        let config = DeviationConfig { k, ..DeviationConfig::default() }.with_default_floor(floor);
        for day in test_days(&series) {
            let test = check_dimension("x", &series, day, &config);
            prop_assert!(!test.deviating, "deviated on {day}");
            for class in &test.classes {
                if let Some(baseline) = class.baseline {
                    prop_assert_eq!(baseline.mad, 0.0);
                }
            }
        }
        let dims = BTreeMap::from([("x".to_string(), series)]);
        if let Some(assessment) = assess(&dims, &config) {
            prop_assert!(assessment.runs.is_empty());
            prop_assert!(!assessment.meaningful);
        }
    }

    /// Multiplying the values by a factor multiplies the median by it and the MAD by its
    /// magnitude.
    #[test]
    fn scaling_a_series_scales_its_statistics(
        values in prop::collection::vec(-1.0e6..1.0e6f64, 1..60),
        magnitude in 1.0e-3..1.0e3f64,
        negative in any::<bool>(),
    ) {
        let factor = if negative { -magnitude } else { magnitude };
        let scaled: Vec<f64> = values.iter().map(|value| value * factor).collect();
        let close = |a: f64, b: f64| (a - b).abs() <= 1.0e-9 * a.abs().max(b.abs()).max(1.0);

        let (m, s) = (median(&values).unwrap(), median(&scaled).unwrap());
        prop_assert!(close(s, m * factor), "median {s} vs {m} × {factor}");
        let (d, sd) = (mad(&values).unwrap(), mad(&scaled).unwrap());
        prop_assert!(close(sd, d * magnitude), "MAD {sd} vs {d} × {magnitude}");
    }

    /// Scaling a series and its floor by a power of two, which floating point does exactly,
    /// scales every baseline and window median and leaves every decision unchanged.
    #[test]
    fn scaling_a_series_and_its_floor_preserves_every_decision(
        series in arb_series(),
        exponent in -8i32..=8,
        floor in 0u32..50,
    ) {
        let factor = 2f64.powi(exponent);
        let scaled: Vec<DailyValue> = series
            .iter()
            .map(|point| DailyValue::new(point.date, point.value * factor))
            .collect();
        let config = DeviationConfig::default().with_default_floor(f64::from(floor));
        let scaled_config =
            DeviationConfig::default().with_default_floor(f64::from(floor) * factor);
        for day in test_days(&series) {
            let original = check_dimension("x", &series, day, &config);
            let transformed = check_dimension("x", &scaled, day, &scaled_config);
            prop_assert_eq!(decisions(&original), decisions(&transformed), "on {}", day);
            for (a, b) in original.classes.iter().zip(&transformed.classes) {
                prop_assert_eq!(a.baseline.map(|x| x.median * factor), b.baseline.map(|x| x.median));
                prop_assert_eq!(a.baseline.map(|x| x.mad * factor), b.baseline.map(|x| x.mad));
                for (wa, wb) in a.windows.iter().zip(&b.windows) {
                    prop_assert_eq!(wa.recent_median.map(|x| x * factor), wb.recent_median);
                }
            }
        }
    }

    /// Adding a constant to a series shifts its medians and leaves its MADs and every
    /// decision unchanged.
    #[test]
    fn a_constant_offset_preserves_every_decision(
        series in arb_series(),
        offset in -10_000i32..10_000,
        floor in 0u32..50,
    ) {
        let shift = f64::from(offset);
        let shifted: Vec<DailyValue> = series
            .iter()
            .map(|point| DailyValue::new(point.date, point.value + shift))
            .collect();
        let config = DeviationConfig::default().with_default_floor(f64::from(floor));
        for day in test_days(&series) {
            let original = check_dimension("x", &series, day, &config);
            let transformed = check_dimension("x", &shifted, day, &config);
            prop_assert_eq!(decisions(&original), decisions(&transformed), "on {}", day);
            for (a, b) in original.classes.iter().zip(&transformed.classes) {
                prop_assert_eq!(a.baseline.map(|x| x.median + shift), b.baseline.map(|x| x.median));
                prop_assert_eq!(a.baseline.map(|x| x.mad), b.baseline.map(|x| x.mad));
            }
        }
    }
}
