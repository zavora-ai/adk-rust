//! Multi-turn conversation metrics.
//!
//! The [`ConversationScorer`] evaluates multi-turn conversations using a
//! [`StructuredJudge`] for semantic metrics (context retention, goal completion,
//! coherence) and optionally an `EmbeddingScorer` (with the `embedding` feature)
//! for topic drift measurement.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::{ConversationScorer, ConversationScorerConfig};
//! use adk_eval::structured_judge::StructuredJudge;
//! use adk_core::Content;
//! use std::sync::Arc;
//!
//! let judge = StructuredJudge::new(model);
//! let scorer = ConversationScorer::new(judge);
//!
//! let conversation = vec![
//!     Content::new("user").with_text("Hello, help me plan a trip to Paris"),
//!     Content::new("model").with_text("I'd be happy to help with your Paris trip!"),
//!     Content::new("user").with_text("What about hotels near the Eiffel Tower?"),
//!     Content::new("model").with_text("Here are some great hotels near the Eiffel Tower..."),
//! ];
//!
//! let metrics = scorer.score(&conversation, "Plan a trip to Paris").await?;
//! assert!((0.0..=1.0).contains(&metrics.context_retention));
//! ```

use crate::error::{EvalError, Result};
use crate::structured_judge::StructuredJudge;
use adk_core::Content;
use serde::{Deserialize, Serialize};

#[cfg(feature = "embedding")]
use std::sync::Arc;

#[cfg(feature = "embedding")]
use crate::embedding_scorer::EmbeddingScorer;

/// Multi-turn conversation quality metrics.
///
/// All scores are in the range \[0.0, 1.0\].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMetrics {
    /// Score measuring whether the agent correctly references information from
    /// prior turns (0.0–1.0).
    pub context_retention: f64,
    /// Score measuring whether the agent achieves the stated objective across
    /// the conversation (0.0–1.0).
    pub goal_completion: f64,
    /// Score measuring logical consistency between consecutive agent responses
    /// (0.0–1.0).
    pub coherence: f64,
    /// Score measuring deviation from original topic (0.0–1.0, where 1.0
    /// indicates no drift).
    pub topic_drift: f64,
}

/// Configuration for conversation scoring thresholds.
///
/// Each threshold defines the minimum acceptable score for a metric.
/// Scores below the threshold indicate a failure for that metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationScorerConfig {
    /// Minimum acceptable context retention score.
    pub context_retention_threshold: f64,
    /// Minimum acceptable goal completion score.
    pub goal_completion_threshold: f64,
    /// Minimum acceptable coherence score.
    pub coherence_threshold: f64,
    /// Minimum acceptable topic drift score (1.0 = no drift).
    pub topic_drift_threshold: f64,
}

impl Default for ConversationScorerConfig {
    fn default() -> Self {
        Self {
            context_retention_threshold: 0.7,
            goal_completion_threshold: 0.7,
            coherence_threshold: 0.7,
            topic_drift_threshold: 0.7,
        }
    }
}

/// Scores multi-turn conversations on quality metrics.
///
/// Uses a [`StructuredJudge`] for context retention, goal completion, and
/// coherence metrics. For topic drift, uses an `EmbeddingScorer` if available,
/// otherwise falls back to the structured judge.
pub struct ConversationScorer {
    judge: StructuredJudge,
    #[cfg(feature = "embedding")]
    embedding_scorer: Option<Arc<EmbeddingScorer>>,
    config: ConversationScorerConfig,
}

impl ConversationScorer {
    /// Create a new conversation scorer with default configuration.
    ///
    /// Uses the structured judge for all metrics including topic drift.
    pub fn new(judge: StructuredJudge) -> Self {
        Self {
            judge,
            #[cfg(feature = "embedding")]
            embedding_scorer: None,
            config: ConversationScorerConfig::default(),
        }
    }

    /// Create a conversation scorer with an embedding scorer for topic drift.
    ///
    /// Topic drift will be measured using cosine similarity between the first
    /// and last turn embeddings instead of the structured judge.
    #[cfg(feature = "embedding")]
    pub fn with_embedding(judge: StructuredJudge, embedding: Arc<EmbeddingScorer>) -> Self {
        Self {
            judge,
            embedding_scorer: Some(embedding),
            config: ConversationScorerConfig::default(),
        }
    }

    /// Create a conversation scorer with full configuration.
    #[cfg(feature = "embedding")]
    pub fn with_config(
        judge: StructuredJudge,
        embedding: Option<Arc<EmbeddingScorer>>,
        config: ConversationScorerConfig,
    ) -> Self {
        Self { judge, embedding_scorer: embedding, config }
    }

