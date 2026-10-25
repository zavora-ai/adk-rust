//! Scenario S11 at the analyzer level (spec 3.5 §5, §7.12, A-I15): two headline variants;
//! variant B lifts sign-ups but doubles unsubscribes. The analysis reports the lift with its
//! interval, the unsubscribe guardrail breach stops the experiment and keeps A, and the decision
//! and its evidence are what the weekly review shows.

#![cfg(feature = "experiments")]

use adk_eval::experiments::{
    Decision, Evidence, ExperimentAnalyzer, ExperimentConfig, ExperimentReport, ExperimentState,
    GuardrailSpec, Look, MetricDirection, MetricSpec, Sample, StopReason, Tolerance,
};
use serde_json::json;

const VISITORS_PER_DAY: u64 = 600;

fn headline_experiment() -> ExperimentConfig {
    ExperimentConfig::new(
        "headline",
        MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
    )
    .with_guardrail(GuardrailSpec::new(
        MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
        Tolerance::Relative(0.1),
    ))
}

/// The cumulative totals after `day` days: 600 visitors per arm per day; A converts 4% and
/// loses 0.5% to unsubscribes, B converts 5% and loses `b_unsubscribes_per_day` per 600.
fn day(day: u64, b_unsubscribes_per_day: u64) -> Look {
    let visitors = VISITORS_PER_DAY * day;
    Look::new()
        .with_metric(
            "signups",
            Sample::proportion(visitors, 24 * day),
            Sample::proportion(visitors, 30 * day),
        )
        .with_metric(
            "unsubscribes",
            Sample::proportion(visitors, 3 * day),
            Sample::proportion(visitors, b_unsubscribes_per_day * day),
        )
}

/// Runs daily looks for four weeks and returns every report.
fn four_weeks(
    analyzer: &mut ExperimentAnalyzer,
    b_unsubscribes_per_day: u64,
) -> Vec<ExperimentReport> {
    (1..=28).map(|n| analyzer.observe(&day(n, b_unsubscribes_per_day)).unwrap()).collect()
}

#[test]
fn s11_guardrail_breach_stops_the_experiment_and_keeps_a() {
    let mut analyzer = ExperimentAnalyzer::new(headline_experiment()).unwrap();
    let reports = four_weeks(&mut analyzer, 6);

    // The first ten days continue; day 11 breaches the unsubscribe guardrail.
    assert!(reports[..10].iter().all(|report| report.decision == Decision::Continue));
    let stop = &reports[10];
    assert_eq!(stop.look, 11);
    assert_eq!(
        stop.decision,
        Decision::KeepControl {
            reason: StopReason::GuardrailBreach { guardrails: vec!["unsubscribes".into()] }
        }
    );

    // The lift is reported with its interval: +1 point (+25%), not yet conclusive.
    let effect = stop.primary.effect;
    assert!((effect.difference - 0.01).abs() < 1e-12);
    let interval = effect.difference_interval.unwrap();
    assert!(interval.contains(0.01) && interval.contains(0.0));
    assert!((interval.lower - -0.00101).abs() < 1e-5 && (interval.upper - 0.02101).abs() < 1e-5);
    assert!((effect.relative_lift.unwrap() - 0.25).abs() < 1e-12);
    assert!(effect.relative_interval.unwrap().contains(0.25));
    let Evidence::Sequential { p_value, alpha, .. } = stop.primary.evidence else {
        panic!("the default analysis is sequential");
    };
    assert!(p_value > alpha);

    // The guardrail has proven harm beyond its 10% tolerance.
    let guardrail = &stop.guardrails[0];
    assert!(guardrail.breached);
    assert!(guardrail.p_value <= guardrail.alpha);
    assert!(guardrail.harm_lower_bound.unwrap() > 0.0);
    assert_eq!(guardrail.effect.relative_lift.map(|lift| (lift * 100.0).round()), Some(100.0));

    // The experiment stays stopped with A kept: every later look returns the stopping report.
    assert!(reports[11..].iter().all(|report| report == stop));
    assert!(analyzer.is_stopped());
}

#[test]
fn s11_without_the_harm_the_same_lift_is_adopted() {
    // The counterfactual: B's unsubscribes stay at A's rate. Only the guardrail stopped S11.
    let mut analyzer = ExperimentAnalyzer::new(headline_experiment()).unwrap();
    let reports = four_weeks(&mut analyzer, 3);
    let first_final = reports.iter().find(|report| report.decision.is_final()).unwrap();
    assert_eq!(first_final.decision, Decision::AdoptVariant);
    assert_eq!(first_final.look, 14);
    assert!(first_final.primary.effect.difference_interval.unwrap().lower > 0.0);
    assert!(first_final.guardrails.iter().all(|guardrail| !guardrail.breached));
}

#[test]
fn s11_decision_and_evidence_for_the_weekly_review() {
    let mut analyzer = ExperimentAnalyzer::new(headline_experiment()).unwrap();
    let stop = four_weeks(&mut analyzer, 6).remove(10);

    // The evidence block the weekly review shows.
    assert_eq!(
        stop.to_string(),
        "Experiment 'headline', look 11: keep the control — guardrail breached: unsubscribes\n\
         Primary signups (higher is better): control 4.00% of 6600, variant 5.00% of 6600\n  \
         difference +1.00 pp, 95% interval [-0.10 pp, +2.10 pp]; relative lift +25.0%, 95% \
         interval [-5.8%, +55.8%]\n  \
         always-valid p 0.0992 against alpha 0.05; z 2.77 against 3.05\n\
         Guardrail unsubscribes (lower is better, tolerance 10.0% relative): control 0.50% of \
         6600, variant 1.00% of 6600\n  \
         difference +0.50 pp, 90% interval [+0.08 pp, +0.92 pp]; relative lift +100.0%, 90% \
         interval [-18.3%, +218.3%]\n  \
         harm beyond tolerance +0.45 pp, lower bound +0.02 pp; always-valid p 0.0769 against \
         alpha 0.1000: breached\n"
    );

    // The structured record the review and the job's audit trail keep.
    let record = serde_json::to_value(&stop).unwrap();
    assert_eq!(record["experimentId"], json!("headline"));
    assert_eq!(
        record["decision"],
        json!({
            "outcome": "keepControl",
            "reason": { "kind": "guardrailBreach", "guardrails": ["unsubscribes"] }
        })
    );
    assert_eq!(record["primary"]["evidence"]["method"], json!("sequential"));
    assert_eq!(record["guardrails"][0]["breached"], json!(true));
    let round_trip: ExperimentReport = serde_json::from_value(record).unwrap();
    assert_eq!(round_trip, stop);

    // A process that saved the state after the stop resumes it stopped, keeping A.
    let saved = serde_json::to_string(analyzer.state()).unwrap();
    let state: ExperimentState = serde_json::from_str(&saved).unwrap();
    let mut resumed = ExperimentAnalyzer::resume(headline_experiment(), state).unwrap();
    assert!(resumed.is_stopped());
    let after = resumed.observe(&day(29, 6)).unwrap();
    assert_eq!((after.look, after.decision), (stop.look, stop.decision));
}
