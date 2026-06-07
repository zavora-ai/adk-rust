//! Per-model pricing configuration for cost estimation.
//!
//! This module provides pricing tables used by the [`CostTracker`](crate::CostTracker)
//! to compute estimated dollar costs from token usage. Pricing is specified as
//! cost per 1,000 tokens for both input and output.
//!
//! # Example
//!
//! ```rust
//! use adk_eval::pricing::{ModelPricing, default_pricing};
//!
//! // Use built-in pricing for common models
//! let pricing = default_pricing();
//! assert!(!pricing.is_empty());
//!
//! // Create custom pricing for a specific model
//! let custom = ModelPricing {
//!     model_name: "my-custom-model".to_string(),
//!     input_cost_per_1k: 0.001,
//!     output_cost_per_1k: 0.002,
//! };
//! ```

use serde::{Deserialize, Serialize};

/// Per-model pricing configuration.
///
/// Defines the cost per 1,000 input and output tokens for a specific model.
/// Used by [`CostTracker`](crate::CostTracker) to compute estimated dollar
/// costs from token counts extracted during evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPricing {
    /// Model identifier (e.g., "gemini-2.5-flash", "gpt-4o")
    pub model_name: String,
    /// Cost per 1,000 input tokens in USD
    pub input_cost_per_1k: f64,
    /// Cost per 1,000 output tokens in USD
    pub output_cost_per_1k: f64,
}

impl ModelPricing {
    /// Create a new `ModelPricing` entry.
    ///
    /// # Arguments
    ///
    /// * `model_name` - Model identifier string
    /// * `input_cost_per_1k` - Cost per 1K input tokens (USD)
    /// * `output_cost_per_1k` - Cost per 1K output tokens (USD)
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_eval::pricing::ModelPricing;
    ///
    /// let pricing = ModelPricing::new("gpt-4o", 0.0025, 0.01);
    /// assert_eq!(pricing.model_name, "gpt-4o");
    /// ```
    pub fn new(
        model_name: impl Into<String>,
        input_cost_per_1k: f64,
        output_cost_per_1k: f64,
    ) -> Self {
        Self { model_name: model_name.into(), input_cost_per_1k, output_cost_per_1k }
    }
}

/// Returns default pricing tables for common LLM models.
///
/// Includes approximate pricing for Google Gemini, OpenAI GPT, and Anthropic
/// Claude model families. Prices are approximations and may not reflect the
/// latest published rates.
///
/// # Example
///
/// ```rust
/// use adk_eval::pricing::default_pricing;
///
/// let pricing = default_pricing();
/// let gemini_flash = pricing.iter().find(|p| p.model_name == "gemini-2.5-flash");
/// assert!(gemini_flash.is_some());
/// ```
pub fn default_pricing() -> Vec<ModelPricing> {
    vec![
        // Google Gemini models
        ModelPricing::new("gemini-2.5-flash", 0.00015, 0.0006),
        ModelPricing::new("gemini-2.5-pro", 0.00125, 0.005),
        ModelPricing::new("gemini-2.0-flash", 0.0001, 0.0004),
        ModelPricing::new("gemini-2.0-flash-lite", 0.000075, 0.0003),
        // OpenAI models
        ModelPricing::new("gpt-4o", 0.0025, 0.01),
        ModelPricing::new("gpt-4o-mini", 0.00015, 0.0006),
        ModelPricing::new("gpt-4-turbo", 0.01, 0.03),
        ModelPricing::new("gpt-4", 0.03, 0.06),
        ModelPricing::new("gpt-3.5-turbo", 0.0005, 0.0015),
        ModelPricing::new("o1", 0.015, 0.06),
        ModelPricing::new("o1-mini", 0.003, 0.012),
        ModelPricing::new("o3-mini", 0.0011, 0.0044),
        // Anthropic Claude models
        ModelPricing::new("claude-sonnet-4-20250514", 0.003, 0.015),
        ModelPricing::new("claude-3.5-haiku", 0.0008, 0.004),
        ModelPricing::new("claude-3-opus", 0.015, 0.075),
        ModelPricing::new("claude-3-haiku", 0.00025, 0.00125),
        // DeepSeek models
        ModelPricing::new("deepseek-chat", 0.00014, 0.00028),
        ModelPricing::new("deepseek-reasoner", 0.00055, 0.0022),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_pricing_new() {
        let pricing = ModelPricing::new("test-model", 0.001, 0.002);
        assert_eq!(pricing.model_name, "test-model");
        assert_eq!(pricing.input_cost_per_1k, 0.001);
        assert_eq!(pricing.output_cost_per_1k, 0.002);
    }

    #[test]
    fn test_default_pricing_not_empty() {
        let pricing = default_pricing();
        assert!(!pricing.is_empty());
    }

    #[test]
    fn test_default_pricing_includes_gemini() {
        let pricing = default_pricing();
        let gemini = pricing.iter().find(|p| p.model_name == "gemini-2.5-flash");
        assert!(gemini.is_some());
        let gemini = gemini.unwrap();
        assert!(gemini.input_cost_per_1k > 0.0);
        assert!(gemini.output_cost_per_1k > 0.0);
    }

    #[test]
    fn test_default_pricing_includes_openai() {
        let pricing = default_pricing();
        let gpt4o = pricing.iter().find(|p| p.model_name == "gpt-4o");
        assert!(gpt4o.is_some());
        let gpt4o = gpt4o.unwrap();
        assert!(gpt4o.input_cost_per_1k > 0.0);
        assert!(gpt4o.output_cost_per_1k > 0.0);
    }

    #[test]
    fn test_default_pricing_includes_anthropic() {
        let pricing = default_pricing();
        let claude = pricing.iter().find(|p| p.model_name == "claude-sonnet-4-20250514");
        assert!(claude.is_some());
        let claude = claude.unwrap();
        assert!(claude.input_cost_per_1k > 0.0);
        assert!(claude.output_cost_per_1k > 0.0);
    }

    #[test]
    fn test_default_pricing_all_positive_costs() {
        let pricing = default_pricing();
        for model in &pricing {
            assert!(
                model.input_cost_per_1k >= 0.0,
                "Model {} has negative input cost",
                model.model_name
            );
            assert!(
                model.output_cost_per_1k >= 0.0,
                "Model {} has negative output cost",
                model.model_name
            );
        }
    }

    #[test]
    fn test_model_pricing_serialization_roundtrip() {
        let pricing = ModelPricing::new("test-model", 0.001, 0.002);
        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();
        assert_eq!(pricing, deserialized);
    }

    #[test]
    fn test_default_pricing_unique_model_names() {
        let pricing = default_pricing();
        let mut names: Vec<&str> = pricing.iter().map(|p| p.model_name.as_str()).collect();
        let original_len = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), original_len, "Default pricing contains duplicate model names");
    }
}