    /// Create a conversation scorer with custom configuration (no embedding).
    #[cfg(not(feature = "embedding"))]
    pub fn with_config(judge: StructuredJudge, config: ConversationScorerConfig) -> Self {
        Self { judge, config }
    }

    /// Returns the current configuration.
    pub fn config(&self) -> &ConversationScorerConfig {
        &self.config
    }

    /// Score a multi-turn conversation.
    ///
    /// Evaluates the conversation on four metrics:
    /// - **Context retention**: Does the agent reference prior turn information?
    /// - **Goal completion**: Does the agent achieve the stated objective?
    /// - **Coherence**: Are consecutive responses logically consistent?
    /// - **Topic drift**: Does the conversation stay on topic?
    ///
    /// All scores are clamped to \[0.0, 1.0\].
    ///
    /// # Errors
    ///
    /// Returns an error if the judge LLM calls fail or the conversation is empty.
    pub async fn score(&self, conversation: &[Content], goal: &str) -> Result<ConversationMetrics> {
        if conversation.is_empty() {
            return Err(EvalError::ScoringError("cannot score an empty conversation".to_string()));
        }

        let context_retention = self.score_context_retention(conversation).await?;
        let goal_completion = self.score_goal_completion(conversation, goal).await?;
        let coherence = self.score_coherence(conversation).await?;
        let topic_drift = self.score_topic_drift(conversation).await?;

        Ok(ConversationMetrics {
            context_retention: context_retention.clamp(0.0, 1.0),
            goal_completion: goal_completion.clamp(0.0, 1.0),
            coherence: coherence.clamp(0.0, 1.0),
            topic_drift: topic_drift.clamp(0.0, 1.0),
        })
    }

    /// Score context retention using the structured judge.
    ///
    /// Evaluates whether the agent correctly references information from
    /// prior turns in its responses.
    async fn score_context_retention(&self, conversation: &[Content]) -> Result<f64> {
        let conversation_text = format_conversation(conversation);

        let criterion = "Context Retention: Evaluate whether the agent correctly \
            references and uses information from earlier turns in the conversation. \
            A score of 1.0 means the agent perfectly retains and uses all prior context. \
            A score of 0.0 means the agent completely ignores previous context.";

        let verdict = self.judge.judge(&conversation_text, &conversation_text, criterion).await?;

        Ok(verdict.score)
    }

    /// Score goal completion using the structured judge.
    ///
    /// Evaluates whether the agent achieves the stated objective across the
    /// conversation.
    async fn score_goal_completion(&self, conversation: &[Content], goal: &str) -> Result<f64> {
        let conversation_text = format_conversation(conversation);

        let criterion = format!(
            "Goal Completion: Evaluate whether the agent successfully achieves \
            the following goal across the conversation: \"{goal}\". \
            A score of 1.0 means the goal is fully achieved. \
            A score of 0.0 means no progress toward the goal was made."
        );

        let verdict = self.judge.judge(goal, &conversation_text, &criterion).await?;

        Ok(verdict.score)
    }

    /// Score coherence using the structured judge.
    ///
    /// Evaluates logical consistency between consecutive agent responses.
    async fn score_coherence(&self, conversation: &[Content]) -> Result<f64> {
        let conversation_text = format_conversation(conversation);

        let criterion = "Coherence: Evaluate the logical consistency between consecutive \
            responses in this conversation. A score of 1.0 means all responses are \
            perfectly logically consistent with each other. A score of 0.0 means \
            responses contradict each other or are completely incoherent.";

        let verdict = self.judge.judge(&conversation_text, &conversation_text, criterion).await?;

        Ok(verdict.score)
    }

    /// Score topic drift.
    ///
    /// If an embedding scorer is available, uses cosine similarity between the
    /// first and last turn text. Otherwise falls back to the structured judge.
    async fn score_topic_drift(&self, conversation: &[Content]) -> Result<f64> {
        #[cfg(feature = "embedding")]
        if let Some(embedding) = &self.embedding_scorer {
            return self.score_topic_drift_embedding(conversation, embedding).await;
        }

        // Fallback: use structured judge
        self.score_topic_drift_judge(conversation).await
    }

    /// Score topic drift using embedding similarity between first and last turns.
    #[cfg(feature = "embedding")]
    async fn score_topic_drift_embedding(
        &self,
        conversation: &[Content],
        embedding: &EmbeddingScorer,
    ) -> Result<f64> {
        let first_text = extract_text_from_content(&conversation[0]);
        let last_text = extract_text_from_content(conversation.last().unwrap());

        if first_text.is_empty() || last_text.is_empty() {
            // Fall back to judge if we can't extract text
            return self.score_topic_drift_judge(conversation).await;
        }

        // Similarity of 1.0 means no topic drift
        embedding.score(&first_text, &last_text).await
    }

