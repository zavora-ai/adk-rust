//! A/B agent comparison with statistical significance testing.
//!
//! Runs two agent configurations against the same eval set and applies
//! the Wilcoxon signed-rank test to determine whether performance differences
//! are statistically significant.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::AbComparator;
//!
//! let comparator = AbComparator::new(evaluator);
//! let report = comparator.compare(agent_a, agent_b, &eval_cases).await?;
//! for comparison in &report.criteria_comparisons {
//!     println!("{}: p={:.4}, significant={}", comparison.criterion, comparison.p_value, comparison.significant);
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::evaluator::Evaluator;
use crate::schema::EvalCase;

use adk_core::Agent;
use std::sync::Arc;

/// Result of comparing two agents across an eval set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonReport {
    /// Name of agent A
    pub agent_a_name: String,
    /// Name of agent B
    pub agent_b_name: String,
    /// Per-criterion statistical comparisons
    pub criteria_comparisons: Vec<CriterionComparison>,
    /// Total number of eval cases run
    pub total_cases: usize,
}

/// Statistical comparison for a single criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionComparison {
    /// Criterion name
    pub criterion: String,
    /// Mean score for agent A
    pub agent_a_mean: f64,
    /// Mean score for agent B
    pub agent_b_mean: f64,
    /// Score delta (A mean - B mean)
    pub delta: f64,
    /// p-value from Wilcoxon signed-rank test
    pub p_value: f64,
    /// Whether the difference is statistically significant
    pub significant: bool,
    /// Number of cases agent A won
    pub wins_a: usize,
    /// Number of cases agent B won
    pub wins_b: usize,
    /// Number of tied cases
    pub ties: usize,
}

/// Compares two agents using statistical significance testing.
///
/// Executes both agents against the same eval set, then applies the
/// Wilcoxon signed-rank test to per-case score differences.
pub struct AbComparator {
    evaluator: Evaluator,
    significance_level: f64,
}

impl AbComparator {
    /// Create a new comparator with default significance level (0.05).
    pub fn new(evaluator: Evaluator) -> Self {
        Self { evaluator, significance_level: 0.05 }
    }

    /// Create a comparator with a custom significance level.
    pub fn with_significance_level(evaluator: Evaluator, level: f64) -> Self {
        Self { evaluator, significance_level: level }
    }

    /// Run A/B comparison between two agents.
    ///
    /// Both agents are evaluated against every case in the eval set using
    /// identical inputs. Results are compared using the Wilcoxon signed-rank test.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::StatisticsError`] if evaluation or statistical
    /// computation fails.
    pub async fn compare(
        &self,
        agent_a: Arc<dyn Agent>,
        agent_b: Arc<dyn Agent>,
        eval_cases: &[EvalCase],
    ) -> Result<ComparisonReport> {
        use std::collections::HashMap;

        let agent_a_name = agent_a.name().to_string();
        let agent_b_name = agent_b.name().to_string();

        // Evaluate agent A against all cases
        let results_a: Vec<_> =
            self.evaluator.evaluate_cases_parallel(agent_a, eval_cases, 1).await;

        // Evaluate agent B against all cases
        let results_b: Vec<_> =
            self.evaluator.evaluate_cases_parallel(agent_b, eval_cases, 1).await;

        // Collect per-criterion scores for each agent, keyed by criterion name
        // Each entry maps criterion -> vec of (score_a, score_b) pairs
        let mut criterion_pairs: HashMap<String, Vec<(f64, f64)>> = HashMap::new();

        for (res_a, res_b) in results_a.iter().zip(results_b.iter()) {
            let scores_a = match res_a {
                Ok(r) => &r.scores,
                Err(_) => continue,
            };
            let scores_b = match res_b {
                Ok(r) => &r.scores,
                Err(_) => continue,
            };

            // Collect all criteria from both results
            let mut all_criteria: std::collections::HashSet<&String> = scores_a.keys().collect();
            all_criteria.extend(scores_b.keys());

            for criterion in all_criteria {
                let score_a = scores_a.get(criterion).copied().unwrap_or(0.0);
                let score_b = scores_b.get(criterion).copied().unwrap_or(0.0);
                criterion_pairs.entry(criterion.clone()).or_default().push((score_a, score_b));
            }
        }

        // Compute statistical comparison for each criterion
        let mut criteria_comparisons = Vec::new();

        for (criterion, pairs) in &criterion_pairs {
            let n = pairs.len();
            if n == 0 {
                continue;
            }

            let sum_a: f64 = pairs.iter().map(|(a, _)| a).sum();
            let sum_b: f64 = pairs.iter().map(|(_, b)| b).sum();
            let agent_a_mean = sum_a / n as f64;
            let agent_b_mean = sum_b / n as f64;
            let delta = agent_a_mean - agent_b_mean;

            // Compute differences for Wilcoxon test
            let differences: Vec<f64> = pairs.iter().map(|(a, b)| a - b).collect();

            let p_value = wilcoxon_signed_rank(&differences);
            let significant = p_value < self.significance_level;

            // Count wins, losses, ties
            let mut wins_a = 0usize;
            let mut wins_b = 0usize;
            let mut ties = 0usize;

            for (a, b) in pairs {
                if (a - b).abs() < 1e-10 {
                    ties += 1;
                } else if a > b {
                    wins_a += 1;
                } else {
                    wins_b += 1;
                }
            }

            criteria_comparisons.push(CriterionComparison {
                criterion: criterion.clone(),
                agent_a_mean,
                agent_b_mean,
                delta,
                p_value,
                significant,
                wins_a,
                wins_b,
                ties,
            });
        }

        Ok(ComparisonReport {
            agent_a_name,
            agent_b_name,
            criteria_comparisons,
            total_cases: eval_cases.len(),
        })
    }
}

