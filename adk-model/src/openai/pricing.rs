//! Token pricing for OpenAI models.
//!
//! Provides per-model cost calculation based on token counts. Rates are the
//! published standard-tier list prices, last verified against
//! <https://developers.openai.com/api/docs/pricing> on 2026-08-23.
//!
//! Prompt caching is automatic, and the discount on cached input reads varies
//! by family: 90% for GPT-5.x, 75% for GPT-4.1, o3 and o4-mini, and 50% for
//! GPT-4o, o1 and o3-mini. The Pro tiers publish no cached rate, so their
//! `cached_input` mirrors `input`.
//!
//! # Limitations
//!
//! - **Cache writes are not modelled.** The GPT-5.6 family bills cache writes
//!   as a separate category; [`OpenAIPricing`] carries only read rates.
//! - **Long context is a separate constant, not a computed tier.** Where OpenAI
//!   publishes a long-context price, it is a `*_LONG` constant; callers pick the
//!   tier. Thresholds differ per model (272K for GPT-5.4 and GPT-5.5).
//! - **Fast mode is not modelled.** Its multiplier over standard is not uniform
//!   (roughly 1.75×–2.5× depending on the model), so it cannot be derived.
//!   [`OpenAIPricing::GPT_53_CODEX_FAST`] is the one published exception.
//! - **Regional (data residency) endpoints add a 10% uplift** for models
//!   released on or after 2026-03-05. Not applied here.
//! - [`lookup_pricing`] returns `None` for models OpenAI does not publish a rate
//!   for. Treat `None` as unpriced, never as free.
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
    // ── GPT-5.6 family ──
    //
    // Standard tier. `*_LONG` constants carry the long-context rates OpenAI
    // applies above each model's short-context threshold.

    /// GPT-5.6 Sol — flagship capability.
    ///
    /// Promotional pricing, available at least through 2026-11-21.
    pub const GPT_56_SOL: Self = Self { input: 4.00, cached_input: 0.40, output: 20.00 };

    /// GPT-5.6 Sol — long-context tier.
    pub const GPT_56_SOL_LONG: Self = Self { input: 8.00, cached_input: 0.80, output: 30.00 };

    /// GPT-5.6 Terra — balanced intelligence and cost.
    pub const GPT_56_TERRA: Self = Self { input: 2.00, cached_input: 0.20, output: 12.00 };

    /// GPT-5.6 Terra — long-context tier.
    pub const GPT_56_TERRA_LONG: Self = Self { input: 4.00, cached_input: 0.40, output: 18.00 };

    /// GPT-5.6 Luna — efficient high-volume tier.
    pub const GPT_56_LUNA: Self = Self { input: 0.20, cached_input: 0.02, output: 1.20 };

    /// GPT-5.6 Luna — long-context tier.
    pub const GPT_56_LUNA_LONG: Self = Self { input: 0.40, cached_input: 0.04, output: 1.80 };

    /// GPT-5.6 Cyber — Daybreak cyber model, aliased by `daybreak-red-latest`.
    pub const GPT_56_CYBER: Self = Self { input: 12.50, cached_input: 1.25, output: 75.00 };

    // ── GPT-5.5 family (90% cache discount) ──

    /// GPT-5.5 — short context (under 272K tokens).
    pub const GPT_55: Self = Self { input: 5.00, cached_input: 0.50, output: 30.00 };

    /// GPT-5.5 — long context (272K tokens and above).
    pub const GPT_55_LONG: Self = Self { input: 10.00, cached_input: 1.00, output: 45.00 };

    /// GPT-5.5 Pro — short context (under 272K tokens).
    ///
    /// OpenAI publishes no cached-input rate for the Pro tier, so `cached_input`
    /// mirrors `input` rather than understating a cache hit.
    pub const GPT_55_PRO: Self = Self { input: 30.00, cached_input: 30.00, output: 180.00 };

    /// GPT-5.5 Pro — long context (272K tokens and above).
    pub const GPT_55_PRO_LONG: Self = Self { input: 60.00, cached_input: 60.00, output: 270.00 };

    /// GPT-5.5 Cyber — Daybreak cyber model.
    pub const GPT_55_CYBER: Self = Self { input: 12.50, cached_input: 1.25, output: 75.00 };

    /// GPT-5.5 Instant.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const GPT_55_INSTANT: Self = Self { input: 0.50, cached_input: 0.05, output: 3.00 };

    // ── GPT-5.4 family (90% cache discount) ──

    /// GPT-5.4 — short context (under 272K tokens).
    pub const GPT_54: Self = Self { input: 2.50, cached_input: 0.25, output: 15.00 };

    /// GPT-5.4 — long context (272K tokens and above).
    pub const GPT_54_LONG: Self = Self { input: 5.00, cached_input: 0.50, output: 22.50 };

    /// GPT-5.4 Mini — strongest mini model for coding, computer use, subagents.
    pub const GPT_54_MINI: Self = Self { input: 0.75, cached_input: 0.075, output: 4.50 };

    /// GPT-5.4 Nano — cheapest GPT-5.4-class model for high-volume tasks.
    pub const GPT_54_NANO: Self = Self { input: 0.20, cached_input: 0.02, output: 1.25 };

    /// GPT-5.4 Pro — short context (under 272K tokens).
    ///
    /// `cached_input` mirrors `input`; see [`Self::GPT_55_PRO`].
    pub const GPT_54_PRO: Self = Self { input: 30.00, cached_input: 30.00, output: 180.00 };

    /// GPT-5.4 Pro — long context (272K tokens and above).
    pub const GPT_54_PRO_LONG: Self = Self { input: 60.00, cached_input: 60.00, output: 270.00 };

    // ── GPT-5.3 family (90% cache discount) ──

    /// GPT-5.3 Codex — code-optimized model.
    pub const GPT_53_CODEX: Self = Self { input: 1.75, cached_input: 0.175, output: 14.00 };

    /// GPT-5.3 Codex — fast-mode rates.
    pub const GPT_53_CODEX_FAST: Self = Self { input: 3.50, cached_input: 0.35, output: 28.00 };

    /// GPT-5.3 Chat Latest.
    #[deprecated(note = "OpenAI prices this endpoint as `chat-latest`; use Self::CHAT_LATEST")]
    pub const GPT_53_CHAT_LATEST: Self = Self { input: 1.50, cached_input: 0.15, output: 12.00 };

    /// ChatGPT `chat-latest` — the model serving ChatGPT.
    pub const CHAT_LATEST: Self = Self { input: 5.00, cached_input: 0.50, output: 30.00 };

    // ── GPT-5.2 family (90% cache discount) ──

    /// GPT-5.2 — general-purpose model.
    pub const GPT_52: Self = Self { input: 1.75, cached_input: 0.175, output: 14.00 };

    /// GPT-5.2 Pro — premium GPT-5.2-class model.
    ///
    /// `cached_input` mirrors `input`; see [`Self::GPT_55_PRO`].
    pub const GPT_52_PRO: Self = Self { input: 21.00, cached_input: 21.00, output: 168.00 };

    /// GPT-5.2 Codex.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const GPT_52_CODEX: Self = Self { input: 1.25, cached_input: 0.125, output: 10.00 };

    // ── GPT-5.1 family (90% cache discount) ──

    /// GPT-5.1 — general-purpose model.
    pub const GPT_51: Self = Self { input: 1.25, cached_input: 0.125, output: 10.00 };

    /// GPT-5.1 Codex.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const GPT_51_CODEX: Self = Self { input: 1.00, cached_input: 0.10, output: 8.00 };

    /// GPT-5.1 Codex Max.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const GPT_51_CODEX_MAX: Self = Self { input: 2.00, cached_input: 0.20, output: 16.00 };

    /// GPT-5.1 Codex Mini.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const GPT_51_CODEX_MINI: Self = Self { input: 0.30, cached_input: 0.03, output: 2.40 };

    // ── GPT-5 family (90% cache discount) ──

    /// GPT-5 — flagship agentic model.
    pub const GPT_5: Self = Self { input: 1.25, cached_input: 0.125, output: 10.00 };

    /// GPT-5 Mini — budget GPT-5-class model.
    pub const GPT_5_MINI: Self = Self { input: 0.25, cached_input: 0.025, output: 2.00 };

    /// GPT-5 Nano — cheapest GPT-5-class model.
    pub const GPT_5_NANO: Self = Self { input: 0.05, cached_input: 0.005, output: 0.40 };

    /// GPT-5 Pro — premium GPT-5-class model.
    ///
    /// `cached_input` mirrors `input`; see [`Self::GPT_55_PRO`].
    pub const GPT_5_PRO: Self = Self { input: 15.00, cached_input: 15.00, output: 120.00 };

    /// GPT-5 Search API — search-optimized GPT-5 endpoint.
    pub const GPT_5_SEARCH_API: Self = Self { input: 1.25, cached_input: 0.125, output: 10.00 };

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

    /// GPT-Realtime-2.1 — text pricing (audio is separate).
    pub const GPT_REALTIME_21_TEXT: Self = Self { input: 4.00, cached_input: 0.40, output: 24.00 };

    /// GPT-Realtime-2.1 — audio pricing.
    pub const GPT_REALTIME_21_AUDIO: Self =
        Self { input: 32.00, cached_input: 0.40, output: 64.00 };

    // ── Image generation ──

    /// GPT-Image-1.5 — text pricing.
    pub const GPT_IMAGE_15_TEXT: Self = Self { input: 5.00, cached_input: 1.25, output: 10.00 };

    /// GPT-Image-1.5 — image pricing.
    pub const GPT_IMAGE_15_IMAGE: Self = Self { input: 8.00, cached_input: 2.00, output: 32.00 };

    // ── GPT Image 2 ──

    /// GPT-Image-2 — text pricing.
    ///
    /// OpenAI publishes no text-output rate for this model; billed output is
    /// images, priced by [`Self::GPT_IMAGE_2_IMAGE`].
    pub const GPT_IMAGE_2_TEXT: Self = Self { input: 5.00, cached_input: 1.25, output: 0.0 };

    /// GPT-Image-2 — image pricing.
    pub const GPT_IMAGE_2_IMAGE: Self = Self { input: 8.00, cached_input: 2.00, output: 30.00 };

    // ── Deep research models ──

    /// o3 Deep Research.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const O3_DEEP_RESEARCH: Self = Self { input: 2.00, cached_input: 0.50, output: 8.00 };

    /// o4-mini Deep Research.
    #[deprecated(note = "no published OpenAI rate as of 2026-08-23; lookup_pricing returns None")]
    pub const O4_MINI_DEEP_RESEARCH: Self = Self { input: 1.10, cached_input: 0.275, output: 4.40 };
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

