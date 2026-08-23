//! Token pricing for Anthropic models.
//!
//! Provides per-model cost calculation from [`Usage`] data returned by the API.
//! Rates are the published standard-tier list prices, last verified against
//! <https://docs.claude.com/en/docs/about-claude/pricing> on 2026-08-23.
//!
//! # Limitations
//!
//! - **Fast mode** is priced separately. Claude Opus 5 and Opus 4.8 in fast mode
//!   cost 2× standard; use [`ModelPricing::OPUS_5_FAST`].
//! - **Data residency** adds a 1.1× multiplier on every token category for
//!   Claude 4.6 and later when `inference_geo` is `"us"`. Not applied here.
//! - **Batch processing** halves input and output. Not applied here.
//! - Claude 4.7 and later use a newer tokenizer that produces roughly 30% more
//!   tokens for the same text. Per-token maths is unaffected, but character-based
//!   estimates and cross-generation comparisons are not.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_anthropic::pricing::{ModelPricing, estimate_cost};
//!
//! let cost = estimate_cost(ModelPricing::SONNET_5, &response.usage);
//! println!("Cost: ${:.6}", cost.total());
//! ```

use crate::types::{Model, Usage};

/// Per-million-token prices for a single model tier.
///
/// All values are in USD per 1 million tokens.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Base input token price ($/MTok).
    pub input: f64,
    /// 5-minute cache write price ($/MTok). 1.25× base input.
    pub cache_write_5m: f64,
    /// 1-hour cache write price ($/MTok). 2× base input.
    pub cache_write_1h: f64,
    /// Cache read / refresh price ($/MTok). 0.1× base input.
    pub cache_read: f64,
    /// Output token price ($/MTok).
    pub output: f64,
}

impl ModelPricing {
    /// Claude Fable 5.
    pub const FABLE_5: Self = Self {
        input: 10.0,
        cache_write_5m: 12.5,
        cache_write_1h: 20.0,
        cache_read: 1.0,
        output: 50.0,
    };
    /// Claude Mythos 5 — limited availability, same rates as Fable 5.
    pub const MYTHOS_5: Self = Self {
        input: 10.0,
        cache_write_5m: 12.5,
        cache_write_1h: 20.0,
        cache_read: 1.0,
        output: 50.0,
    };
    /// Claude Opus 5.
    pub const OPUS_5: Self = Self {
        input: 5.0,
        cache_write_5m: 6.25,
        cache_write_1h: 10.0,
        cache_read: 0.50,
        output: 25.0,
    };
    /// Claude Sonnet 5.
    ///
    /// The $2/$10 rate launched as introductory pricing through August 31, 2026
    /// and is now the standard price; the scheduled rise to $3/$15 was cancelled.
    pub const SONNET_5: Self = Self {
        input: 2.0,
        cache_write_5m: 2.5,
        cache_write_1h: 4.0,
        cache_read: 0.20,
        output: 10.0,
    };

    /// Fast mode rates for Claude Opus 5 and Claude Opus 4.8.
    ///
    /// Fast mode is a research preview on the first-party Claude API only, and
    /// applies across the full context window. It is not available on Opus 4.7
    /// (which errors) or Opus 4.6 (which runs at standard speed and rates).
    pub const OPUS_5_FAST: Self = Self {
        input: 10.0,
        cache_write_5m: 12.5,
        cache_write_1h: 20.0,
        cache_read: 1.0,
        output: 50.0,
    };

