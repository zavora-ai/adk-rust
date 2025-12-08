//! Scoring implementations for evaluation criteria
//!
//! Provides various scorers for tool trajectory, response similarity, etc.

#![allow(clippy::needless_range_loop)] // Intentional for DP algorithms

use crate::criteria::{ResponseMatchConfig, SimilarityAlgorithm, ToolTrajectoryConfig};
use crate::schema::ToolUse;
use std::collections::HashSet;

/// Scorer for tool trajectory matching
pub struct ToolTrajectoryScorer {
    config: ToolTrajectoryConfig,
}

impl ToolTrajectoryScorer {
    /// Create a new scorer with default config
    pub fn new() -> Self {
        Self { config: ToolTrajectoryConfig::default() }
    }

    /// Create with custom config
    pub fn with_config(config: ToolTrajectoryConfig) -> Self {
        Self { config }
    }

    /// Score tool trajectory
    ///
    /// Returns a score from 0.0 to 1.0 indicating how well the actual
    /// tool calls match the expected tool calls.
    pub fn score(&self, expected: &[ToolUse], actual: &[ToolUse]) -> f64 {
        if expected.is_empty() && actual.is_empty() {
            return 1.0;
        }

        if expected.is_empty() || actual.is_empty() {
            return 0.0;
        }

        if self.config.strict_order {
            self.score_ordered(expected, actual)
        } else {
            self.score_unordered(expected, actual)
        }
    }

    /// Score with strict ordering
    fn score_ordered(&self, expected: &[ToolUse], actual: &[ToolUse]) -> f64 {
        let mut matches = 0;
        let mut exp_idx = 0;
        let mut act_idx = 0;

        while exp_idx < expected.len() && act_idx < actual.len() {
            if expected[exp_idx].matches(&actual[act_idx], self.config.strict_args) {
                matches += 1;
                exp_idx += 1;
                act_idx += 1;
            } else {
                // Try to find the expected tool in remaining actual calls
                let mut found = false;
                for i in (act_idx + 1)..actual.len() {
                    if expected[exp_idx].matches(&actual[i], self.config.strict_args) {
                        matches += 1;
                        exp_idx += 1;
                        act_idx = i + 1;
                        found = true;
                        break;
                    }
                }
                if !found {
                    exp_idx += 1;
                }
            }
        }

        let max_len = expected.len().max(actual.len());
        matches as f64 / max_len as f64
    }

    /// Score without strict ordering (set comparison)
    fn score_unordered(&self, expected: &[ToolUse], actual: &[ToolUse]) -> f64 {
        let mut matched_actual: HashSet<usize> = HashSet::new();
        let mut matches = 0;

        for exp in expected {
            for (i, act) in actual.iter().enumerate() {
                if !matched_actual.contains(&i) && exp.matches(act, self.config.strict_args) {
                    matches += 1;
                    matched_actual.insert(i);
                    break;
                }
            }
        }

        let max_len = expected.len().max(actual.len());
        matches as f64 / max_len as f64
    }

    /// Get detailed comparison
    pub fn compare(&self, expected: &[ToolUse], actual: &[ToolUse]) -> ToolTrajectoryComparison {
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        let mut extra = Vec::new();
        let mut matched_actual: HashSet<usize> = HashSet::new();

        for exp in expected {
            let mut found = false;
            for (i, act) in actual.iter().enumerate() {
                if !matched_actual.contains(&i) && exp.matches(act, self.config.strict_args) {
                    matched.push((exp.clone(), act.clone()));
                    matched_actual.insert(i);
                    found = true;
                    break;
                }
            }
            if !found {
                missing.push(exp.clone());
            }
        }

        for (i, act) in actual.iter().enumerate() {
            if !matched_actual.contains(&i) {
                extra.push(act.clone());
            }
        }

        ToolTrajectoryComparison { matched, missing, extra, score: self.score(expected, actual) }
    }
}

impl Default for ToolTrajectoryScorer {
    fn default() -> Self {
        Self::new()
    }
}

