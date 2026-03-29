//! Token pricing for OpenAI models (March 2026).
//!
//! Provides per-model cost calculation based on token counts.
//!
//! OpenAI models have automatic prompt caching with varying discount tiers:
//! - GPT-5 family: 90% off cached input reads
//! - GPT-4.1 family: 75% off cached input reads
//! - GPT-4o / o-series: 50% off cached input reads
//!
//! # Example
//!
//! ```rust
//! use adk_model::openai::pricing::{OpenAIPricing, estimate_cost};
//!
//! let cost = estimate_cost(&OpenAIPricing::GPT_41, 50_000, 1_000, 10_000);
//! println!("Total: ${:.6}", cost.total());
//! ```

/// Per-million-token prices for a single OpenAI model.
///
/// All values are in USD per 1 million tokens.
#[derive(Debug, Clone, Copy)]
pub struct OpenAIPricing {
    /// Input token price ($/MTok).
    pub input: f64,
    /// Cached input token price ($/MTok).
    pub cached_input: f64,
    /// Output token price ($/MTok).
    pub output: f64,
}

impl OpenAIPricing {
    // ── GPT-5 family (90% cache discount) ──

    /// GPT-5.4 — most capable model for professional work.
    pub const GPT_54: Self = Self { input: 2.50, cached_input: 0.25, output: 15.00 };

    /// GPT-5.4 Mini — strongest mini model for coding, computer use, subagents.
    pub const GPT_54_MINI: Self = Self { input: 0.75, cached_input: 0.075, output: 4.50 };

    /// GPT-5.4 Nano — cheapest GPT-5.4-class model for high-volume tasks.
    pub const GPT_54_NANO: Self = Self { input: 0.20, cached_input: 0.02, output: 1.25 };

    /// GPT-5 — flagship agentic model.
    pub const GPT_5: Self = Self { input: 1.25, cached_input: 0.125, output: 10.00 };

    /// GPT-5 Mini — budget GPT-5-class model.
    pub const GPT_5_MINI: Self = Self { input: 0.25, cached_input: 0.025, output: 2.00 };

    // ── GPT-4.1 family (75% cache discount) ──

    /// GPT-4.1 — production workhorse, 1M context window.
    pub const GPT_41: Self = Self { input: 2.00, cached_input: 0.50, output: 8.00 };

    /// GPT-4.1 Mini — mid-tier production tasks, 1M context.
    pub const GPT_41_MINI: Self = Self { input: 0.40, cached_input: 0.10, output: 1.60 };

    /// GPT-4.1 Nano — classification, routing, extraction, 1M context.
    pub const GPT_41_NANO: Self = Self { input: 0.10, cached_input: 0.025, output: 0.40 };

    // ── o-series reasoning models (50% cache discount) ──

    /// o3 — advanced reasoning model.
    pub const O3: Self = Self { input: 2.00, cached_input: 0.50, output: 8.00 };

    /// o4-mini — best-value reasoning model.
    pub const O4_MINI: Self = Self { input: 1.10, cached_input: 0.275, output: 4.40 };

    /// o3-mini — legacy reasoning model.
    pub const O3_MINI: Self = Self { input: 1.10, cached_input: 0.55, output: 4.40 };

    /// o1 — legacy deep reasoning model.
    pub const O1: Self = Self { input: 15.00, cached_input: 7.50, output: 60.00 };

    // ── GPT-4o family (50% cache discount, legacy) ──

    /// GPT-4o — legacy production model.
    pub const GPT_4O: Self = Self { input: 2.50, cached_input: 1.25, output: 10.00 };

    /// GPT-4o Mini — legacy simple tasks.
    pub const GPT_4O_MINI: Self = Self { input: 0.15, cached_input: 0.075, output: 0.60 };

    // ── Realtime models ──

    /// GPT-Realtime-1.5 — text pricing (audio is separate).
    ///
    /// Audio: input $32/MTok, cached $0.40/MTok, output $64/MTok.
    /// Image: input $5/MTok, cached $0.50/MTok.
    pub const GPT_REALTIME_15_TEXT: Self = Self { input: 4.00, cached_input: 0.40, output: 16.00 };

    /// GPT-Realtime-1.5 — audio pricing.
    pub const GPT_REALTIME_15_AUDIO: Self =
        Self { input: 32.00, cached_input: 0.40, output: 64.00 };

    // ── Image generation ──

    /// GPT-Image-1.5 — text pricing.
    pub const GPT_IMAGE_15_TEXT: Self = Self { input: 5.00, cached_input: 1.25, output: 10.00 };

    /// GPT-Image-1.5 — image pricing.
    pub const GPT_IMAGE_15_IMAGE: Self = Self { input: 8.00, cached_input: 2.00, output: 32.00 };
}

/// Itemised cost breakdown from a single API call.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostBreakdown {
    /// Cost of uncached input tokens.
    pub input_cost: f64,
    /// Cost of cached input tokens.
    pub cache_cost: f64,
    /// Cost of output tokens.
    pub output_cost: f64,
}

