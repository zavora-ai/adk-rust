//! Token pricing for Gemini models.
//!
//! Provides per-model cost calculation based on token counts.
//!
//! Gemini models have tiered pricing: a base rate for contexts up to 200K tokens,
//! and a higher rate for longer contexts. Models that support caching have separate
//! cache input and cache storage rates.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_gemini::pricing::{GeminiPricing, CostBreakdown, estimate_cost};
//!
//! let cost = estimate_cost(&GeminiPricing::GEMINI_25_FLASH, 10_000, 500, 0);
//! println!("Cost: ${:.6}", cost.total());
//! ```

/// Per-million-token prices for a single Gemini model tier.
///
/// All values are in USD per 1 million tokens. Models with long-context
/// pricing (>200K tokens) have separate `input_long` and `output_long` rates.
/// For models without long-context tiers, these equal the base rates.
#[derive(Debug, Clone, Copy)]
pub struct GeminiPricing {
    /// Base input token price ($/MTok) for contexts ≤200K tokens.
    pub input: f64,
    /// Input token price ($/MTok) for contexts >200K tokens.
    pub input_long: f64,
    /// Base output token price ($/MTok) for contexts ≤200K tokens.
    pub output: f64,
    /// Output token price ($/MTok) for contexts >200K tokens.
    pub output_long: f64,
    /// Cache input token price ($/MTok) for contexts ≤200K tokens.
    pub cache_input: f64,
    /// Cache input token price ($/MTok) for contexts >200K tokens.
    pub cache_input_long: f64,
    /// Cache storage price ($/MTok per hour).
    pub cache_storage_per_hour: f64,
}

impl GeminiPricing {
    /// Gemini 3.1 Pro Preview
    pub const GEMINI_31_PRO_PREVIEW: Self = Self {
        input: 2.00,
        input_long: 4.00,
        output: 12.00,
        output_long: 18.00,
        cache_input: 0.20,
        cache_input_long: 0.40,
        cache_storage_per_hour: 4.50,
    };

    /// Gemini 3.1 Flash Lite
    pub const GEMINI_31_FLASH_LITE: Self = Self {
        input: 0.25,
        input_long: 0.25,
        output: 1.50,
        output_long: 1.50,
        cache_input: 0.025,
        cache_input_long: 0.025,
        cache_storage_per_hour: 1.00,
    };

    /// Gemini 3 Flash Preview
    pub const GEMINI_3_FLASH_PREVIEW: Self = Self {
        input: 0.50,
        input_long: 0.50,
        output: 3.00,
        output_long: 3.00,
        cache_input: 0.05,
        cache_input_long: 0.10,
        cache_storage_per_hour: 1.00,
    };

    /// Gemini 2.5 Pro
    pub const GEMINI_25_PRO: Self = Self {
        input: 1.25,
        input_long: 2.50,
        output: 10.00,
        output_long: 15.00,
        cache_input: 0.125,
        cache_input_long: 0.25,
        cache_storage_per_hour: 4.50,
    };

    /// Gemini 2.5 Flash
    pub const GEMINI_25_FLASH: Self = Self {
        input: 0.30,
        input_long: 0.30,
        output: 2.50,
        output_long: 2.50,
        cache_input: 0.03,
        cache_input_long: 0.10,
        cache_storage_per_hour: 1.00,
    };