/// Detailed comparison of tool trajectories
#[derive(Debug, Clone)]
pub struct ToolTrajectoryComparison {
    /// Tools that matched
    pub matched: Vec<(ToolUse, ToolUse)>,
    /// Expected tools that weren't called
    pub missing: Vec<ToolUse>,
    /// Actual tools that weren't expected
    pub extra: Vec<ToolUse>,
    /// Overall score
    pub score: f64,
}

/// Scorer for response text similarity
pub struct ResponseScorer {
    config: ResponseMatchConfig,
}

impl ResponseScorer {
    /// Create a new scorer with default config
    pub fn new() -> Self {
        Self { config: ResponseMatchConfig::default() }
    }

    /// Create with custom config
    pub fn with_config(config: ResponseMatchConfig) -> Self {
        Self { config }
    }

    /// Score response similarity
    pub fn score(&self, expected: &str, actual: &str) -> f64 {
        let (expected, actual) = if self.config.normalize {
            (self.normalize(expected), self.normalize(actual))
        } else {
            (expected.to_string(), actual.to_string())
        };

        match self.config.algorithm {
            SimilarityAlgorithm::Exact => {
                if expected == actual {
                    1.0
                } else {
                    0.0
                }
            }
            SimilarityAlgorithm::Contains => {
                if actual.contains(&expected) || expected.contains(&actual) {
                    1.0
                } else {
                    0.0
                }
            }
            SimilarityAlgorithm::Levenshtein => self.levenshtein_similarity(&expected, &actual),
            SimilarityAlgorithm::Jaccard => self.jaccard_similarity(&expected, &actual),
            SimilarityAlgorithm::Rouge1 => self.rouge_n(&expected, &actual, 1),
            SimilarityAlgorithm::Rouge2 => self.rouge_n(&expected, &actual, 2),
            SimilarityAlgorithm::RougeL => self.rouge_l(&expected, &actual),
        }
    }

    /// Normalize text for comparison
    fn normalize(&self, text: &str) -> String {
        let mut result = text.to_string();

        if self.config.ignore_case {
            result = result.to_lowercase();
        }

        if self.config.ignore_punctuation {
            result = result.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
        }

        // Normalize whitespace
        result.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Levenshtein distance based similarity
    fn levenshtein_similarity(&self, a: &str, b: &str) -> f64 {
        let distance = self.levenshtein_distance(a, b);
        let max_len = a.len().max(b.len());
        if max_len == 0 {
            1.0
        } else {
            1.0 - (distance as f64 / max_len as f64)
        }
    }

    /// Calculate Levenshtein distance
    fn levenshtein_distance(&self, a: &str, b: &str) -> usize {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let m = a_chars.len();
        let n = b_chars.len();

        if m == 0 {
            return n;
        }
        if n == 0 {
            return m;
        }

        let mut dp = vec![vec![0; n + 1]; m + 1];

        for i in 0..=m {
            dp[i][0] = i;
        }
        for j in 0..=n {
            dp[0][j] = j;
        }

        for i in 1..=m {
            for j in 1..=n {
                let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
                dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
            }
        }

        dp[m][n]
    }

    /// Jaccard similarity (word overlap)
    fn jaccard_similarity(&self, a: &str, b: &str) -> f64 {
        let a_words: HashSet<&str> = a.split_whitespace().collect();
        let b_words: HashSet<&str> = b.split_whitespace().collect();

        if a_words.is_empty() && b_words.is_empty() {
            return 1.0;
        }

        let intersection = a_words.intersection(&b_words).count();
        let union = a_words.union(&b_words).count();

        if union == 0 {
            0.0
        } else {
            intersection as f64 / union as f64
        }
    }

    /// ROUGE-N score (n-gram overlap)
    fn rouge_n(&self, reference: &str, candidate: &str, n: usize) -> f64 {
        let ref_ngrams = self.get_ngrams(reference, n);
        let cand_ngrams = self.get_ngrams(candidate, n);

        if ref_ngrams.is_empty() {
            return if cand_ngrams.is_empty() { 1.0 } else { 0.0 };
        }

        let overlap = ref_ngrams.intersection(&cand_ngrams).count();
        overlap as f64 / ref_ngrams.len() as f64
    }

    /// Get n-grams from text
    fn get_ngrams<'a>(&self, text: &'a str, n: usize) -> HashSet<Vec<&'a str>> {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() < n {
            return HashSet::new();
        }

        words.windows(n).map(|w| w.to_vec()).collect()
    }

    /// ROUGE-L score (longest common subsequence)
    fn rouge_l(&self, reference: &str, candidate: &str) -> f64 {
        let ref_words: Vec<&str> = reference.split_whitespace().collect();
        let cand_words: Vec<&str> = candidate.split_whitespace().collect();

        if ref_words.is_empty() {
            return if cand_words.is_empty() { 1.0 } else { 0.0 };
        }

        let lcs_len = self.lcs_length(&ref_words, &cand_words);

        // F1 score of precision and recall
        let precision =
            if cand_words.is_empty() { 0.0 } else { lcs_len as f64 / cand_words.len() as f64 };
        let recall = lcs_len as f64 / ref_words.len() as f64;

        if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        }
    }

    /// Length of longest common subsequence
    fn lcs_length(&self, a: &[&str], b: &[&str]) -> usize {
        let m = a.len();
        let n = b.len();

        if m == 0 || n == 0 {
            return 0;
        }

        let mut dp = vec![vec![0; n + 1]; m + 1];

        for i in 1..=m {
            for j in 1..=n {
                if a[i - 1] == b[j - 1] {
                    dp[i][j] = dp[i - 1][j - 1] + 1;
                } else {
                    dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
                }
            }
        }

        dp[m][n]
    }
}

