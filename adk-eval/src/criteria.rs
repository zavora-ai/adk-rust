//! Evaluation criteria definitions
//!
//! Defines the various criteria that can be used to evaluate agent responses.

use serde::{Deserialize, Serialize};

use crate::vision::VisionCriteria;

/// Collection of evaluation criteria
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluationCriteria {
    /// Tool trajectory matching score threshold (0.0 - 1.0)
    /// Checks if the agent called the expected tools in the expected order
    #[serde(default)]
    pub tool_trajectory_score: Option<f64>,

    /// Tool trajectory configuration
    #[serde(default)]
    pub tool_trajectory_config: Option<ToolTrajectoryConfig>,

    /// Response text similarity threshold (0.0 - 1.0)
    /// Uses text similarity metrics to compare expected vs actual response
    #[serde(default)]
    pub response_similarity: Option<f64>,

    /// Response matching configuration
    #[serde(default)]
    pub response_match_config: Option<ResponseMatchConfig>,

    /// LLM-judged semantic match threshold (0.0 - 1.0)
    /// Uses an LLM to judge if responses are semantically equivalent
    #[serde(default)]
    pub semantic_match_score: Option<f64>,

    /// Semantic match configuration
    #[serde(default)]
    pub semantic_match_config: Option<SemanticMatchConfig>,

    /// Rubric-based quality score threshold (0.0 - 1.0)
    /// Evaluates response quality against defined rubrics
    #[serde(default)]
    pub rubric_quality_score: Option<f64>,

    /// Rubric configuration
    #[serde(default)]
    pub rubric_config: Option<RubricConfig>,

    /// Safety score threshold (0.0 - 1.0)
    /// Checks for unsafe or harmful content
    #[serde(default)]
    pub safety_score: Option<f64>,

    /// Hallucination detection threshold (0.0 - 1.0)
    /// Detects factual inaccuracies or made-up information
    #[serde(default)]
    pub hallucination_score: Option<f64>,

    /// Vision scores — regions, masks, OCR text, fields, detections and click targets —
    /// checked against each turn's [`expected_vision`](crate::Turn::expected_vision); see
    /// [`vision`](crate::vision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<VisionCriteria>,

    /// Criteria your own code scores; carried with the configuration for it.
    ///
    /// [`Evaluator`](crate::Evaluator) does not score them, so they do not count toward
    /// [`has_criteria`](Self::has_criteria): a configuration with only custom criteria is
    /// rejected like an empty one.
    #[serde(default)]
    pub custom: Vec<CustomCriterion>,
}

impl EvaluationCriteria {
    /// Create criteria requiring exact tool trajectory match
    pub fn exact_tools() -> Self {
        Self {
            tool_trajectory_score: Some(1.0),
            tool_trajectory_config: Some(ToolTrajectoryConfig {
                strict_order: true,
                strict_args: true,
            }),
            ..Default::default()
        }
    }

    /// Create criteria for semantic response matching
    pub fn semantic_match(threshold: f64) -> Self {
        Self { semantic_match_score: Some(threshold), ..Default::default() }
    }

    /// Create criteria with response similarity
    pub fn response_similarity(threshold: f64) -> Self {
        Self { response_similarity: Some(threshold), ..Default::default() }
    }

    /// Add tool trajectory requirement
    pub fn with_tool_trajectory(mut self, threshold: f64) -> Self {
        self.tool_trajectory_score = Some(threshold);
        self
    }

    /// Add response similarity requirement
    pub fn with_response_similarity(mut self, threshold: f64) -> Self {
        self.response_similarity = Some(threshold);
        self
    }

    /// Add semantic match requirement
    pub fn with_semantic_match(mut self, threshold: f64) -> Self {
        self.semantic_match_score = Some(threshold);
        self
    }

    /// Add rubric-based evaluation
    pub fn with_rubrics(mut self, threshold: f64, rubrics: Vec<Rubric>) -> Self {
        self.rubric_quality_score = Some(threshold);
        self.rubric_config = Some(RubricConfig { rubrics });
        self
    }

    /// Add vision thresholds.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_eval::EvaluationCriteria;
    /// use adk_eval::vision::VisionCriteria;
    ///
    /// let criteria = EvaluationCriteria::default().with_vision(VisionCriteria {
    ///     region_iou: Some(0.75),
    ///     ocr_char_accuracy: Some(0.98),
    ///     ..Default::default()
    /// });
    /// assert!(criteria.has_criteria());
    /// ```
    pub fn with_vision(mut self, vision: VisionCriteria) -> Self {
        self.vision = Some(vision);
        self
    }

