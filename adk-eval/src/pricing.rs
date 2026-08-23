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

/// Returns default pricing tables for current LLM models.
///
/// Rates are the vendors' published standard-tier list prices, converted to USD
/// per 1,000 tokens, verified 2026-08-23 against:
///
/// - <https://ai.google.dev/gemini-api/docs/pricing>
/// - <https://developers.openai.com/api/docs/pricing>
/// - <https://docs.claude.com/en/docs/about-claude/pricing>
/// - <https://api-docs.deepseek.com/quick_start/pricing>
///
/// Only text input and output are modelled. Prompt-cache reads, batch discounts,
/// long-context tiers, audio and image rates, and DeepSeek's off-peak halving are
/// not represented; use the provider crates' pricing modules for those.
///
/// # Example
///
/// ```rust
/// use adk_eval::pricing::default_pricing;
///
/// let pricing = default_pricing();
/// let flash = pricing.iter().find(|p| p.model_name == "gemini-3.7-flash");
/// assert!(flash.is_some());
/// ```
pub fn default_pricing() -> Vec<ModelPricing> {
    vec![
        // Google Gemini. 3.7 and 3.6 Flash carry introductory rates that double
        // on 2027-01-01.
        ModelPricing::new("gemini-3.7-flash", 0.00075, 0.00375),
        ModelPricing::new("gemini-3.6-flash", 0.00075, 0.00375),
        ModelPricing::new("gemini-3.5-flash", 0.0015, 0.009),
        ModelPricing::new("gemini-3.5-flash-lite", 0.0003, 0.0025),
        ModelPricing::new("gemini-3.1-pro-preview", 0.002, 0.012),
        ModelPricing::new("gemini-3.1-flash-lite", 0.00025, 0.0015),
        ModelPricing::new("gemini-3-flash-preview", 0.0005, 0.003),
        ModelPricing::new("gemini-2.5-pro", 0.00125, 0.01),
        ModelPricing::new("gemini-2.5-flash", 0.0003, 0.0025),
        ModelPricing::new("gemini-2.5-flash-lite", 0.0001, 0.0004),
        // OpenAI
        ModelPricing::new("gpt-5.6-sol", 0.004, 0.02),
        ModelPricing::new("gpt-5.6-terra", 0.002, 0.012),
        ModelPricing::new("gpt-5.6-luna", 0.0002, 0.0012),
        ModelPricing::new("gpt-5.5", 0.005, 0.03),
        ModelPricing::new("gpt-5.4", 0.0025, 0.015),
        ModelPricing::new("gpt-5.4-mini", 0.00075, 0.0045),
        ModelPricing::new("gpt-5.4-nano", 0.0002, 0.00125),
        ModelPricing::new("gpt-5.3-codex", 0.00175, 0.014),
        ModelPricing::new("gpt-5.2", 0.00175, 0.014),
        ModelPricing::new("gpt-5.1", 0.00125, 0.01),
        ModelPricing::new("gpt-5", 0.00125, 0.01),
        ModelPricing::new("gpt-5-mini", 0.00025, 0.002),
        ModelPricing::new("gpt-5-nano", 0.00005, 0.0004),
        ModelPricing::new("gpt-4.1", 0.002, 0.008),
        ModelPricing::new("gpt-4.1-mini", 0.0004, 0.0016),
        ModelPricing::new("gpt-4o", 0.0025, 0.01),
        ModelPricing::new("gpt-4o-mini", 0.00015, 0.0006),
        ModelPricing::new("o3", 0.002, 0.008),
        ModelPricing::new("o4-mini", 0.0011, 0.0044),
        // Anthropic Claude
        ModelPricing::new("claude-fable-5", 0.01, 0.05),
        ModelPricing::new("claude-mythos-5", 0.01, 0.05),
        ModelPricing::new("claude-opus-5", 0.005, 0.025),
        ModelPricing::new("claude-opus-4-8", 0.005, 0.025),
        ModelPricing::new("claude-sonnet-5", 0.002, 0.01),
        ModelPricing::new("claude-sonnet-4-6", 0.003, 0.015),
        ModelPricing::new("claude-haiku-4-5", 0.001, 0.005),
        // DeepSeek. Peak (cache-miss) rates; off-peak is half.
        ModelPricing::new("deepseek-v4-flash", 0.00044, 0.00132),
        ModelPricing::new("deepseek-v4-pro", 0.00132, 0.00396),
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
        let gemini = pricing.iter().find(|p| p.model_name == "gemini-3.7-flash");
        assert!(gemini.is_some());
        let gemini = gemini.unwrap();
        assert!(gemini.input_cost_per_1k > 0.0);
        assert!(gemini.output_cost_per_1k > 0.0);
    }

    #[test]
    fn test_default_pricing_includes_openai() {
        let pricing = default_pricing();
        let gpt = pricing.iter().find(|p| p.model_name == "gpt-5.6-terra");
        assert!(gpt.is_some());
        let gpt = gpt.unwrap();
        assert!(gpt.input_cost_per_1k > 0.0);
        assert!(gpt.output_cost_per_1k > 0.0);
    }

    #[test]
    fn test_default_pricing_includes_anthropic() {
        let pricing = default_pricing();
        let claude = pricing.iter().find(|p| p.model_name == "claude-sonnet-5");
        assert!(claude.is_some());
        let claude = claude.unwrap();
        assert!(claude.input_cost_per_1k > 0.0);
        assert!(claude.output_cost_per_1k > 0.0);
    }

    /// The table must not carry models the vendors have shut down, and must not
    /// duplicate a model ID.
    #[test]
    fn test_default_pricing_excludes_retired_models() {
        let pricing = default_pricing();
        for retired in [
            "gemini-2.0-flash",
            "gemini-2.0-flash-lite",
            "gemini-3-pro-preview",
            "claude-3-opus",
            "claude-3-haiku",
            "claude-3.5-haiku",
            "claude-sonnet-4-20250514",
            "deepseek-chat",
            "deepseek-reasoner",
            "gpt-4",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
        ] {
            assert!(
                !pricing.iter().any(|p| p.model_name == retired),
                "{retired} is retired and must not be in the default table"
            );
        }

        let mut names: Vec<&str> = pricing.iter().map(|p| p.model_name.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate model IDs in default pricing");
    }

    /// Output must never be cheaper than input for these vendors' text models.
    /// A swapped pair is the most common transcription error.
    #[test]
    fn test_output_never_cheaper_than_input() {
        for model in default_pricing() {
            assert!(
                model.output_cost_per_1k >= model.input_cost_per_1k,
                "{} output {} is below input {}",
                model.model_name,
                model.output_cost_per_1k,
                model.input_cost_per_1k
            );
        }
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