    /// Claude Opus 4.8 — same pricing as Opus 4.7.
    pub const OPUS_48: Self = Self {
        input: 5.0,
        cache_write_5m: 6.25,
        cache_write_1h: 10.0,
        cache_read: 0.50,
        output: 25.0,
    };
    /// Claude Opus 4.7 — same pricing as Opus 4.6.
    pub const OPUS_47: Self = Self {
        input: 5.0,
        cache_write_5m: 6.25,
        cache_write_1h: 10.0,
        cache_read: 0.50,
        output: 25.0,
    };
    /// Claude Opus 4.6
    pub const OPUS_46: Self = Self {
        input: 5.0,
        cache_write_5m: 6.25,
        cache_write_1h: 10.0,
        cache_read: 0.50,
        output: 25.0,
    };
    /// Claude Opus 4.5
    pub const OPUS_45: Self = Self {
        input: 5.0,
        cache_write_5m: 6.25,
        cache_write_1h: 10.0,
        cache_read: 0.50,
        output: 25.0,
    };
    /// Claude Opus 4.1
    pub const OPUS_41: Self = Self {
        input: 15.0,
        cache_write_5m: 18.75,
        cache_write_1h: 30.0,
        cache_read: 1.50,
        output: 75.0,
    };
    /// Claude Opus 4
    pub const OPUS_4: Self = Self {
        input: 15.0,
        cache_write_5m: 18.75,
        cache_write_1h: 30.0,
        cache_read: 1.50,
        output: 75.0,
    };
    /// Claude Sonnet 4.6
    pub const SONNET_46: Self = Self {
        input: 3.0,
        cache_write_5m: 3.75,
        cache_write_1h: 6.0,
        cache_read: 0.30,
        output: 15.0,
    };
    /// Claude Sonnet 4.5
    pub const SONNET_45: Self = Self {
        input: 3.0,
        cache_write_5m: 3.75,
        cache_write_1h: 6.0,
        cache_read: 0.30,
        output: 15.0,
    };
    /// Claude Sonnet 4
    pub const SONNET_4: Self = Self {
        input: 3.0,
        cache_write_5m: 3.75,
        cache_write_1h: 6.0,
        cache_read: 0.30,
        output: 15.0,
    };
    /// Claude Haiku 4.5
    pub const HAIKU_45: Self = Self {
        input: 1.0,
        cache_write_5m: 1.25,
        cache_write_1h: 2.0,
        cache_read: 0.10,
        output: 5.0,
    };

    /// Claude Haiku 3.5 — retired except on Bedrock and Google Cloud.
    pub const HAIKU_35: Self = Self {
        input: 0.80,
        cache_write_5m: 1.0,
        cache_write_1h: 1.60,
        cache_read: 0.08,
        output: 4.0,
    };

    /// Returns the standard pricing for a [`Model`], if Anthropic publishes one.
    ///
    /// Resolves both [`Model::Known`] variants and the [`Model::Custom`]
    /// identifiers returned by the Claude 5 factories. Returns `None` for
    /// unrecognised identifiers — treat that as unpriced, never as free.
    ///
    /// Rates are the standard tier. Apply the fast-mode constants
    /// ([`Self::OPUS_5_FAST`]) and the data-residency multiplier separately.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::{Model, pricing::ModelPricing};
    ///
    /// let pricing = ModelPricing::for_model(&Model::claude_sonnet_5()).unwrap();
    /// assert_eq!(pricing.input, 2.0);
    /// ```
    pub fn for_model(model: &Model) -> Option<Self> {
        Self::for_model_id(&model.to_string())
    }

    /// Returns the standard pricing for a raw Anthropic model ID.
    ///
    /// Accepts dated aliases (for example `claude-sonnet-4-5-20250929`) by
    /// matching on the undated family prefix.
    ///
    /// Returns `None` when Anthropic publishes no rate for the identifier.
    pub fn for_model_id(model_id: &str) -> Option<Self> {
        let pricing = match model_id {
            id if id.starts_with("claude-fable-5") => Self::FABLE_5,
            id if id.starts_with("claude-mythos-5") => Self::MYTHOS_5,
            id if id.starts_with("claude-opus-5") => Self::OPUS_5,
            id if id.starts_with("claude-sonnet-5") => Self::SONNET_5,
            id if id.starts_with("claude-opus-4-8") => Self::OPUS_48,
            id if id.starts_with("claude-opus-4-7") => Self::OPUS_47,
            id if id.starts_with("claude-opus-4-6") => Self::OPUS_46,
            id if id.starts_with("claude-opus-4-5") => Self::OPUS_45,
            id if id.starts_with("claude-opus-4-1") => Self::OPUS_41,
            id if id.starts_with("claude-opus-4") => Self::OPUS_4,
            id if id.starts_with("claude-sonnet-4-6") => Self::SONNET_46,
            id if id.starts_with("claude-sonnet-4-5") => Self::SONNET_45,
            id if id.starts_with("claude-sonnet-4") => Self::SONNET_4,
            id if id.starts_with("claude-haiku-4-5") => Self::HAIKU_45,
            id if id.starts_with("claude-haiku-3-5") || id.starts_with("claude-3-5-haiku") => {
                Self::HAIKU_35
            }
            _ => return None,
        };
        Some(pricing)
    }
}

