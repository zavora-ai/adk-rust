//! Simulation tests of `ExperimentAnalyzer` (spec 3.5 §7.12): the type I error under daily
//! peeking stays within the error budget, the test has power on a known lift, and its intervals
//! cover the true effect when it stops.
//!
//! Every simulation uses a fixed seed, so each run draws the same data.

#![cfg(feature = "experiments")]

use adk_eval::experiments::{
    AnalysisMethod, BayesianConfig, Decision, ErrorBudget, Evidence, ExperimentAnalyzer,
    ExperimentConfig, GuardrailSpec, Look, MetricDirection, MetricSpec, Sample, StopReason,
    Tolerance,
};

/// SplitMix64: a small, fast generator that is good enough for Monte Carlo.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform draw in the open interval (0, 1).
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// An exact Binomial(n, p) draw by skipping geometrically from one event to the next.
    fn binomial(&mut self, n: u64, p: f64) -> u64 {
        let log_miss = (1.0 - p).ln();
        let (mut events, mut position) = (0, 0u64);
        loop {
            let skip = (self.uniform().ln() / log_miss).floor() as u64;
            position = position.saturating_add(skip).saturating_add(1);
            if position > n {
                return events;
            }
            events += 1;
        }
    }

    fn normal(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
}

/// What one simulated experiment ended with.
struct Run {
    decision: Decision,
    /// Whether the primary metric's difference interval at the stopping look covered the truth.
    covered: Option<bool>,
    /// Whether the naive fixed-sample z test (|z| ≥ 1.96) would have fired at any look.
    naive_fired: bool,
}

/// Simulates daily looks at a proportion experiment.
fn run_proportions(
    rng: &mut Rng,
    config: &ExperimentConfig,
    per_day: u64,
    days: u64,
    (signups_control, signups_variant): (f64, f64),
    guardrail: Option<(f64, f64)>,
) -> Run {
    let mut analyzer = ExperimentAnalyzer::new(config.clone()).unwrap();
    let (mut control, mut variant, mut guard_control, mut guard_variant) = (0, 0, 0, 0);
    let mut naive_fired = false;
    let mut last = None;
    for day in 1..=days {
        control += rng.binomial(per_day, signups_control);
        variant += rng.binomial(per_day, signups_variant);
        let units = per_day * day;
        let (rate_control, rate_variant) =
            (control as f64 / units as f64, variant as f64 / units as f64);
        let se = ((rate_control * (1.0 - rate_control) + rate_variant * (1.0 - rate_variant))
            / units as f64)
            .sqrt();
        naive_fired |= se > 0.0 && ((rate_variant - rate_control) / se).abs() >= 1.959_964;
        let mut look = Look::new().with_metric(
            "signups",
            Sample::proportion(units, control),
            Sample::proportion(units, variant),
        );
        if let Some((guard_rate_control, guard_rate_variant)) = guardrail {
            guard_control += rng.binomial(per_day, guard_rate_control);
            guard_variant += rng.binomial(per_day, guard_rate_variant);
            look = look.with_metric(
                "unsubscribes",
                Sample::proportion(units, guard_control),
                Sample::proportion(units, guard_variant),
            );
        }
        let report = analyzer.observe(&look).unwrap();
        let stopped = report.decision.is_final();
        last = Some(report);
        if stopped {
            break;
        }
    }
    let report = last.expect("at least one look");
    let truth = signups_variant - signups_control;
    Run {
        covered: report.primary.effect.difference_interval.map(|interval| interval.contains(truth)),
        decision: report.decision,
        naive_fired,
    }
}

fn signups_config() -> ExperimentConfig {
    ExperimentConfig::new("sim", MetricSpec::proportion("signups", MetricDirection::HigherIsBetter))
}

fn rate(runs: &[Run], predicate: impl Fn(&Run) -> bool) -> f64 {
    runs.iter().filter(|run| predicate(run)).count() as f64 / runs.len() as f64
}