impl CostBreakdown {
    /// Total cost in USD.
    pub fn total(&self) -> f64 {
        self.input_cost + self.cache_cost + self.output_cost
    }
}

impl std::fmt::Display for CostBreakdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "${:.6} (in=${:.6} cache=${:.6} out=${:.6})",
            self.total(),
            self.input_cost,
            self.cache_cost,
            self.output_cost
        )
    }
}

/// Estimate the cost of a single API call.
///
/// # Arguments
///
/// * `pricing` - The model's pricing tier
/// * `input_tokens` - Number of uncached input tokens
/// * `output_tokens` - Number of output tokens (includes reasoning tokens for o-series)
/// * `cached_tokens` - Number of tokens served from cache
///
/// # Example
///
/// ```rust
/// use adk_model::openai::pricing::{OpenAIPricing, estimate_cost};
///
/// let cost = estimate_cost(&OpenAIPricing::GPT_41, 50_000, 1_000, 10_000);
/// println!("Total: ${:.6}", cost.total());
/// ```
pub fn estimate_cost(
    pricing: &OpenAIPricing,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> CostBreakdown {
    let mtok = 1_000_000.0;
    CostBreakdown {
        input_cost: input_tokens as f64 / mtok * pricing.input,
        cache_cost: cached_tokens as f64 / mtok * pricing.cached_input,
        output_cost: output_tokens as f64 / mtok * pricing.output,
    }
}

/// Estimate batch API cost (50% off all token costs).
pub fn estimate_batch_cost(
    pricing: &OpenAIPricing,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> CostBreakdown {
    let mtok = 1_000_000.0;
    CostBreakdown {
        input_cost: input_tokens as f64 / mtok * pricing.input * 0.5,
        cache_cost: cached_tokens as f64 / mtok * pricing.cached_input * 0.5,
        output_cost: output_tokens as f64 / mtok * pricing.output * 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_41_basic_cost() {
        let cost = estimate_cost(&OpenAIPricing::GPT_41, 1_000_000, 1_000_000, 0);
        assert!((cost.input_cost - 2.00).abs() < 1e-9);
        assert!((cost.output_cost - 8.00).abs() < 1e-9);
        assert!((cost.total() - 10.00).abs() < 1e-9);
    }

    #[test]
    fn gpt_41_with_cache() {
        let cost = estimate_cost(&OpenAIPricing::GPT_41, 500_000, 100_000, 500_000);
        // 500K input @ $2.00/MTok = $1.00
        assert!((cost.input_cost - 1.00).abs() < 1e-9);
        // 500K cached @ $0.50/MTok = $0.25
        assert!((cost.cache_cost - 0.25).abs() < 1e-9);
        // 100K output @ $8.00/MTok = $0.80
        assert!((cost.output_cost - 0.80).abs() < 1e-9);
        assert!((cost.total() - 2.05).abs() < 1e-9);
    }

    #[test]
    fn gpt_5_cache_discount_90_percent() {
        // GPT-5: input $1.25, cached $0.125 (90% off)
        let cost = estimate_cost(&OpenAIPricing::GPT_5, 0, 0, 1_000_000);
        assert!((cost.cache_cost - 0.125).abs() < 1e-9);
    }

    #[test]
    fn o4_mini_reasoning_cost() {
        // o4-mini: 1M input + 5M output (reasoning tokens count as output)
        let cost = estimate_cost(&OpenAIPricing::O4_MINI, 1_000_000, 5_000_000, 0);
        assert!((cost.input_cost - 1.10).abs() < 1e-9);
        assert!((cost.output_cost - 22.00).abs() < 1e-9);
    }

    #[test]
    fn batch_50_percent_discount() {
        let standard = estimate_cost(&OpenAIPricing::GPT_41, 1_000_000, 1_000_000, 0);
        let batch = estimate_batch_cost(&OpenAIPricing::GPT_41, 1_000_000, 1_000_000, 0);
        assert!((batch.total() - standard.total() * 0.5).abs() < 1e-9);
    }

    #[test]
    fn gpt_41_nano_cheapest() {
        let cost = estimate_cost(&OpenAIPricing::GPT_41_NANO, 1_000_000, 1_000_000, 0);
        assert!((cost.input_cost - 0.10).abs() < 1e-9);
        assert!((cost.output_cost - 0.40).abs() < 1e-9);
    }

    #[test]
    fn zero_tokens_zero_cost() {
        let cost = estimate_cost(&OpenAIPricing::GPT_5, 0, 0, 0);
        assert!((cost.total() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn display_format() {
        let cost = CostBreakdown { input_cost: 0.003, cache_cost: 0.001, output_cost: 0.0075 };
        let s = cost.to_string();
        assert!(s.starts_with('$'));
        assert!(s.contains("in="));
        assert!(s.contains("cache="));
        assert!(s.contains("out="));
    }
}
