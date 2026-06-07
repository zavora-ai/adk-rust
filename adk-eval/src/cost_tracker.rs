//! Cost and latency tracking for evaluation runs.
//!
//! This module provides [`CostTracker`] which extracts token usage from agent
//! event streams and computes estimated dollar costs using configurable
//! per-model pricing tables.
//!
//! # Example
//!
//! ```rust
//! use adk_eval::cost_tracker::{CostTracker, CostMetrics};
//!
//! let tracker = CostTracker::new();
//!
//! // Compute cost for a known model
//! let cost = tracker.compute_cost("gpt-4o", 1000, 500);
//! assert!(cost.is_some());
//!
//! // Unknown models return None
//! let cost = tracker.compute_cost("unknown-model", 100, 50);
//! assert!(cost.is_none());
//! ```

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use adk_core::Event;

use crate::pricing::{ModelPricing, default_pricing};

/// Cost and latency metrics for a single evaluation turn.
///
/// Captures token usage, estimated cost, and wall-clock latency for
/// a set of events produced during agent execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostMetrics {
    /// Number of prompt/input tokens used.
    pub prompt_tokens: u64,
    /// Number of completion/output tokens generated.
    pub completion_tokens: u64,
    /// Total token count (prompt + completion).
    pub total_tokens: u64,
    /// Estimated cost in USD (None if pricing unavailable for the model).
    pub cost_usd: Option<f64>,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u64,
}

/// Tracks cost and latency metrics from agent event streams.
///
/// Uses per-model pricing tables to compute estimated USD costs from
/// token counts extracted from [`Event`] streams.
///
/// # Example
///
/// ```rust
/// use adk_eval::cost_tracker::CostTracker;
/// use adk_eval::pricing::ModelPricing;
///
/// // Use default pricing
/// let tracker = CostTracker::new();
///
/// // Or provide custom pricing
/// let custom_pricing = vec![
///     ModelPricing::new("my-model", 0.001, 0.002),
/// ];
/// let tracker = CostTracker::with_pricing(custom_pricing);
/// ```
pub struct CostTracker {
    pricing: HashMap<String, ModelPricing>,
}

impl CostTracker {
    /// Creates a new `CostTracker` with default pricing for common models.
    ///
    /// Default pricing includes Google Gemini, OpenAI GPT, and Anthropic
    /// Claude model families.
    pub fn new() -> Self {
        Self::with_pricing(default_pricing())
    }

    /// Creates a new `CostTracker` with the specified pricing table.
    ///
    /// # Arguments
    ///
    /// * `pricing` - A list of [`ModelPricing`] entries to use for cost computation.
    pub fn with_pricing(pricing: Vec<ModelPricing>) -> Self {
        let pricing_map = pricing.into_iter().map(|p| (p.model_name.clone(), p)).collect();
        Self { pricing: pricing_map }
    }

    /// Extract cost metrics from an event stream.
    ///
    /// Iterates over events looking for [`UsageMetadata`](adk_core::UsageMetadata)
    /// on LLM responses. Token counts are summed across all events that contain
    /// usage metadata. If no usage metadata is found, token counts default to zero.
    ///
    /// The `duration` parameter is converted to milliseconds for the `latency_ms` field.
    ///
    /// Note: The `cost_usd` field will be `None` because the model name is not
    /// available on the Event struct. Use [`compute_cost`](Self::compute_cost)
    /// separately when the model name is known.
    ///
    /// # Arguments
    ///
    /// * `events` - Slice of events from an agent execution.
    /// * `duration` - Wall-clock duration of the execution.
    ///
    /// # Returns
    ///
    /// A [`CostMetrics`] struct with aggregated token counts and latency.
    pub fn extract_metrics(&self, events: &[Event], duration: Duration) -> CostMetrics {
        let mut prompt_tokens: u64 = 0;
        let mut completion_tokens: u64 = 0;
        let mut total_tokens: u64 = 0;

        for event in events {
            if let Some(usage) = &event.llm_response.usage_metadata {
                // Accumulate token counts, treating negative values as zero
                prompt_tokens += u64::try_from(usage.prompt_token_count.max(0)).unwrap_or(0);
                completion_tokens +=
                    u64::try_from(usage.candidates_token_count.max(0)).unwrap_or(0);
                total_tokens += u64::try_from(usage.total_token_count.max(0)).unwrap_or(0);
            }
        }

        CostMetrics {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd: None,
            latency_ms: duration.as_millis() as u64,
        }
    }