    /// Score topic drift using the structured judge as fallback.
    async fn score_topic_drift_judge(&self, conversation: &[Content]) -> Result<f64> {
        let conversation_text = format_conversation(conversation);

        let criterion = "Topic Drift: Evaluate how well the conversation stays on its \
            original topic. A score of 1.0 means the conversation remains perfectly \
            on-topic throughout. A score of 0.0 means the conversation has completely \
            diverged from its original topic.";

        let verdict = self.judge.judge(&conversation_text, &conversation_text, criterion).await?;

        Ok(verdict.score)
    }
}

/// Extract all text parts from a Content item and concatenate them.
fn extract_text_from_content(content: &Content) -> String {
    content.parts.iter().filter_map(|part| part.text()).collect::<Vec<_>>().join(" ")
}

/// Format a conversation into a readable string for the judge.
fn format_conversation(conversation: &[Content]) -> String {
    let mut output = String::new();
    for (i, content) in conversation.iter().enumerate() {
        let text = extract_text_from_content(content);
        if !text.is_empty() {
            output.push_str(&format!("Turn {} [{}]: {}\n", i + 1, content.role, text));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_conversation_scorer_config_default() {
        let config = ConversationScorerConfig::default();
        assert_eq!(config.context_retention_threshold, 0.7);
        assert_eq!(config.goal_completion_threshold, 0.7);
        assert_eq!(config.coherence_threshold, 0.7);
        assert_eq!(config.topic_drift_threshold, 0.7);
    }

    #[test]
    fn test_conversation_scorer_config_serialization() {
        let config = ConversationScorerConfig {
            context_retention_threshold: 0.8,
            goal_completion_threshold: 0.6,
            coherence_threshold: 0.75,
            topic_drift_threshold: 0.9,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ConversationScorerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.context_retention_threshold, 0.8);
        assert_eq!(deserialized.goal_completion_threshold, 0.6);
        assert_eq!(deserialized.coherence_threshold, 0.75);
        assert_eq!(deserialized.topic_drift_threshold, 0.9);
    }

    #[test]
    fn test_conversation_metrics_serialization() {
        let metrics = ConversationMetrics {
            context_retention: 0.85,
            goal_completion: 0.9,
            coherence: 0.75,
            topic_drift: 0.8,
        };
        let json = serde_json::to_string(&metrics).unwrap();
        let deserialized: ConversationMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.context_retention, 0.85);
        assert_eq!(deserialized.goal_completion, 0.9);
        assert_eq!(deserialized.coherence, 0.75);
        assert_eq!(deserialized.topic_drift, 0.8);
    }

    #[test]
    fn test_extract_text_from_content() {
        let content = Content::new("user").with_text("Hello world");
        let text = extract_text_from_content(&content);
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_extract_text_from_content_multiple_parts() {
        let content = Content::new("model").with_text("Part one").with_text("Part two");
        let text = extract_text_from_content(&content);
        assert_eq!(text, "Part one Part two");
    }

    #[test]
    fn test_extract_text_from_empty_content() {
        let content = Content::new("user");
        let text = extract_text_from_content(&content);
        assert_eq!(text, "");
    }

    #[test]
    fn test_format_conversation() {
        let conversation = vec![
            Content::new("user").with_text("Hi there"),
            Content::new("model").with_text("Hello! How can I help?"),
            Content::new("user").with_text("Tell me about Rust"),
        ];
        let formatted = format_conversation(&conversation);
        assert!(formatted.contains("Turn 1 [user]: Hi there"));
        assert!(formatted.contains("Turn 2 [model]: Hello! How can I help?"));
        assert!(formatted.contains("Turn 3 [user]: Tell me about Rust"));
    }

    #[test]
    fn test_format_conversation_skips_empty_text() {
        let conversation = vec![
            Content::new("user").with_text("Hello"),
            Content::new("model"), // empty content
            Content::new("user").with_text("World"),
        ];
        let formatted = format_conversation(&conversation);
        assert!(formatted.contains("Turn 1 [user]: Hello"));
        assert!(!formatted.contains("Turn 2"));
        assert!(formatted.contains("Turn 3 [user]: World"));
    }

    #[test]
    fn test_conversation_scorer_new() {
        let model = Arc::new(adk_model::MockLlm::new("test-model"));
        let judge = StructuredJudge::new(model);
        let scorer = ConversationScorer::new(judge);
        assert_eq!(scorer.config().context_retention_threshold, 0.7);
    }

    #[tokio::test]
    async fn test_conversation_scorer_empty_conversation_error() {
        let model = Arc::new(adk_model::MockLlm::new("test-model"));
        let judge = StructuredJudge::new(model);
        let scorer = ConversationScorer::new(judge);

        let result = scorer.score(&[], "some goal").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("empty conversation"));
    }
}