/// Wilcoxon signed-rank test for paired samples.
///
/// Computes a p-value for the null hypothesis that the median of the
/// paired differences is zero. Uses the normal approximation for the
/// test statistic when the sample size is large enough.
///
/// # Arguments
///
/// * `differences` - Paired score differences (agent A score - agent B score)
///
/// # Returns
///
/// A p-value in \[0.0, 1.0\]. Returns 1.0 if all differences are zero
/// (no evidence against the null hypothesis).
pub fn wilcoxon_signed_rank(differences: &[f64]) -> f64 {
    use statrs::distribution::{ContinuousCDF, Normal};

    // Filter out zero differences
    let non_zero: Vec<f64> = differences.iter().copied().filter(|d| d.abs() > 1e-10).collect();

    if non_zero.is_empty() {
        return 1.0;
    }

    let n = non_zero.len();

    // Rank by absolute value
    let mut abs_ranked: Vec<(usize, f64)> =
        non_zero.iter().enumerate().map(|(i, d)| (i, d.abs())).collect();
    abs_ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    // Assign ranks (handle ties with average rank)
    let mut ranks = vec![0.0_f64; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && (abs_ranked[j].1 - abs_ranked[i].1).abs() < 1e-10 {
            j += 1;
        }
        // Average rank for tied group
        let avg_rank = (i + 1 + j) as f64 / 2.0;
        for item in abs_ranked.iter().take(j).skip(i) {
            ranks[item.0] = avg_rank;
        }
        i = j;
    }

    // Compute W+ (sum of ranks for positive differences)
    let w_plus: f64 =
        non_zero.iter().enumerate().filter(|(_, d)| **d > 0.0).map(|(i, _)| ranks[i]).sum();

    let n_f64 = n as f64;

    // Expected value and variance under null hypothesis
    let expected = n_f64 * (n_f64 + 1.0) / 4.0;
    let variance = n_f64 * (n_f64 + 1.0) * (2.0 * n_f64 + 1.0) / 24.0;

    if variance == 0.0 {
        return 1.0;
    }

    let std_dev = variance.sqrt();

    // Continuity correction
    let z = (w_plus - expected).abs() - 0.5;
    let z = if z < 0.0 { 0.0 } else { z / std_dev };

    // Two-tailed p-value using normal approximation
    let normal = Normal::new(0.0, 1.0).unwrap();
    let p_value = 2.0 * (1.0 - normal.cdf(z));

    p_value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wilcoxon_all_zeros() {
        let diffs = vec![0.0, 0.0, 0.0, 0.0];
        let p = wilcoxon_signed_rank(&diffs);
        assert_eq!(p, 1.0);
    }

    #[test]
    fn test_wilcoxon_empty() {
        let diffs: Vec<f64> = vec![];
        let p = wilcoxon_signed_rank(&diffs);
        assert_eq!(p, 1.0);
    }

    #[test]
    fn test_wilcoxon_significant_difference() {
        // Large consistent positive differences should yield low p-value
        let diffs = vec![0.3, 0.25, 0.4, 0.35, 0.28, 0.32, 0.38, 0.27, 0.31, 0.29];
        let p = wilcoxon_signed_rank(&diffs);
        assert!(p < 0.05, "Expected significant result, got p={p}");
    }

    #[test]
    fn test_wilcoxon_result_in_range() {
        let diffs = vec![0.1, -0.2, 0.05, -0.15, 0.3];
        let p = wilcoxon_signed_rank(&diffs);
        assert!((0.0..=1.0).contains(&p), "p-value {p} out of range");
    }

    #[test]
    fn test_wilcoxon_single_element() {
        let diffs = vec![0.5];
        let p = wilcoxon_signed_rank(&diffs);
        assert!((0.0..=1.0).contains(&p));
    }
}