/// Itemised cost breakdown from a single API response.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostBreakdown {
    /// Cost of uncached input tokens.
    pub input_cost: f64,
    /// Cost of tokens written to the 5-minute cache.
    pub cache_write_cost: f64,
    /// Cost of tokens read from cache.
    pub cache_read_cost: f64,
    /// Cost of output tokens.
    pub output_cost: f64,
}

impl CostBreakdown {
    /// Total cost in USD.
    pub fn total(&self) -> f64 {
        self.input_cost + self.cache_write_cost + self.cache_read_cost + self.output_cost
    }
}

impl std::fmt::Display for CostBreakdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "${:.6} (in=${:.6} cache_w=${:.6} cache_r=${:.6} out=${:.6})",
            self.total(),
            self.input_cost,
            self.cache_write_cost,
            self.cache_read_cost,
            self.output_cost
        )
    }
}

/// Estimate the cost of a single API call from its [`Usage`] and [`ModelPricing`].
///
/// Uses `cache_creation_input_tokens` as 5-minute cache writes. For 1-hour
/// cache writes, use [`estimate_cost_1h`] instead.
pub fn estimate_cost(pricing: ModelPricing, usage: &Usage) -> CostBreakdown {
    let mtok = 1_000_000.0;
    CostBreakdown {
        input_cost: usage.input_tokens as f64 / mtok * pricing.input,
        cache_write_cost: usage.cache_creation_input_tokens.unwrap_or(0) as f64 / mtok
            * pricing.cache_write_5m,
        cache_read_cost: usage.cache_read_input_tokens.unwrap_or(0) as f64 / mtok
            * pricing.cache_read,
        output_cost: usage.output_tokens as f64 / mtok * pricing.output,
    }
}