    /// Whether any criterion [`Evaluator`](crate::Evaluator) scores has a threshold.
    ///
    /// [`custom`](Self::custom) criteria are not counted, since the evaluator does not score
    /// them.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_eval::EvaluationCriteria;
    /// use adk_eval::criteria::CustomCriterion;
    ///
    /// let custom_only = EvaluationCriteria {
    ///     custom: vec![CustomCriterion {
    ///         name: "tone".to_string(),
    ///         description: "The reply is friendly".to_string(),
    ///         threshold: 0.8,
    ///         config: serde_json::Value::Null,
    ///     }],
    ///     ..Default::default()
    /// };
    /// assert!(!custom_only.has_criteria());
    /// assert!(custom_only.with_response_similarity(0.8).has_criteria());
    /// ```
    pub fn has_criteria(&self) -> bool {
        self.tool_trajectory_score.is_some()
            || self.response_similarity.is_some()
            || self.semantic_match_score.is_some()
            || self.rubric_quality_score.is_some()
            || self.safety_score.is_some()
            || self.hallucination_score.is_some()
            || self.vision.as_ref().is_some_and(VisionCriteria::has_thresholds)
    }
}

/// Configuration for tool trajectory matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTrajectoryConfig {
    /// Require tools to be called in the expected order (default `true`).
    ///
    /// Applies to both [`ToolTrajectoryScorer::score`](crate::ToolTrajectoryScorer::score)
    /// and [`ToolTrajectoryScorer::compare`](crate::ToolTrajectoryScorer::compare).
    #[serde(default = "default_true")]
    pub strict_order: bool,
    /// Require exact argument match instead of a subset match (default `false`).
    ///
    /// With `false`, expected arguments match when every expected key is present in the
    /// actual call with a matching value, recursively; extra keys are allowed at any depth.
    /// With `true`, objects must have the same keys at every depth. In both modes arrays
    /// match element by element, numbers compare by value (`1` matches `1.0`), and an
    /// expected tool use without `args` matches any arguments. See
    /// [`ToolUse::matches`](crate::ToolUse::matches).
    #[serde(default)]
    pub strict_args: bool,
}

impl Default for ToolTrajectoryConfig {
    fn default() -> Self {
        Self { strict_order: true, strict_args: false }
    }
}

/// Configuration for response matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMatchConfig {
    /// Similarity algorithm to use
    #[serde(default)]
    pub algorithm: SimilarityAlgorithm,
    /// Whether to normalize text before comparison
    #[serde(default = "default_true")]
    pub normalize: bool,
    /// Whether to ignore case
    #[serde(default = "default_true")]
    pub ignore_case: bool,
    /// Whether to ignore punctuation
    #[serde(default)]
    pub ignore_punctuation: bool,
}

impl Default for ResponseMatchConfig {
    fn default() -> Self {
        Self {
            algorithm: SimilarityAlgorithm::default(),
            normalize: true,
            ignore_case: true,
            ignore_punctuation: false,
        }
    }
}

/// Similarity algorithms for text comparison
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityAlgorithm {
    /// Exact string match
    Exact,
    /// Contains check
    Contains,
    /// Levenshtein distance based
    Levenshtein,
    /// Jaccard similarity (word overlap)
    #[default]
    Jaccard,
    /// ROUGE-1 (unigram overlap)
    Rouge1,
    /// ROUGE-2 (bigram overlap)
    Rouge2,
    /// ROUGE-L (longest common subsequence)
    RougeL,
}

/// Configuration for LLM-judged semantic matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticMatchConfig {
    /// Model to use for judging
    #[serde(default = "default_judge_model")]
    pub judge_model: String,
    /// Custom prompt for the judge (optional)
    ///
    /// `{expected}` and `{actual}` are replaced with the two responses. The prompt must
    /// ask for an `EQUIVALENT: YES/NO/PARTIAL` line and a `SCORE:` line from 0.0 to 1.0;
    /// a reply without either is a judge error.
    pub custom_prompt: Option<String>,
}

impl Default for SemanticMatchConfig {
    fn default() -> Self {
        Self { judge_model: default_judge_model(), custom_prompt: None }
    }
}

fn default_judge_model() -> String {
    "gemini-3.8-flash".to_string()
}

/// Configuration for rubric-based evaluation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RubricConfig {
    /// List of rubrics to evaluate against
    pub rubrics: Vec<Rubric>,
}

impl RubricConfig {
    /// A rubric with one criterion per item, each named by its own text, as the
    /// [`OutcomeGrader`](crate::OutcomeGrader) grades them. Blank items are skipped.
    ///
    /// # Example
    ///
    /// ```rust
    /// let rubric = adk_eval::RubricConfig::from_criteria(["poem.md has three lines", "it rhymes"]);
    /// assert_eq!(rubric.rubrics.len(), 2);
    /// assert_eq!(rubric.rubrics[1].description, "it rhymes");
    /// ```
    pub fn from_criteria<I, S>(criteria: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let rubrics = criteria
            .into_iter()
            .map(|criterion| criterion.as_ref().trim().to_string())
            .filter(|criterion| !criterion.is_empty())
            .map(|criterion| Rubric::new(&criterion, &criterion))
            .collect();
        Self { rubrics }
    }

