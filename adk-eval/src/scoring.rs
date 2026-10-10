//! Scoring implementations for evaluation criteria
//!
//! Provides various scorers for tool trajectory, response similarity, etc.

#![allow(clippy::needless_range_loop)] // Intentional for DP algorithms

use crate::criteria::{ResponseMatchConfig, SimilarityAlgorithm, ToolTrajectoryConfig};
use crate::schema::ToolUse;
use std::collections::HashSet;

/// Unicode-aware text tokenizer for scoring.
///
/// For whitespace-delimited languages (English, etc.), splits on whitespace.
/// For CJK text (Chinese, Japanese, Korean) which has no word-separating
/// whitespace, emits each character as an individual token.
/// This enables meaningful Jaccard, ROUGE, and n-gram scoring across all languages.
fn unicode_tokenize(text: &str) -> impl Iterator<Item = &str> {
    UnicodeTokenizer { text, pos: 0 }
}

struct UnicodeTokenizer<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Iterator for UnicodeTokenizer<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        // Skip whitespace
        while self.pos < self.text.len() {
            if self.text[self.pos..].starts_with(char::is_whitespace) {
                self.pos += self.text[self.pos..].chars().next().unwrap().len_utf8();
            } else {
                break;
            }
        }

        if self.pos >= self.text.len() {
            return None;
        }

        let start = self.pos;
        let c = self.text[start..].chars().next().unwrap();

        // CJK character: emit as a single-character token
        if is_cjk_char(c) {
            self.pos += c.len_utf8();
            return Some(&self.text[start..self.pos]);
        }

        // Non-CJK: accumulate until whitespace or CJK
        while self.pos < self.text.len() {
            let ch = self.text[self.pos..].chars().next().unwrap();
            if ch.is_whitespace() || is_cjk_char(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }

        Some(&self.text[start..self.pos])
    }
}

/// Returns true if the character is in a CJK script block.
fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4dbf}' // CJK Extension A
        | '\u{f900}'..='\u{faff}' // CJK Compatibility
        | '\u{3040}'..='\u{309f}' // Hiragana
        | '\u{30a0}'..='\u{30ff}' // Katakana
        | '\u{ac00}'..='\u{d7af}' // Hangul Syllables
    )
}

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
    /// Returns a score from 0.0 to 1.0: the number of matched calls divided by the
    /// length of the longer of the two trajectories.
    pub fn score(&self, expected: &[ToolUse], actual: &[ToolUse]) -> f64 {
        if expected.is_empty() && actual.is_empty() {
            return 1.0;
        }
        let matches = self.match_pairs(expected, actual).len();
        matches as f64 / expected.len().max(actual.len()) as f64
    }

    /// Pairs each matched expected call with the actual call it matched, as indices.
    ///
    /// With `strict_order`, each match must come after the previous match in `actual`;
    /// otherwise each expected call takes the first unmatched actual call that satisfies it.
    fn match_pairs(&self, expected: &[ToolUse], actual: &[ToolUse]) -> Vec<(usize, usize)> {
        let strict_args = self.config.strict_args;
        let mut pairs = Vec::new();

        if self.config.strict_order {
            let mut next_actual = 0;
            for (exp_idx, exp) in expected.iter().enumerate() {
                if let Some(offset) =
                    actual[next_actual..].iter().position(|act| exp.matches(act, strict_args))
                {
                    pairs.push((exp_idx, next_actual + offset));
                    next_actual += offset + 1;
                }
            }
        } else {
            let mut used = vec![false; actual.len()];
            for (exp_idx, exp) in expected.iter().enumerate() {
                if let Some(act_idx) =
                    (0..actual.len()).find(|&i| !used[i] && exp.matches(&actual[i], strict_args))
                {
                    used[act_idx] = true;
                    pairs.push((exp_idx, act_idx));
                }
            }
        }

        pairs
    }

    /// Get detailed comparison
    ///
    /// Uses the same matching as [`score`](Self::score), so `strict_order` decides
    /// which calls count as matched, missing, or extra.
    pub fn compare(&self, expected: &[ToolUse], actual: &[ToolUse]) -> ToolTrajectoryComparison {
        let pairs = self.match_pairs(expected, actual);
        let mut expected_matched = vec![false; expected.len()];
        let mut actual_matched = vec![false; actual.len()];
        let mut matched = Vec::with_capacity(pairs.len());
        for (exp_idx, act_idx) in pairs {
            expected_matched[exp_idx] = true;
            actual_matched[act_idx] = true;
            matched.push((expected[exp_idx].clone(), actual[act_idx].clone()));
        }

        let missing = expected
            .iter()
            .zip(&expected_matched)
            .filter(|(_, is_matched)| !**is_matched)
            .map(|(tool, _)| tool.clone())
            .collect();
        let extra = actual
            .iter()
            .zip(&actual_matched)
            .filter(|(_, is_matched)| !**is_matched)
            .map(|(tool, _)| tool.clone())
            .collect();

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
                if actual.contains(&expected) || expected.contains(&actual) { 1.0 } else { 0.0 }
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
        let max_len = a.chars().count().max(b.chars().count());
        if max_len == 0 { 1.0 } else { 1.0 - (distance as f64 / max_len as f64) }
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
        let a_words: HashSet<&str> = unicode_tokenize(a).collect();
        let b_words: HashSet<&str> = unicode_tokenize(b).collect();

        if a_words.is_empty() && b_words.is_empty() {
            return 1.0;
        }

        let intersection = a_words.intersection(&b_words).count();
        let union = a_words.union(&b_words).count();

        if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
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
    fn get_ngrams(&self, text: &str, n: usize) -> HashSet<Vec<String>> {
        let words: Vec<String> = unicode_tokenize(text).map(|s| s.to_string()).collect();
        if words.len() < n {
            return HashSet::new();
        }

        words.windows(n).map(|w| w.to_vec()).collect()
    }

    /// ROUGE-L score (longest common subsequence)
    fn rouge_l(&self, reference: &str, candidate: &str) -> f64 {
        let ref_words: Vec<&str> = unicode_tokenize(reference).collect();
        let cand_words: Vec<&str> = unicode_tokenize(candidate).collect();

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
    fn compare_honours_strict_order() {
        let expected = vec![ToolUse::new("tool_a"), ToolUse::new("tool_b")];
        let actual = vec![ToolUse::new("tool_b"), ToolUse::new("tool_a")];

        let ordered = ToolTrajectoryScorer::new().compare(&expected, &actual);
        assert_eq!(ordered.matched.len(), 1);
        assert_eq!(ordered.matched[0].0.name, "tool_a");
        assert_eq!(
            ordered.missing.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["tool_b"]
        );
        assert_eq!(
            ordered.extra.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["tool_b"]
        );
        assert_eq!(ordered.score, 0.5);

        let unordered = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
            strict_order: false,
            strict_args: false,
        })
        .compare(&expected, &actual);
        assert_eq!(unordered.matched.len(), 2);
        assert!(unordered.missing.is_empty());
        assert!(unordered.extra.is_empty());
        assert_eq!(unordered.score, 1.0);
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
