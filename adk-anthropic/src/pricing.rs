//! Token pricing for Anthropic models.
//!
//! Provides per-model cost calculation from [`Usage`] data returned by the API.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_anthropic::pricing::{ModelPricing, estimate_cost};
//!
//! let cost = estimate_cost(ModelPricing::SONNET_46, &response.usage);
//! println!("Cost: ${:.6}", cost.total());
//! ```

use crate::types::Usage;

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