    /// Gemini 2.5 Flash Lite (no caching support)
    pub const GEMINI_25_FLASH_LITE: Self = Self {
        input: 0.10,
        input_long: 0.10,
        output: 0.40,
        output_long: 0.40,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 2.0 Flash (no caching support)
    pub const GEMINI_20_FLASH: Self = Self {
        input: 0.10,
        input_long: 0.10,
        output: 0.40,
        output_long: 0.40,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 3.1 Flash Live Preview (realtime model).
    ///
    /// Text input $0.75/MTok, audio input $3.00/MTok, image/video input $1.00/MTok.
    /// Text output $4.50/MTok, audio output $12.00/MTok.
    /// Rates here use text input/output; for audio use the audio-specific rates directly.
    pub const GEMINI_31_FLASH_LIVE: Self = Self {
        input: 0.75,
        input_long: 0.75,
        output: 4.50,
        output_long: 4.50,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 2.5 Flash Native Audio (Live API).
    ///
    /// Text input $0.50/MTok, audio/video input $3.00/MTok.
    /// Text output $2.00/MTok, audio output $12.00/MTok.
    /// Rates here use text input/output.
    pub const GEMINI_25_FLASH_NATIVE_AUDIO: Self = Self {
        input: 0.50,
        input_long: 0.50,
        output: 2.00,
        output_long: 2.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 3.1 Flash Image Preview.
    ///
    /// Text/image input $0.50/MTok. Text/thinking output $3.00/MTok.
    /// Image output ~$60/MTok (roughly $0.045–$0.151 per image depending on resolution).
    /// Rates here use text input/output; image output is significantly higher.
    pub const GEMINI_31_FLASH_IMAGE: Self = Self {
        input: 0.50,
        input_long: 0.50,
        output: 3.00,
        output_long: 3.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 2.5 Flash Image.
    ///
    /// Text/image input $0.30/MTok. Image output ~$30/MTok (~$0.039/image).
    pub const GEMINI_25_FLASH_IMAGE: Self = Self {
        input: 0.30,
        input_long: 0.30,
        output: 30.00,
        output_long: 30.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 3 Pro Image Preview.
    ///
    /// Text/image input $2.00/MTok. Text/thinking output $12.00/MTok.
    /// Image output ~$120/MTok.
    pub const GEMINI_3_PRO_IMAGE: Self = Self {
        input: 2.00,
        input_long: 2.00,
        output: 12.00,
        output_long: 12.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 2.5 Computer Use Preview.
    pub const GEMINI_25_COMPUTER_USE: Self = Self {
        input: 1.25,
        input_long: 2.50,
        output: 10.00,
        output_long: 15.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 2.5 Flash Preview TTS.
    ///
    /// Text input $0.50/MTok. Audio output $10.00/MTok.
    pub const GEMINI_25_FLASH_TTS: Self = Self {
        input: 0.50,
        input_long: 0.50,
        output: 10.00,
        output_long: 10.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini 2.5 Pro Preview TTS.
    ///
    /// Text input $1.00/MTok. Audio output $20.00/MTok.
    pub const GEMINI_25_PRO_TTS: Self = Self {
        input: 1.00,
        input_long: 1.00,
        output: 20.00,
        output_long: 20.00,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini Embedding (text-only). Input $0.15/MTok.
    pub const GEMINI_EMBEDDING: Self = Self {
        input: 0.15,
        input_long: 0.15,
        output: 0.0,
        output_long: 0.0,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };

    /// Gemini Embedding 2 Preview (multimodal).
    ///
    /// Text $0.20/MTok, image $0.45/MTok, audio $6.50/MTok, video $12.00/MTok.
    /// Rate here uses text input.
    pub const GEMINI_EMBEDDING_2: Self = Self {
        input: 0.20,
        input_long: 0.20,
        output: 0.0,
        output_long: 0.0,
        cache_input: 0.0,
        cache_input_long: 0.0,
        cache_storage_per_hour: 0.0,
    };
}

/// Itemised cost breakdown from a single API call.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostBreakdown {
    /// Cost of input tokens.
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

/// Estimate the cost of a single API call using base (≤200K) pricing.
///
/// # Arguments
///
/// * `pricing` - The model's pricing tier
/// * `input_tokens` - Number of input tokens (excluding cached)
/// * `output_tokens` - Number of output tokens
/// * `cached_tokens` - Number of tokens served from cache
///
/// # Example
///
/// ```rust,ignore
/// use adk_gemini::pricing::{GeminiPricing, estimate_cost};
///
/// let cost = estimate_cost(&GeminiPricing::GEMINI_25_FLASH, 50_000, 1_000, 10_000);
/// println!("Total: ${:.6}", cost.total());
/// ```
pub fn estimate_cost(
    pricing: &GeminiPricing,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> CostBreakdown {
    let mtok = 1_000_000.0;
    CostBreakdown {
        input_cost: input_tokens as f64 / mtok * pricing.input,
        cache_cost: cached_tokens as f64 / mtok * pricing.cache_input,
        output_cost: output_tokens as f64 / mtok * pricing.output,
    }
}

/// Same as [`estimate_cost`] but uses long-context (>200K) pricing.
pub fn estimate_cost_long(
    pricing: &GeminiPricing,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> CostBreakdown {
    let mtok = 1_000_000.0;
    CostBreakdown {
        input_cost: input_tokens as f64 / mtok * pricing.input_long,
        cache_cost: cached_tokens as f64 / mtok * pricing.cache_input_long,
        output_cost: output_tokens as f64 / mtok * pricing.output_long,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_25_flash_basic_cost() {
        let cost = estimate_cost(&GeminiPricing::GEMINI_25_FLASH, 1_000_000, 1_000_000, 0);
        // 1M input @ $0.30/MTok = $0.30
        assert!((cost.input_cost - 0.30).abs() < 1e-9);
        // 1M output @ $2.50/MTok = $2.50
        assert!((cost.output_cost - 2.50).abs() < 1e-9);
        assert!((cost.total() - 2.80).abs() < 1e-9);
    }

    #[test]
    fn gemini_25_pro_with_cache() {
        let cost = estimate_cost(&GeminiPricing::GEMINI_25_PRO, 500_000, 100_000, 200_000);
        // 500K input @ $1.25/MTok = $0.625
        assert!((cost.input_cost - 0.625).abs() < 1e-9);
        // 200K cached @ $0.125/MTok = $0.025
        assert!((cost.cache_cost - 0.025).abs() < 1e-9);
        // 100K output @ $10.00/MTok = $1.00
        assert!((cost.output_cost - 1.00).abs() < 1e-9);
        assert!((cost.total() - 1.65).abs() < 1e-9);
    }

    #[test]
    fn gemini_25_pro_long_context() {
        let cost = estimate_cost_long(&GeminiPricing::GEMINI_25_PRO, 1_000_000, 1_000_000, 0);
        // 1M input @ $2.50/MTok = $2.50
        assert!((cost.input_cost - 2.50).abs() < 1e-9);
        // 1M output @ $15.00/MTok = $15.00
        assert!((cost.output_cost - 15.00).abs() < 1e-9);
        assert!((cost.total() - 17.50).abs() < 1e-9);
    }

    #[test]
    fn no_cache_model_zero_cache_cost() {
        let cost = estimate_cost(&GeminiPricing::GEMINI_20_FLASH, 1_000_000, 1_000_000, 500_000);
        // cache_input is 0.0, so cache cost should be 0
        assert!((cost.cache_cost - 0.0).abs() < 1e-9);
        // input: 1M @ $0.10 = $0.10
        assert!((cost.input_cost - 0.10).abs() < 1e-9);
        // output: 1M @ $0.40 = $0.40
        assert!((cost.output_cost - 0.40).abs() < 1e-9);
    }

    #[test]
    fn zero_tokens_zero_cost() {
        let cost = estimate_cost(&GeminiPricing::GEMINI_25_PRO, 0, 0, 0);
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
