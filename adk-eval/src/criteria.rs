//! Evaluation criteria definitions
//!
//! Defines the various criteria that can be used to evaluate agent responses.

use serde::{Deserialize, Serialize};

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

    /// Custom criteria for extensibility
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

    /// Check if any criteria are defined
    pub fn has_criteria(&self) -> bool {
        self.tool_trajectory_score.is_some()
            || self.response_similarity.is_some()
            || self.semantic_match_score.is_some()
            || self.rubric_quality_score.is_some()
            || self.safety_score.is_some()
            || self.hallucination_score.is_some()
            || !self.custom.is_empty()
    }
}

/// Configuration for tool trajectory matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTrajectoryConfig {
    /// Require tools to be called in exact order
    #[serde(default = "default_true")]
    pub strict_order: bool,
    /// Require exact argument match (vs partial)
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
    pub custom_prompt: Option<String>,
}

impl Default for SemanticMatchConfig {
    fn default() -> Self {
        Self { judge_model: default_judge_model(), custom_prompt: None }
    }
}

fn default_judge_model() -> String {
    "gemini-2.0-flash".to_string()
}

/// Configuration for rubric-based evaluation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RubricConfig {
    /// List of rubrics to evaluate against
    pub rubrics: Vec<Rubric>,
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
}