impl Default for ResponseScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tool_trajectory_exact_match() {
        let scorer = ToolTrajectoryScorer::new();

        let expected = vec![
            ToolUse::new("get_weather").with_args(json!({"location": "NYC"})),
            ToolUse::new("get_forecast").with_args(json!({"days": 3})),
        ];

        let actual = vec![
            ToolUse::new("get_weather").with_args(json!({"location": "NYC"})),
            ToolUse::new("get_forecast").with_args(json!({"days": 3})),
        ];

        assert_eq!(scorer.score(&expected, &actual), 1.0);
    }

    #[test]
    fn test_tool_trajectory_partial_match() {
        let scorer = ToolTrajectoryScorer::new();

        let expected = vec![ToolUse::new("tool_a"), ToolUse::new("tool_b")];

        let actual = vec![ToolUse::new("tool_a"), ToolUse::new("tool_c")];

        let score = scorer.score(&expected, &actual);
        assert!(score > 0.0 && score < 1.0);
    }

    #[test]
    fn test_tool_trajectory_unordered() {
        let scorer = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
            strict_order: false,
            strict_args: false,
        });

        let expected = vec![ToolUse::new("tool_a"), ToolUse::new("tool_b")];

        let actual = vec![ToolUse::new("tool_b"), ToolUse::new("tool_a")];

        assert_eq!(scorer.score(&expected, &actual), 1.0);
    }

    #[test]
    fn test_response_exact_match() {
        let scorer = ResponseScorer::with_config(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Exact,
            normalize: true,
            ignore_case: true,
            ignore_punctuation: false,
        });

        assert_eq!(scorer.score("Hello World", "hello world"), 1.0);
        assert_eq!(scorer.score("Hello", "World"), 0.0);
    }

    #[test]
    fn test_response_jaccard() {
        let scorer = ResponseScorer::new();

        let score = scorer.score("the quick brown fox", "the quick brown dog");
        assert!(score > 0.5 && score < 1.0);
    }

    #[test]
    fn test_response_levenshtein() {
        let scorer = ResponseScorer::with_config(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Levenshtein,
            ..Default::default()
        });

        let score = scorer.score("hello", "hallo");
        assert!(score > 0.7);

        let score = scorer.score("abc", "xyz");
        assert!(score < 0.5);
    }

    #[test]
    fn test_rouge_l() {
        let scorer = ResponseScorer::with_config(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::RougeL,
            ..Default::default()
        });

        let score = scorer.score("the cat sat on the mat", "the cat was on the mat");
        assert!(score > 0.7);
    }
}