/// Same as [`estimate_cost`] but treats cache writes as 1-hour tier.
pub fn estimate_cost_1h(pricing: ModelPricing, usage: &Usage) -> CostBreakdown {
    let mtok = 1_000_000.0;
    CostBreakdown {
        input_cost: usage.input_tokens as f64 / mtok * pricing.input,
        cache_write_cost: usage.cache_creation_input_tokens.unwrap_or(0) as f64 / mtok
            * pricing.cache_write_1h,
        cache_read_cost: usage.cache_read_input_tokens.unwrap_or(0) as f64 / mtok
            * pricing.cache_read,
        output_cost: usage.output_tokens as f64 / mtok * pricing.output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet_46_basic_cost() {
        let usage = Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens_1h: None,
            server_tool_use: None,
        };
        let cost = estimate_cost(ModelPricing::SONNET_46, &usage);
        // 1000 input @ $3/MTok = $0.003, 500 output @ $15/MTok = $0.0075
        assert!((cost.input_cost - 0.003).abs() < 1e-9);
        assert!((cost.output_cost - 0.0075).abs() < 1e-9);
        assert!((cost.total() - 0.0105).abs() < 1e-9);
    }

    #[test]
    fn sonnet_5_basic_cost() {
        let usage = Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens_1h: None,
            server_tool_use: None,
        };
        let cost = estimate_cost(ModelPricing::SONNET_5, &usage);
        assert!((cost.input_cost - 0.002).abs() < 1e-9);
        assert!((cost.output_cost - 0.005).abs() < 1e-9);
    }

    #[test]
    fn sonnet_46_with_caching() {
        let usage = Usage {
            input_tokens: 3,
            output_tokens: 256,
            cache_creation_input_tokens: Some(274),
            cache_read_input_tokens: Some(2048),
            cache_creation_input_tokens_1h: None,
            server_tool_use: None,
        };
        let cost = estimate_cost(ModelPricing::SONNET_46, &usage);
        // cache_read: 2048 @ $0.30/MTok = $0.0006144
        // cache_write: 274 @ $3.75/MTok = $0.0010275
        assert!(cost.cache_read_cost > 0.0);
        assert!(cost.cache_write_cost > 0.0);
        assert!(cost.total() > 0.0);
    }

    /// Anchor values from <https://docs.claude.com/en/docs/about-claude/pricing>
    /// verified 2026-08-23. Update only against the vendor page.
    #[test]
    fn published_rates_match_vendor_page() {
        for (id, input, output, cache_read) in [
            ("claude-fable-5", 10.0, 50.0, 1.0),
            ("claude-mythos-5", 10.0, 50.0, 1.0),
            ("claude-opus-5", 5.0, 25.0, 0.50),
            ("claude-opus-4-8", 5.0, 25.0, 0.50),
            ("claude-opus-4-7", 5.0, 25.0, 0.50),
            ("claude-opus-4-6", 5.0, 25.0, 0.50),
            ("claude-opus-4-5", 5.0, 25.0, 0.50),
            ("claude-opus-4-1", 15.0, 75.0, 1.50),
            ("claude-sonnet-5", 2.0, 10.0, 0.20),
            ("claude-sonnet-4-6", 3.0, 15.0, 0.30),
            ("claude-sonnet-4-5", 3.0, 15.0, 0.30),
            ("claude-haiku-4-5", 1.0, 5.0, 0.10),
            ("claude-haiku-3-5", 0.80, 4.0, 0.08),
        ] {
            let p = ModelPricing::for_model_id(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!((p.input - input).abs() < 1e-9, "{id} input {} != {input}", p.input);
            assert!((p.output - output).abs() < 1e-9, "{id} output {} != {output}", p.output);
            assert!(
                (p.cache_read - cache_read).abs() < 1e-9,
                "{id} cache_read {} != {cache_read}",
                p.cache_read
            );
        }
    }

    /// Every model reachable through a `Model` factory must resolve to a price.
    #[test]
    fn factory_models_all_resolve_to_pricing() {
        for model in [
            Model::claude_sonnet_5(),
            Model::claude_opus_5(),
            Model::claude_fable_5(),
            Model::claude_mythos_5(),
        ] {
            assert!(ModelPricing::for_model(&model).is_some(), "{model} has no pricing entry");
        }
    }

    /// Dated aliases must resolve to their family's rates.
    #[test]
    fn dated_aliases_resolve() {
        let dated = ModelPricing::for_model_id("claude-sonnet-4-5-20250929").unwrap();
        assert!((dated.input - ModelPricing::SONNET_45.input).abs() < 1e-9);
        let dated = ModelPricing::for_model_id("claude-opus-4-5-20251101").unwrap();
        assert!((dated.input - ModelPricing::OPUS_45.input).abs() < 1e-9);
        assert!(ModelPricing::for_model_id("claude-99-turbo").is_none());
    }

    /// Anthropic documents cache writes as 1.25× (5m) and 2× (1h) base input, and
    /// cache reads as 0.1×. Asserting the ratios keeps this meaningful when list
    /// prices move.
    #[test]
    fn cache_multipliers_follow_documented_ratios() {
        for p in [
            ModelPricing::FABLE_5,
            ModelPricing::MYTHOS_5,
            ModelPricing::OPUS_5,
            ModelPricing::OPUS_48,
            ModelPricing::SONNET_5,
            ModelPricing::SONNET_46,
            ModelPricing::HAIKU_45,
            ModelPricing::HAIKU_35,
        ] {
            assert!((p.cache_write_5m - p.input * 1.25).abs() < 1e-9);
            assert!((p.cache_write_1h - p.input * 2.0).abs() < 1e-9);
            assert!((p.cache_read - p.input * 0.1).abs() < 1e-9);
        }
    }

    /// Fast mode is twice the standard rate for the models that support it.
    #[test]
    fn fast_mode_doubles_standard_rates() {
        assert!((ModelPricing::OPUS_5_FAST.input - ModelPricing::OPUS_5.input * 2.0).abs() < 1e-9);
        assert!(
            (ModelPricing::OPUS_5_FAST.output - ModelPricing::OPUS_5.output * 2.0).abs() < 1e-9
        );
        assert!((ModelPricing::OPUS_5_FAST.input - ModelPricing::OPUS_48.input * 2.0).abs() < 1e-9);
    }

    #[test]
    fn display_format() {
        let cost = CostBreakdown {
            input_cost: 0.003,
            cache_write_cost: 0.001,
            cache_read_cost: 0.0005,
            output_cost: 0.0075,
        };
        let s = cost.to_string();
        assert!(s.starts_with('$'));
        assert!(s.contains("in="));
        assert!(s.contains("out="));
    }
}