/// Look up pricing for a model by its identifier string.
///
/// Returns `None` when OpenAI publishes no rate for the model.
///
/// `None` means unpriced, not free — a caller that treats it as zero will
/// under-report spend on any model added since this table was verified.
///
/// # Arguments
///
/// * `model_name` - The model identifier (e.g., "gpt-5.6-terra", "gpt-4.1")
///
/// # Example
///
/// ```rust
/// use adk_model::openai::pricing::lookup_pricing;
///
/// let pricing = lookup_pricing("gpt-5.6-terra");
/// assert!(pricing.is_some());
///
/// let unknown = lookup_pricing("unknown-model");
/// assert!(unknown.is_none());
/// ```
pub fn lookup_pricing(model_name: &str) -> Option<&'static OpenAIPricing> {
    match model_name {
        // GPT-5.6 family. The family alias resolves to Sol, as does the
        // Daybreak blue alias; the red alias resolves to Cyber.
        "gpt-5.6" | "gpt-5.6-sol" | "daybreak-blue-latest" => Some(&OpenAIPricing::GPT_56_SOL),
        "gpt-5.6-terra" => Some(&OpenAIPricing::GPT_56_TERRA),
        "gpt-5.6-luna" => Some(&OpenAIPricing::GPT_56_LUNA),
        "gpt-5.6-cyber" | "daybreak-red-latest" => Some(&OpenAIPricing::GPT_56_CYBER),

        // GPT-5.5 family
        "gpt-5.5" => Some(&OpenAIPricing::GPT_55),
        "gpt-5.5-pro" => Some(&OpenAIPricing::GPT_55_PRO),
        "gpt-5.5-cyber" => Some(&OpenAIPricing::GPT_55_CYBER),

        // GPT-5.4 family
        "gpt-5.4" => Some(&OpenAIPricing::GPT_54),
        "gpt-5.4-mini" => Some(&OpenAIPricing::GPT_54_MINI),
        "gpt-5.4-nano" => Some(&OpenAIPricing::GPT_54_NANO),
        "gpt-5.4-pro" => Some(&OpenAIPricing::GPT_54_PRO),

        // GPT-5.3 family
        "gpt-5.3-codex" => Some(&OpenAIPricing::GPT_53_CODEX),

        // GPT-5.2 family
        "gpt-5.2" => Some(&OpenAIPricing::GPT_52),
        "gpt-5.2-pro" => Some(&OpenAIPricing::GPT_52_PRO),

        // GPT-5.1 family
        "gpt-5.1" => Some(&OpenAIPricing::GPT_51),

        // GPT-5 family
        "gpt-5" => Some(&OpenAIPricing::GPT_5),
        "gpt-5-mini" => Some(&OpenAIPricing::GPT_5_MINI),
        "gpt-5-nano" => Some(&OpenAIPricing::GPT_5_NANO),
        "gpt-5-pro" => Some(&OpenAIPricing::GPT_5_PRO),
        "gpt-5-search-api" => Some(&OpenAIPricing::GPT_5_SEARCH_API),

        // ChatGPT
        "chat-latest" => Some(&OpenAIPricing::CHAT_LATEST),

        // GPT-4.1 family
        "gpt-4.1" => Some(&OpenAIPricing::GPT_41),
        "gpt-4.1-mini" => Some(&OpenAIPricing::GPT_41_MINI),
        "gpt-4.1-nano" => Some(&OpenAIPricing::GPT_41_NANO),

        // o-series reasoning models
        "o3" => Some(&OpenAIPricing::O3),
        "o4-mini" => Some(&OpenAIPricing::O4_MINI),
        "o3-mini" => Some(&OpenAIPricing::O3_MINI),
        "o1" => Some(&OpenAIPricing::O1),

        // GPT-4o family (legacy)
        "gpt-4o" => Some(&OpenAIPricing::GPT_4O),
        "gpt-4o-mini" => Some(&OpenAIPricing::GPT_4O_MINI),

        // Realtime models
        "gpt-realtime-1.5" => Some(&OpenAIPricing::GPT_REALTIME_15_TEXT),
        "gpt-realtime-1.5-audio" => Some(&OpenAIPricing::GPT_REALTIME_15_AUDIO),
        "gpt-realtime-2.1" => Some(&OpenAIPricing::GPT_REALTIME_21_TEXT),
        "gpt-realtime-2.1-audio" => Some(&OpenAIPricing::GPT_REALTIME_21_AUDIO),

        // Image generation models
        "gpt-image-1.5" => Some(&OpenAIPricing::GPT_IMAGE_15_TEXT),
        "gpt-image-1.5-image" => Some(&OpenAIPricing::GPT_IMAGE_15_IMAGE),
        "gpt-image-2" => Some(&OpenAIPricing::GPT_IMAGE_2_TEXT),
        "gpt-image-2-image" => Some(&OpenAIPricing::GPT_IMAGE_2_IMAGE),

        // Unknown model — return None for zero-cost fallback
        _ => None,
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

    /// The GPT-5.x families discount cached input reads by 90%. Asserting the
    /// ratio rather than a literal keeps this test meaningful when list prices
    /// move.
    #[test]
    fn gpt_5_cache_discount_90_percent() {
        for p in [
            OpenAIPricing::GPT_5,
            OpenAIPricing::GPT_5_MINI,
            OpenAIPricing::GPT_5_NANO,
            OpenAIPricing::GPT_51,
            OpenAIPricing::GPT_52,
            OpenAIPricing::GPT_54,
            OpenAIPricing::GPT_55,
            OpenAIPricing::GPT_56_SOL,
            OpenAIPricing::GPT_56_TERRA,
            OpenAIPricing::GPT_56_LUNA,
        ] {
            assert!(
                (p.cached_input - p.input * 0.1).abs() < 1e-9,
                "cached {} is not 10% of input {}",
                p.cached_input,
                p.input
            );
        }
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

    #[test]
    fn lookup_known_models() {
        for id in [
            "gpt-5.6",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.6-cyber",
            "daybreak-blue-latest",
            "daybreak-red-latest",
            "gpt-realtime-2.1",
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.4-pro",
            "gpt-5.3-codex",
            "chat-latest",
            "gpt-5.2",
            "gpt-5.2-pro",
            "gpt-5.1",
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-5-pro",
            "gpt-5-search-api",
            "gpt-image-2",
            "gpt-image-2-image",
        ] {
            assert!(lookup_pricing(id).is_some(), "{id} should be priced");
        }
    }

    #[test]
    fn lookup_unknown_model_returns_none() {
        assert!(lookup_pricing("unknown-model").is_none());
        assert!(lookup_pricing("gpt-99").is_none());
        assert!(lookup_pricing("").is_none());
    }

    /// Models OpenAI publishes no rate for must report unpriced rather than a
    /// fabricated rate. Guards the regression that had five invented constants
    /// answering lookups.
    #[test]
    fn unpublished_models_are_unpriced() {
        for id in [
            "gpt-5.5-instant",
            "gpt-5.2-codex",
            "gpt-5.1-codex",
            "gpt-5.1-codex-max",
            "gpt-5.1-codex-mini",
            "gpt-5.3-chat-latest",
            "o3-deep-research",
            "o4-mini-deep-research",
        ] {
            assert!(lookup_pricing(id).is_none(), "{id} has no published rate");
        }
    }

    /// Anchor values transcribed from
    /// <https://developers.openai.com/api/docs/pricing> (standard tier, short
    /// context) on 2026-08-23. Update only against the vendor page.
    #[test]
    fn lookup_pricing_matches_published_rates() {
        for (id, input, cached, output) in [
            ("gpt-5.6-sol", 4.00, 0.40, 20.00),
            ("gpt-5.6-terra", 2.00, 0.20, 12.00),
            ("gpt-5.6-luna", 0.20, 0.02, 1.20),
            ("gpt-5.6-cyber", 12.50, 1.25, 75.00),
            ("gpt-5.5", 5.00, 0.50, 30.00),
            ("gpt-5.5-pro", 30.00, 30.00, 180.00),
            ("gpt-5.4", 2.50, 0.25, 15.00),
            ("gpt-5.4-mini", 0.75, 0.075, 4.50),
            ("gpt-5.4-nano", 0.20, 0.02, 1.25),
            ("gpt-5.4-pro", 30.00, 30.00, 180.00),
            ("gpt-5.3-codex", 1.75, 0.175, 14.00),
            ("chat-latest", 5.00, 0.50, 30.00),
            ("gpt-5.2", 1.75, 0.175, 14.00),
            ("gpt-5.2-pro", 21.00, 21.00, 168.00),
            ("gpt-5.1", 1.25, 0.125, 10.00),
            ("gpt-5", 1.25, 0.125, 10.00),
            ("gpt-5-mini", 0.25, 0.025, 2.00),
            ("gpt-5-nano", 0.05, 0.005, 0.40),
            ("gpt-5-pro", 15.00, 15.00, 120.00),
            ("gpt-4.1", 2.00, 0.50, 8.00),
            ("gpt-4.1-mini", 0.40, 0.10, 1.60),
            ("gpt-4.1-nano", 0.10, 0.025, 0.40),
            ("gpt-4o", 2.50, 1.25, 10.00),
            ("gpt-4o-mini", 0.15, 0.075, 0.60),
            ("o3", 2.00, 0.50, 8.00),
            ("o4-mini", 1.10, 0.275, 4.40),
            ("o3-mini", 1.10, 0.55, 4.40),
            ("o1", 15.00, 7.50, 60.00),
        ] {
            let p = lookup_pricing(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!((p.input - input).abs() < 1e-9, "{id} input {} != {input}", p.input);
            assert!(
                (p.cached_input - cached).abs() < 1e-9,
                "{id} cached {} != {cached}",
                p.cached_input
            );
            assert!((p.output - output).abs() < 1e-9, "{id} output {} != {output}", p.output);
        }
    }

    /// Long-context constants must be strictly more expensive than their
    /// short-context counterparts, and the aliases must agree with their target.
    #[test]
    fn tier_and_alias_invariants() {
        for (short, long) in [
            (OpenAIPricing::GPT_56_SOL, OpenAIPricing::GPT_56_SOL_LONG),
            (OpenAIPricing::GPT_56_TERRA, OpenAIPricing::GPT_56_TERRA_LONG),
            (OpenAIPricing::GPT_56_LUNA, OpenAIPricing::GPT_56_LUNA_LONG),
            (OpenAIPricing::GPT_55, OpenAIPricing::GPT_55_LONG),
            (OpenAIPricing::GPT_54, OpenAIPricing::GPT_54_LONG),
            (OpenAIPricing::GPT_55_PRO, OpenAIPricing::GPT_55_PRO_LONG),
            (OpenAIPricing::GPT_54_PRO, OpenAIPricing::GPT_54_PRO_LONG),
        ] {
            assert!(long.input > short.input);
            assert!(long.output > short.output);
        }

        let sol = lookup_pricing("gpt-5.6-sol").unwrap();
        assert!((lookup_pricing("daybreak-blue-latest").unwrap().input - sol.input).abs() < 1e-9);
        let cyber = lookup_pricing("gpt-5.6-cyber").unwrap();
        assert!((lookup_pricing("daybreak-red-latest").unwrap().input - cyber.input).abs() < 1e-9);
    }
}