    /// Compute cost from token counts and model name.
    ///
    /// Uses the formula:
    /// ```text
    /// (prompt_tokens / 1000.0) * input_cost_per_1k + (completion_tokens / 1000.0) * output_cost_per_1k
    /// ```
    ///
    /// Returns `None` if the model is not found in the pricing table.
    ///
    /// # Arguments
    ///
    /// * `model` - Model identifier to look up pricing for.
    /// * `prompt_tokens` - Number of input tokens.
    /// * `completion_tokens` - Number of output tokens.
    pub fn compute_cost(
        &self,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) -> Option<f64> {
        self.pricing.get(model).map(|p| {
            (prompt_tokens as f64 / 1000.0) * p.input_cost_per_1k
                + (completion_tokens as f64 / 1000.0) * p.output_cost_per_1k
        })
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cost_tracker_new_has_default_pricing() {
        let tracker = CostTracker::new();
        assert!(!tracker.pricing.is_empty());
        assert!(tracker.pricing.contains_key("gpt-4o"));
        assert!(tracker.pricing.contains_key("gemini-2.5-flash"));
    }

    #[test]
    fn test_cost_tracker_with_custom_pricing() {
        let pricing = vec![ModelPricing::new("custom-model", 0.01, 0.02)];
        let tracker = CostTracker::with_pricing(pricing);
        assert_eq!(tracker.pricing.len(), 1);
        assert!(tracker.pricing.contains_key("custom-model"));
    }

    #[test]
    fn test_compute_cost_known_model() {
        let pricing = vec![ModelPricing::new("test-model", 0.001, 0.002)];
        let tracker = CostTracker::with_pricing(pricing);

        let cost = tracker.compute_cost("test-model", 1000, 500);
        assert!(cost.is_some());
        // (1000/1000) * 0.001 + (500/1000) * 0.002 = 0.001 + 0.001 = 0.002
        let expected = (1000.0 / 1000.0) * 0.001 + (500.0 / 1000.0) * 0.002;
        assert!((cost.unwrap() - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_cost_unknown_model() {
        let tracker = CostTracker::with_pricing(vec![]);
        let cost = tracker.compute_cost("unknown", 100, 50);
        assert!(cost.is_none());
    }

    #[test]
    fn test_compute_cost_zero_tokens() {
        let pricing = vec![ModelPricing::new("test-model", 0.001, 0.002)];
        let tracker = CostTracker::with_pricing(pricing);

        let cost = tracker.compute_cost("test-model", 0, 0);
        assert_eq!(cost, Some(0.0));
    }

    #[test]
    fn test_extract_metrics_empty_events() {
        let tracker = CostTracker::new();
        let metrics = tracker.extract_metrics(&[], Duration::from_millis(500));

        assert_eq!(metrics.prompt_tokens, 0);
        assert_eq!(metrics.completion_tokens, 0);
        assert_eq!(metrics.total_tokens, 0);
        assert_eq!(metrics.cost_usd, None);
        assert_eq!(metrics.latency_ms, 500);
    }

    #[test]
    fn test_extract_metrics_with_usage() {
        let tracker =
            CostTracker::with_pricing(vec![ModelPricing::new("test-model", 0.001, 0.002)]);

        let mut event = Event::new("inv-1");
        event.llm_response.usage_metadata = Some(adk_core::UsageMetadata {
            prompt_token_count: 100,
            candidates_token_count: 50,
            total_token_count: 150,
            ..Default::default()
        });

        let metrics = tracker.extract_metrics(&[event], Duration::from_secs(2));

        assert_eq!(metrics.prompt_tokens, 100);
        assert_eq!(metrics.completion_tokens, 50);
        assert_eq!(metrics.total_tokens, 150);
        // cost_usd is None because model can't be determined from events
        assert_eq!(metrics.cost_usd, None);
        assert_eq!(metrics.latency_ms, 2000);
    }

    #[test]
    fn test_extract_metrics_no_usage_metadata() {
        let tracker = CostTracker::new();
        let event = Event::new("inv-1");

        let metrics = tracker.extract_metrics(&[event], Duration::from_millis(100));

        assert_eq!(metrics.prompt_tokens, 0);
        assert_eq!(metrics.completion_tokens, 0);
        assert_eq!(metrics.total_tokens, 0);
        assert_eq!(metrics.cost_usd, None);
        assert_eq!(metrics.latency_ms, 100);
    }

    #[test]
    fn test_extract_metrics_multiple_events_accumulate() {
        let tracker =
            CostTracker::with_pricing(vec![ModelPricing::new("test-model", 0.001, 0.002)]);

        let mut event1 = Event::new("inv-1");
        event1.llm_response.usage_metadata = Some(adk_core::UsageMetadata {
            prompt_token_count: 50,
            candidates_token_count: 25,
            total_token_count: 75,
            ..Default::default()
        });

        let mut event2 = Event::new("inv-1");
        event2.llm_response.usage_metadata = Some(adk_core::UsageMetadata {
            prompt_token_count: 60,
            candidates_token_count: 30,
            total_token_count: 90,
            ..Default::default()
        });

        let metrics = tracker.extract_metrics(&[event1, event2], Duration::from_millis(300));

        assert_eq!(metrics.prompt_tokens, 110);
        assert_eq!(metrics.completion_tokens, 55);
        assert_eq!(metrics.total_tokens, 165);
        // cost_usd is None because model can't be determined from events
        assert_eq!(metrics.cost_usd, None);
        assert_eq!(metrics.latency_ms, 300);
    }

    #[test]
    fn test_default_impl() {
        let tracker = CostTracker::default();
        assert!(!tracker.pricing.is_empty());
    }
}
