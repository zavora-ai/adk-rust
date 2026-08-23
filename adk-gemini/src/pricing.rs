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
    /// Gemini 3.7 Flash introductory pricing through December 31, 2026.
    ///
    /// From January 1, 2027 these rates double to $1.50 input, $7.50 output,
    /// $0.15 cache and $1.00/hour storage.
    pub const GEMINI_37_FLASH: Self = Self {
        input: 0.75,
        input_long: 0.75,
        output: 3.75,
        output_long: 3.75,
        cache_input: 0.075,
        cache_input_long: 0.075,
        cache_storage_per_hour: 0.50,
    };

    /// Gemini 3.6 Flash introductory pricing through December 31, 2026.
    ///
    /// From January 1, 2027 these rates double, as for [`Self::GEMINI_37_FLASH`].
    pub const GEMINI_36_FLASH: Self = Self {
        input: 0.75,
        input_long: 0.75,
        output: 3.75,
        output_long: 3.75,
        cache_input: 0.075,
        cache_input_long: 0.075,
        cache_storage_per_hour: 0.50,
    };

    /// Gemini 3.5 Flash-Lite (GA). The most cost-efficient GA model.
    pub const GEMINI_35_FLASH_LITE: Self = Self {
        input: 0.30,
        input_long: 0.30,
        output: 2.50,
        output_long: 2.50,
        cache_input: 0.03,
        cache_input_long: 0.03,
        cache_storage_per_hour: 1.00,
    };

    /// Gemini 3.5 Flash (GA). Input $1.50/MTok, output $9.00/MTok (incl. thinking).
    pub const GEMINI_35_FLASH: Self = Self {
        input: 1.50,
        input_long: 1.50,
        output: 9.00,
        output_long: 9.00,
        cache_input: 0.15,
        cache_input_long: 0.15,
        cache_storage_per_hour: 1.00,
    };

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

    /// Gemini 3 Flash Preview.
    ///
    /// Audio input is $1.00/MTok and audio cache $0.10/MTok; this constant
    /// carries the text/image/video rates. There is no long-context tier.
    pub const GEMINI_3_FLASH_PREVIEW: Self = Self {
        input: 0.50,
        input_long: 0.50,
        output: 3.00,
        output_long: 3.00,
        cache_input: 0.05,
        cache_input_long: 0.05,
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

    /// Gemini 2.5 Flash.
    ///
    /// Audio input is $1.00/MTok and audio cache $0.10/MTok; this constant
    /// carries the text/image/video rates. There is no long-context tier, so the
    /// `*_long` rates equal the base rates.
    pub const GEMINI_25_FLASH: Self = Self {
        input: 0.30,
        input_long: 0.30,
        output: 2.50,
        output_long: 2.50,
        cache_input: 0.03,
        cache_input_long: 0.03,
        cache_storage_per_hour: 1.00,
    };

    /// Gemini 2.5 Flash Lite.
    ///
    /// Audio input is $0.30/MTok and audio cache $0.03/MTok; this constant
    /// carries the text/image/video rates.
    pub const GEMINI_25_FLASH_LITE: Self = Self {
        input: 0.10,
        input_long: 0.10,
        output: 0.40,
        output_long: 0.40,
        cache_input: 0.01,
        cache_input_long: 0.01,
        cache_storage_per_hour: 1.00,
    };

    /// Gemini 2.0 Flash.
    #[deprecated(note = "Gemini 2.0 Flash was shut down on June 1, 2026; rates are historical")]
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

    /// Returns the standard (paid-tier) pricing for a known [`Model`](crate::client::Model), if one is
    /// defined.
    ///
    /// Returns `None` for models without published per-token text pricing (e.g.
    /// [`Model::Custom`](crate::client::Model::Custom), video/music models, or models that are free of charge).
    /// Image and audio models return their text input/output rates; their media
    /// output is billed at significantly higher rates not captured here.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_gemini::{Model, pricing::GeminiPricing};
    ///
    /// let pricing = GeminiPricing::for_model(&Model::Gemini35Flash).unwrap();
    /// assert_eq!(pricing.input, 1.50);
    /// ```
    pub fn for_model(model: &crate::client::Model) -> Option<Self> {
        use crate::client::Model;
        #[allow(deprecated)]
        let pricing = match model {
            Model::Gemini35Flash => Self::GEMINI_35_FLASH,
            Model::Gemini31ProPreview => Self::GEMINI_31_PRO_PREVIEW,
            Model::Gemini31FlashLite => Self::GEMINI_31_FLASH_LITE,
            Model::Gemini31FlashImage => Self::GEMINI_31_FLASH_IMAGE,
            Model::Gemini3FlashPreview => Self::GEMINI_3_FLASH_PREVIEW,
            Model::Gemini3ProImage | Model::Gemini3ProImagePreview => Self::GEMINI_3_PRO_IMAGE,
            Model::Gemini25Pro => Self::GEMINI_25_PRO,
            Model::Gemini25ProPreviewTts => Self::GEMINI_25_PRO_TTS,
            Model::Gemini25Flash | Model::Gemini25FlashPreview092025 => Self::GEMINI_25_FLASH,
            Model::Gemini25FlashImage | Model::Gemini25FlashImagePreview => {
                Self::GEMINI_25_FLASH_IMAGE
            }
            Model::Gemini25FlashPreviewTts => Self::GEMINI_25_FLASH_TTS,
            Model::Gemini25FlashLite | Model::Gemini25FlashLitePreview092025 => {
                Self::GEMINI_25_FLASH_LITE
            }
            Model::Gemini25FlashLive122025 | Model::Gemini25FlashLive092025 => {
                Self::GEMINI_25_FLASH_NATIVE_AUDIO
            }
            Model::GeminiEmbedding2 => Self::GEMINI_EMBEDDING_2,
            Model::GeminiEmbedding001 => Self::GEMINI_EMBEDDING,
            // No published per-token text pricing (Pro Image preview text uses 3.1 Pro
            // rates; the dedicated Gemini 3 Pro Preview text model is discontinued).
            Model::Gemini3ProPreview => Self::GEMINI_31_PRO_PREVIEW,
            Model::Custom(id) => return Self::for_model_id(id),
        };
        Some(pricing)
    }

    /// Returns the standard (paid-tier) pricing for a raw Gemini model ID.
    ///
    /// Accepts both the bare ID and the `models/`-prefixed resource name. This is
    /// the resolution path for models exposed through
    /// [`Model::Custom`](crate::client::Model::Custom) factories
    /// rather than enum variants.
    ///
    /// Returns `None` when Google publishes no per-token text price for the ID.
    /// `None` means unpriced, never free.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_gemini::pricing::GeminiPricing;
    ///
    /// let p = GeminiPricing::for_model_id("gemini-3.7-flash").unwrap();
    /// assert_eq!(p.input, 0.75);
    /// ```
    pub fn for_model_id(model_id: &str) -> Option<Self> {
        let bare = model_id.strip_prefix("models/").unwrap_or(model_id);
        let pricing = match bare {
            "gemini-3.7-flash" => Self::GEMINI_37_FLASH,
            "gemini-3.6-flash" => Self::GEMINI_36_FLASH,
            "gemini-3.5-flash" => Self::GEMINI_35_FLASH,
            "gemini-3.5-flash-lite" => Self::GEMINI_35_FLASH_LITE,
            "gemini-3.1-pro-preview" => Self::GEMINI_31_PRO_PREVIEW,
            "gemini-3.1-flash-lite" => Self::GEMINI_31_FLASH_LITE,
            "gemini-3.1-flash-image" => Self::GEMINI_31_FLASH_IMAGE,
            "gemini-3.1-flash-live-preview" => Self::GEMINI_31_FLASH_LIVE,
            "gemini-3-flash-preview" => Self::GEMINI_3_FLASH_PREVIEW,
            "gemini-3-pro-image" => Self::GEMINI_3_PRO_IMAGE,
            "gemini-2.5-pro" => Self::GEMINI_25_PRO,
            "gemini-2.5-flash" => Self::GEMINI_25_FLASH,
            "gemini-2.5-flash-lite" => Self::GEMINI_25_FLASH_LITE,
            "gemini-2.5-flash-image" => Self::GEMINI_25_FLASH_IMAGE,
            "gemini-2.5-computer-use-preview-10-2025" => Self::GEMINI_25_COMPUTER_USE,
            "gemini-embedding-2" => Self::GEMINI_EMBEDDING_2,
            "gemini-embedding-001" => Self::GEMINI_EMBEDDING,
            _ => return None,
        };
        Some(pricing)
    }
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
    fn gemini_35_flash_basic_cost() {
        let cost = estimate_cost(&GeminiPricing::GEMINI_35_FLASH, 1_000_000, 1_000_000, 0);
        // 1M input @ $1.50/MTok = $1.50
        assert!((cost.input_cost - 1.50).abs() < 1e-9);
        // 1M output @ $9.00/MTok = $9.00
        assert!((cost.output_cost - 9.00).abs() < 1e-9);
        assert!((cost.total() - 10.50).abs() < 1e-9);
    }

    #[test]
    fn for_model_maps_known_models() {
        use crate::client::Model;
        // GA flagship Flash
        let p = GeminiPricing::for_model(&Model::Gemini35Flash).unwrap();
        assert!((p.input - 1.50).abs() < 1e-9);
        // GA Flash-Lite and its (deprecated) preview share pricing
        let lite = GeminiPricing::for_model(&Model::Gemini31FlashLite).unwrap();
        assert!((lite.input - 0.25).abs() < 1e-9);
        // Embedding 2
        let emb = GeminiPricing::for_model(&Model::GeminiEmbedding2).unwrap();
        assert!((emb.input - 0.20).abs() < 1e-9);
        // Custom models with no published pricing
        assert!(GeminiPricing::for_model(&Model::Custom("models/x".into())).is_none());
        let current = GeminiPricing::for_model(&Model::gemini_3_7_flash()).unwrap();
        assert_eq!(current.input, GeminiPricing::GEMINI_37_FLASH.input);
        assert_eq!(current.output, GeminiPricing::GEMINI_37_FLASH.output);
    }

    /// Every model reachable through a `Model` factory must resolve to a price.
    /// A factory without a matching `for_model_id` arm silently reports no cost.
    #[test]
    fn factory_models_all_resolve_to_pricing() {
        use crate::client::Model;
        for model in [
            Model::gemini_3_7_flash(),
            Model::gemini_3_6_flash(),
            Model::gemini_3_5_flash_lite(),
            Model::default(),
        ] {
            assert!(
                GeminiPricing::for_model(&model).is_some(),
                "{} has no pricing entry",
                model.as_str()
            );
        }
    }

    /// Resolution must accept both the bare ID and the `models/` resource name.
    #[test]
    fn for_model_id_accepts_both_id_forms() {
        let bare = GeminiPricing::for_model_id("gemini-3.6-flash").unwrap();
        let prefixed = GeminiPricing::for_model_id("models/gemini-3.6-flash").unwrap();
        assert_eq!(bare.input, prefixed.input);
        assert_eq!(bare.input, 0.75);
        assert!(GeminiPricing::for_model_id("gemini-nonexistent").is_none());
    }

    /// Anchor values from <https://ai.google.dev/gemini-api/docs/pricing>
    /// (paid tier, standard) verified 2026-08-23. Update only against the page.
    #[test]
    fn published_rates_match_vendor_page() {
        for (id, input, output, cache) in [
            ("gemini-3.7-flash", 0.75, 3.75, 0.075),
            ("gemini-3.6-flash", 0.75, 3.75, 0.075),
            ("gemini-3.5-flash", 1.50, 9.00, 0.15),
            ("gemini-3.5-flash-lite", 0.30, 2.50, 0.03),
            ("gemini-3.1-pro-preview", 2.00, 12.00, 0.20),
            ("gemini-3.1-flash-lite", 0.25, 1.50, 0.025),
            ("gemini-3-flash-preview", 0.50, 3.00, 0.05),
            ("gemini-2.5-pro", 1.25, 10.00, 0.125),
            ("gemini-2.5-flash", 0.30, 2.50, 0.03),
            ("gemini-2.5-flash-lite", 0.10, 0.40, 0.01),
            ("gemini-embedding-2", 0.20, 0.0, 0.0),
            ("gemini-embedding-001", 0.15, 0.0, 0.0),
        ] {
            let p = GeminiPricing::for_model_id(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!((p.input - input).abs() < 1e-9, "{id} input {} != {input}", p.input);
            assert!((p.output - output).abs() < 1e-9, "{id} output {} != {output}", p.output);
            assert!(
                (p.cache_input - cache).abs() < 1e-9,
                "{id} cache {} != {cache}",
                p.cache_input
            );
        }
    }

    /// Flash-tier models have no long-context tier, so the `*_long` rates must
    /// equal the base rates. Guards the regression where audio cache rates were
    /// stored in `cache_input_long`.
    #[test]
    fn flash_tiers_have_no_long_context_premium() {
        for p in [
            GeminiPricing::GEMINI_37_FLASH,
            GeminiPricing::GEMINI_36_FLASH,
            GeminiPricing::GEMINI_35_FLASH,
            GeminiPricing::GEMINI_35_FLASH_LITE,
            GeminiPricing::GEMINI_31_FLASH_LITE,
            GeminiPricing::GEMINI_3_FLASH_PREVIEW,
            GeminiPricing::GEMINI_25_FLASH,
            GeminiPricing::GEMINI_25_FLASH_LITE,
        ] {
            assert_eq!(p.input, p.input_long);
            assert_eq!(p.output, p.output_long);
            assert_eq!(p.cache_input, p.cache_input_long);
        }
    }

    /// Models that support context caching must carry a non-zero cache rate and
    /// storage price, or cached tokens are silently billed as free.
    #[test]
    fn cache_capable_models_price_cache_reads() {
        for p in [
            GeminiPricing::GEMINI_37_FLASH,
            GeminiPricing::GEMINI_36_FLASH,
            GeminiPricing::GEMINI_35_FLASH,
            GeminiPricing::GEMINI_35_FLASH_LITE,
            GeminiPricing::GEMINI_31_FLASH_LITE,
            GeminiPricing::GEMINI_31_PRO_PREVIEW,
            GeminiPricing::GEMINI_3_FLASH_PREVIEW,
            GeminiPricing::GEMINI_25_PRO,
            GeminiPricing::GEMINI_25_FLASH,
            GeminiPricing::GEMINI_25_FLASH_LITE,
        ] {
            assert!(p.cache_input > 0.0);
            assert!(p.cache_storage_per_hour > 0.0);
        }
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
        // A model without a cache rate must contribute no cache cost. Built
        // inline rather than from a retired model's constant.
        let uncached = GeminiPricing {
            input: 0.10,
            input_long: 0.10,
            output: 0.40,
            output_long: 0.40,
            cache_input: 0.0,
            cache_input_long: 0.0,
            cache_storage_per_hour: 0.0,
        };
        let cost = estimate_cost(&uncached, 1_000_000, 1_000_000, 500_000);
        assert!((cost.cache_cost - 0.0).abs() < 1e-9);
        assert!((cost.input_cost - 0.10).abs() < 1e-9);
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
