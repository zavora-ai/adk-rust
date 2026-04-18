//! Property-based tests for the Prompt Optimizer.
//!
//! Tests the core optimization loop logic using `proptest` to verify
//! that iteration bounds and early stopping are respected across all
//! valid configurations.

use proptest::prelude::*;

use adk_eval::optimizer::run_optimization_loop;

/// Generate a sequence of evaluation scores (1 initial + up to 20 iteration scores).
fn arb_scores() -> impl Strategy<Value = Vec<f64>> {
    prop::collection::vec(0.0..=1.0f64, 1..=21)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 8: Optimizer Respects Iteration Bounds**
    ///
    /// *For any* `max_iterations` in 1..=20 and `target_threshold` in 0.0..=1.0,
    /// and *for any* sequence of evaluation scores, the optimizer runs at most
    /// `max_iterations` iterations and stops early if any score meets or exceeds
    /// `target_threshold`.
    ///
    /// **Validates: Requirements 6.3**
    #[test]
    fn prop_optimizer_respects_iteration_bounds(
        max_iterations in 1u32..=20,
        target_threshold in 0.0..=1.0f64,
        scores in arb_scores(),
    ) {
        let (iterations_run, best_score) = run_optimization_loop(
            &scores,
            max_iterations,
            target_threshold,
        );

        // Property 1: iterations_run <= max_iterations
        prop_assert!(
            iterations_run <= max_iterations,
            "ran {} iterations but max is {}",
            iterations_run,
            max_iterations,
        );

        // Property 2: if initial score meets threshold, zero iterations run
        if scores[0] >= target_threshold {
            prop_assert_eq!(
                iterations_run, 0,
                "initial score {} meets threshold {} but ran {} iterations",
                scores[0], target_threshold, iterations_run,
            );
        }

        // Property 3: if early stop happened (iterations_run < max_iterations and
        // initial score didn't meet threshold), then best_score >= target_threshold
        if iterations_run > 0 && iterations_run < max_iterations && scores[0] < target_threshold {
            prop_assert!(
                best_score >= target_threshold,
                "stopped early at iteration {} but best_score {} < threshold {}",
                iterations_run,
                best_score,
                target_threshold,
            );
        }

        // Property 4: best_score is the maximum of all scores seen
        // (initial + scores up to iterations_run)
        let scores_seen: Vec<f64> = (0..=iterations_run as usize)
            .map(|i| {
                if i < scores.len() {
                    scores[i]
                } else {
                    scores[scores.len() - 1]
                }
            })
            .collect();
        let expected_best = scores_seen
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        prop_assert!(
            (best_score - expected_best).abs() < f64::EPSILON,
            "best_score {} != expected max {} from scores {:?}",
            best_score,
            expected_best,
            scores_seen,
        );
    }

    /// Verify that when all scores are below threshold, the optimizer runs
    /// exactly max_iterations.
    ///
    /// **Validates: Requirements 6.3**
    #[test]
    fn prop_optimizer_runs_all_iterations_when_below_threshold(
        max_iterations in 1u32..=20,
        scores in prop::collection::vec(0.0..0.5f64, 1..=21),
    ) {
        // Use a threshold that no score can reach
        let target_threshold = 0.99;

        let (iterations_run, _best_score) = run_optimization_loop(
            &scores,
            max_iterations,
            target_threshold,
        );

        // If initial score is below threshold, should run all iterations
        if scores[0] < target_threshold {
            prop_assert_eq!(
                iterations_run, max_iterations,
                "expected {} iterations but ran {}",
                max_iterations, iterations_run,
            );
        }
    }

    /// Verify that when the first iteration score meets the threshold,
    /// the optimizer stops after exactly 1 iteration.
    ///
    /// **Validates: Requirements 6.3**
    #[test]
    fn prop_optimizer_stops_early_on_first_good_score(
        max_iterations in 2u32..=20,
        initial_score in 0.0..0.5f64,
        good_score in 0.9..=1.0f64,
    ) {
        let scores = vec![initial_score, good_score, 0.1, 0.1, 0.1];
        let target_threshold = 0.9;

        let (iterations_run, best_score) = run_optimization_loop(
            &scores,
            max_iterations,
            target_threshold,
        );

        prop_assert_eq!(
            iterations_run, 1,
            "expected 1 iteration but ran {}",
            iterations_run,
        );
        prop_assert!(
            best_score >= target_threshold,
            "best_score {} should meet threshold {}",
            best_score,
            target_threshold,
        );
    }
}