#[test]
fn type_i_error_under_daily_peeking_stays_within_budget_for_proportions() {
    let mut rng = Rng(0x5EED_0001);
    let config = signups_config();
    let runs: Vec<Run> = (0..2_000)
        .map(|_| run_proportions(&mut rng, &config, 400, 30, (0.10, 0.10), None))
        .collect();
    let false_decisions = rate(&runs, |run| {
        matches!(
            run.decision,
            Decision::AdoptVariant | Decision::KeepControl { reason: StopReason::VariantWorse }
        )
    });
    let naive = rate(&runs, |run| run.naive_fired);
    eprintln!("sequential false decision rate {false_decisions}, naive peeking rate {naive}");
    // Thirty looks at a fixed-sample 5% test fire far more often than 5%; the sequential test
    // stays inside its budget on the same data.
    assert!(false_decisions <= 0.05, "sequential false decision rate {false_decisions}");
    assert!(naive > 0.15, "naive peeking rate {naive}");
}

#[test]
fn type_i_error_stays_within_budget_for_heavy_tailed_means() {
    let mut rng = Rng(0x5EED_0002);
    let config =
        ExperimentConfig::new("sim", MetricSpec::mean("revenue", MetricDirection::HigherIsBetter));
    // Revenue per visitor: 3% buy, with a log-normal basket.
    let day = |rng: &mut Rng| {
        let values: Vec<f64> = (0..300)
            .map(|_| if rng.uniform() < 0.03 { (3.5 + rng.normal()).exp() } else { 0.0 })
            .collect();
        Sample::from_values(&values)
    };
    let mut false_decisions = 0;
    let reps = 1_000;
    for _ in 0..reps {
        let mut analyzer = ExperimentAnalyzer::new(config.clone()).unwrap();
        let (mut control, mut variant) = (Sample::mean(0, 0.0, 0.0), Sample::mean(0, 0.0, 0.0));
        for _ in 0..30 {
            control = control.merge(&day(&mut rng)).unwrap();
            variant = variant.merge(&day(&mut rng)).unwrap();
            let report =
                analyzer.observe(&Look::new().with_metric("revenue", control, variant)).unwrap();
            if report.decision.is_final() {
                false_decisions += 1;
                break;
            }
        }
    }
    let rate = false_decisions as f64 / reps as f64;
    eprintln!("heavy-tailed means false decision rate {rate}");
    assert!(rate <= 0.05, "false decision rate {rate}");
}

#[test]
fn rare_event_guardrails_stay_within_their_budget() {
    let mut rng = Rng(0x5EED_0003);
    // Unsubscribes at 0.5% in both arms, and a variant sitting exactly on a 10% relative
    // tolerance: neither is harm beyond tolerance, so every breach is a false alarm.
    for (tolerance, rates) in
        [(Tolerance::Absolute(0.0), (0.005, 0.005)), (Tolerance::Relative(0.1), (0.01, 0.011))]
    {
        let config = signups_config().with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
            tolerance,
        ));
        let runs: Vec<Run> = (0..2_000)
            .map(|_| run_proportions(&mut rng, &config, 300, 30, (0.04, 0.04), Some(rates)))
            .collect();
        let breaches = rate(&runs, |run| {
            matches!(
                run.decision,
                Decision::KeepControl { reason: StopReason::GuardrailBreach { .. } }
            )
        });
        eprintln!("{tolerance:?} false breach rate {breaches}");
        assert!(breaches <= ErrorBudget::default().guardrail_alpha, "{tolerance:?}: {breaches}");
    }
}

#[test]
fn power_and_coverage_on_a_known_lift() {
    let mut rng = Rng(0x5EED_0004);
    let config = signups_config();
    // Sign-ups from 10% to 12%, 500 visitors per arm per day, four weeks.
    let runs: Vec<Run> = (0..1_000)
        .map(|_| run_proportions(&mut rng, &config, 500, 28, (0.10, 0.12), None))
        .collect();
    let power = rate(&runs, |run| run.decision == Decision::AdoptVariant);
    let wrong_way = rate(&runs, |run| {
        run.decision == Decision::KeepControl { reason: StopReason::VariantWorse }
    });
    let covered = rate(&runs, |run| run.covered == Some(true));
    eprintln!("power {power}, wrong way {wrong_way}, coverage at stop {covered}");
    assert!(power >= 0.9, "power {power}");
    assert_eq!(wrong_way, 0.0);
    // A confidence sequence covers the truth at every look at once, so also at the look where
    // the experiment stopped.
    assert!(covered >= 0.95, "coverage {covered}");
}