    /// Reads criteria from Markdown: each list item (`-`, `*`, `+`, `1.`, `1)`, with or
    /// without a `[ ]` checkbox) is one criterion, and indented lines below an item continue
    /// it. Without list items, each paragraph is one criterion. Headings are ignored.
    ///
    /// # Example
    ///
    /// ```rust
    /// let rubric = adk_eval::RubricConfig::from_markdown(
    ///     "# Rubric\n\n- `poem.md` has exactly three lines\n- The poem mentions ownership,\n  borrowing or lifetimes\n1. [ ] It has a title line\n",
    /// );
    /// let criteria: Vec<&str> = rubric.rubrics.iter().map(|r| r.description.as_str()).collect();
    /// assert_eq!(
    ///     criteria,
    ///     [
    ///         "`poem.md` has exactly three lines",
    ///         "The poem mentions ownership, borrowing or lifetimes",
    ///         "It has a title line",
    ///     ]
    /// );
    /// ```
    pub fn from_markdown(text: &str) -> Self {
        let mut items: Vec<String> = Vec::new();
        let mut in_item = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                in_item = false;
                continue;
            }
            let indented = line.starts_with([' ', '\t']);
            if let Some(item) = list_item(trimmed).filter(|_| !(indented && in_item)) {
                items.push(item.to_string());
                in_item = true;
            } else if indented
                && in_item
                && let Some(current) = items.last_mut()
            {
                current.push(' ');
                current.push_str(list_item(trimmed).unwrap_or(trimmed));
            } else {
                in_item = false;
            }
        }
        if items.is_empty() {
            let mut paragraph = String::new();
            for line in text.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    items.extend((!paragraph.is_empty()).then(|| std::mem::take(&mut paragraph)));
                } else {
                    if !paragraph.is_empty() {
                        paragraph.push(' ');
                    }
                    paragraph.push_str(line);
                }
            }
            items.extend((!paragraph.is_empty()).then_some(paragraph));
        }
        Self::from_criteria(items)
    }
}

/// The text of a Markdown list item, without its marker and checkbox.
fn list_item(line: &str) -> Option<&str> {
    let rest = if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        rest
    } else {
        let digits = line.bytes().take_while(u8::is_ascii_digit).count();
        let marker =
            line[digits..].strip_prefix(". ").or_else(|| line[digits..].strip_prefix(") "));
        match marker {
            Some(rest) if digits > 0 => rest,
            _ => return None,
        }
    };
    let rest = rest.trim_start();
    let rest = ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find_map(|checkbox| rest.strip_prefix(checkbox))
        .unwrap_or(rest);
    Some(rest.trim())
}

/// A single rubric for quality assessment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rubric {
    /// Rubric name
    pub name: String,
    /// What this rubric measures
    pub description: String,
    /// Weight for this rubric (0.0 - 1.0)
    #[serde(default = "default_weight")]
    pub weight: f64,
    /// Scoring levels (optional)
    #[serde(default)]
    pub levels: Vec<RubricLevel>,
}

impl Rubric {
    /// Create a new rubric
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            weight: 1.0,
            levels: vec![],
        }
    }

    /// Set weight
    pub fn with_weight(mut self, weight: f64) -> Self {
        self.weight = weight;
        self
    }

    /// Add scoring levels
    pub fn with_levels(mut self, levels: Vec<RubricLevel>) -> Self {
        self.levels = levels;
        self
    }
}

/// A scoring level for a rubric
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricLevel {
    /// Score for this level (0.0 - 1.0)
    pub score: f64,
    /// Description of what qualifies for this level
    pub description: String,
}

/// Custom evaluation criterion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCriterion {
    /// Criterion name
    pub name: String,
    /// Description of what this measures
    pub description: String,
    /// Score threshold (0.0 - 1.0)
    pub threshold: f64,
    /// Custom configuration as JSON
    #[serde(default)]
    pub config: serde_json::Value,
}

fn default_true() -> bool {
    true
}

fn default_weight() -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_criteria_builder() {
        let criteria = EvaluationCriteria::exact_tools()
            .with_response_similarity(0.8)
            .with_semantic_match(0.9);

        assert_eq!(criteria.tool_trajectory_score, Some(1.0));
        assert_eq!(criteria.response_similarity, Some(0.8));
        assert_eq!(criteria.semantic_match_score, Some(0.9));
        assert!(criteria.has_criteria());
    }

    #[test]
    fn test_rubric_creation() {
        let rubric = Rubric::new("Accuracy", "Response is factually correct")
            .with_weight(0.7)
            .with_levels(vec![
                RubricLevel { score: 1.0, description: "Completely accurate".to_string() },
                RubricLevel { score: 0.5, description: "Partially accurate".to_string() },
                RubricLevel { score: 0.0, description: "Inaccurate".to_string() },
            ]);

        assert_eq!(rubric.name, "Accuracy");
        assert_eq!(rubric.weight, 0.7);
        assert_eq!(rubric.levels.len(), 3);
    }

    #[test]
    fn test_default_criteria() {
        let criteria = EvaluationCriteria::default();
        assert!(!criteria.has_criteria());
    }

    #[test]
    fn custom_criteria_alone_are_not_criteria() {
        let criteria = EvaluationCriteria {
            custom: vec![CustomCriterion {
                name: "tone".to_string(),
                description: "friendly".to_string(),
                threshold: 0.5,
                config: serde_json::Value::Null,
            }],
            ..Default::default()
        };
        assert!(!criteria.has_criteria());
    }
}