#[test]
fn power_on_a_known_shift_in_means() {
    let mut rng = Rng(0x5EED_0005);
    let config =
        ExperimentConfig::new("sim", MetricSpec::mean("pages", MetricDirection::HigherIsBetter));
    let reps = 400;
    let mut adopted = 0;
    for _ in 0..reps {
        let mut analyzer = ExperimentAnalyzer::new(config.clone()).unwrap();
        let (mut control, mut variant) = (Sample::mean(0, 0.0, 0.0), Sample::mean(0, 0.0, 0.0));
        for _ in 0..30 {
            // Pages per session: mean 10, sd 3; the variant adds a quarter of a page.
            let control_day: Vec<f64> = (0..200).map(|_| 10.0 + 3.0 * rng.normal()).collect();
            let variant_day: Vec<f64> = (0..200).map(|_| 10.25 + 3.0 * rng.normal()).collect();
            control = control.merge(&Sample::from_values(&control_day)).unwrap();
            variant = variant.merge(&Sample::from_values(&variant_day)).unwrap();
            let report =
                analyzer.observe(&Look::new().with_metric("pages", control, variant)).unwrap();
            if report.decision.is_final() {
                adopted += usize::from(report.decision == Decision::AdoptVariant);
                break;
            }
        }
    }
    let power = adopted as f64 / reps as f64;
    eprintln!("means power {power}");
    assert!(power >= 0.8, "power {power}");
}

#[test]
fn a_doubled_guardrail_metric_is_caught() {
    let mut rng = Rng(0x5EED_0006);
    let config = signups_config().with_guardrail(GuardrailSpec::new(
        MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
        Tolerance::Relative(0.1),
    ));
    // S11's shape: the variant lifts sign-ups but doubles unsubscribes.
    let runs: Vec<Run> = (0..500)
        .map(|_| run_proportions(&mut rng, &config, 600, 28, (0.04, 0.05), Some((0.005, 0.01))))
        .collect();
    let stopped = rate(&runs, |run| {
        matches!(run.decision, Decision::KeepControl { reason: StopReason::GuardrailBreach { .. } })
    });
    let adopted = rate(&runs, |run| run.decision == Decision::AdoptVariant);
    eprintln!("guardrail detection rate {stopped}, adoption rate {adopted}");
    assert!(stopped >= 0.9, "guardrail detection rate {stopped}");
    assert!(adopted <= 0.05, "adoption rate despite the harm {adopted}");
}

#[test]
fn the_bayesian_option_finds_a_known_lift_but_has_no_peeking_guarantee() {
    let mut rng = Rng(0x5EED_0007);
    let config = signups_config().with_method(AnalysisMethod::Bayesian(BayesianConfig::default()));
    let lift: Vec<Run> =
        (0..500).map(|_| run_proportions(&mut rng, &config, 500, 28, (0.10, 0.12), None)).collect();
    let power = rate(&lift, |run| run.decision == Decision::AdoptVariant);
    assert!(power >= 0.9, "power {power}");

    // With no difference, deciding whenever P(B better) leaves (0.05, 0.95) at any of thirty
    // looks decides far more often than a 5% error budget would allow; this is why the
    // sequential test is the default (spec 3.5 §13 Q8).
    let null: Vec<Run> = (0..1_000)
        .map(|_| run_proportions(&mut rng, &config, 400, 30, (0.10, 0.10), None))
        .collect();
    let decided = rate(&null, |run| run.decision.is_final());
    eprintln!("bayesian power {power}, decision rate under no difference {decided}");
    assert!(decided > 0.10, "bayesian decision rate under no difference {decided}");
}

#[test]
fn the_sequential_evidence_is_reported_at_every_look() {
    let mut rng = Rng(0x5EED_0008);
    let mut analyzer = ExperimentAnalyzer::new(signups_config()).unwrap();
    let (mut control, mut variant) = (0, 0);
    let mut previous = 1.0;
    for day in 1..=20 {
        control += rng.binomial(500, 0.10);
        variant += rng.binomial(500, 0.105);
        let units = 500 * day;
        let report = analyzer
            .observe(&Look::new().with_metric(
                "signups",
                Sample::proportion(units, control),
                Sample::proportion(units, variant),
            ))
            .unwrap();
        let Evidence::Sequential { p_value, .. } = report.primary.evidence else {
            panic!("sequential evidence");
        };
        assert!(p_value <= previous, "the always-valid p-value never rises");
        previous = p_value;
    }
}
