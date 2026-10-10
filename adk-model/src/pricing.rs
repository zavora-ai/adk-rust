//! Token prices for every provider and cost calculation from [`UsageMetadata`](adk_core::UsageMetadata).
//!
//! [`PricingCatalog`] is the single table `adk-model` uses to fill
//! [`UsageMetadata::cost`](adk_core::UsageMetadata::cost) on provider responses that do not report a cost
//! themselves. It consolidates the vendor tables in `adk_gemini::pricing`,
//! `adk_model::openai::pricing` and `adk_anthropic::pricing` (tests keep the
//! rates in step) and adds DeepSeek list prices. Every rate is USD per one
//! million tokens.
//!
//! # Usage normalization
//!
//! `adk-model` providers report `prompt_token_count` *including* cache reads
//! and cache writes, and `candidates_token_count` including thinking tokens.
//! The catalog bills each prompt token once:
//!
//! | Tokens | Rate |
//! |--------|------|
//! | `prompt − cache reads − cache writes` | `input` |
//! | `cache_read_input_token_count` | `cache_read` |
//! | 5-minute cache writes | `cache_write_5m` |
//! | 1-hour cache writes (Anthropic `cache_creation_input_tokens_1h`) | `cache_write_1h` |
//! | `candidates_token_count` | `output` |
//!
//! # Model resolution
//!
//! Lookup strips resource prefixes (`models/`, `publishers/…/models/`,
//! `vendor/`), Bedrock region and vendor prefixes (`us.anthropic.`) and
//! version suffixes (`-v1:0`), then matches the identifier exactly or as a
//! dated alias of a listed model: `claude-sonnet-4-5-20250929`,
//! `gpt-4.1-2025-04-14` and `claude-sonnet-4-5@20250929` resolve, while an
//! unlisted point release such as `claude-opus-4-9` does not.
//!
//! A vendor's list price applies only to providers that bill it: Google rates
//! to `gemini` and `openrouter`; OpenAI rates to `openai`, `openai-responses`,
//! `azure-openai`, `azure-ai` and `openrouter`; Anthropic rates to `anthropic`,
//! `bedrock`, `azure-ai` and `openrouter`; DeepSeek rates to `deepseek`.
//! `ollama` runs locally and costs zero, except `*cloud*` models.
//!
//! # Limitations
//!
//! - Standard tier only: batch, flex, priority, fast mode, data-residency and
//!   regional uplifts are not applied.
//! - Image, audio and realtime output rates are not modelled; models whose
//!   output is billed by modality are left out, so they resolve to `None`.
//! - GPT-5.6 long-context rates are not applied: the threshold is unpublished.
//! - Cache storage (Gemini per-hour storage) is not a per-request cost.
//! - DeepSeek cache hits are billed at the cache-miss rate and off-peak
//!   discounts are ignored, which over-estimates.
//!
//! `None` from [`PricingCatalog::cost_usd`] means the cost is unknown, never
//! that the request was free.
//!
//! # Example
//!
//! ```
//! use adk_core::UsageMetadata;
//! use adk_model::pricing::PricingCatalog;
//!
//! let usage = UsageMetadata {
//!     prompt_token_count: 1_000_000,
//!     candidates_token_count: 100_000,
//!     total_token_count: 1_100_000,
//!     cache_read_input_token_count: Some(400_000),
//!     ..Default::default()
//! };
//! // 600K uncached at $2.00 + 400K cached at $0.50 + 100K output at $8.00.
//! let cost = PricingCatalog::standard().cost_usd(Some("openai"), "gpt-4.1", &usage).unwrap();
//! assert!((cost - 2.20).abs() < 1e-9);
//! assert!(PricingCatalog::standard().cost_usd(Some("openai"), "my-fine-tune", &usage).is_none());
//! ```

use adk_core::UsageMetadata;
use chrono::NaiveDate;
use std::sync::LazyLock;

/// Date the standard catalog's rates were last verified against the vendors'
/// published price lists.
pub const PRICING_EFFECTIVE_DATE: &str = "2026-10-10";

/// The vendor whose published list price a [`ModelPrice`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PriceVendor {
    /// Google Gemini.
    Google,
    /// OpenAI.
    OpenAI,
    /// Anthropic Claude.
    Anthropic,
    /// DeepSeek.
    DeepSeek,
}

impl PriceVendor {
    /// Returns `true` when `provider` bills this vendor's models at list price.
    pub fn billed_by(self, provider: &str) -> bool {
        match self {
            Self::Google => matches!(provider, "gemini" | "openrouter"),
            Self::OpenAI => matches!(
                provider,
                "openai" | "openai-responses" | "azure-openai" | "azure-ai" | "openrouter"
            ),
            Self::Anthropic => {
                matches!(provider, "anthropic" | "bedrock" | "azure-ai" | "openrouter")
            }
            Self::DeepSeek => provider == "deepseek",
        }
    }
}

/// Per-million-token prices for one rate card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRates {
    /// Uncached input tokens.
    pub input: f64,
    /// Output tokens, including thinking tokens.
    pub output: f64,
    /// Input tokens read from a cache.
    pub cache_read: f64,
    /// Input tokens written to a 5-minute (or untyped) cache entry.
    pub cache_write_5m: f64,
    /// Input tokens written to a 1-hour cache entry.
    pub cache_write_1h: f64,
}

impl ModelRates {
    /// Rates with cache reads and writes billed at the input rate.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::pricing::ModelRates;
    ///
    /// let rates = ModelRates::new(2.0, 8.0).with_cache_read(0.5);
    /// assert_eq!(rates.cache_read, 0.5);
    /// assert_eq!(rates.cache_write_5m, 2.0);
    /// ```
    pub const fn new(input: f64, output: f64) -> Self {
        Self { input, output, cache_read: input, cache_write_5m: input, cache_write_1h: input }
    }

    /// Sets the cache-read rate.
    pub const fn with_cache_read(mut self, rate: f64) -> Self {
        self.cache_read = rate;
        self
    }

    /// Sets the 5-minute and 1-hour cache-write rates.
    pub const fn with_cache_writes(mut self, five_minute: f64, one_hour: f64) -> Self {
        self.cache_write_5m = five_minute;
        self.cache_write_1h = one_hour;
        self
    }
}

/// A rate card that replaces the base rates once a prompt reaches a size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongContextTier {
    /// Smallest prompt, cache reads and writes included, billed at `rates`.
    pub min_prompt_tokens: u64,
    /// Rates for the whole request once the threshold is reached.
    pub rates: ModelRates,
}

/// A scheduled change to a model's rates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateChange {
    /// First day (UTC) the new rates apply.
    pub effective: NaiveDate,
    /// Rates from `effective` onward.
    pub rates: ModelRates,
}

/// The price of one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPrice {
    /// Vendor whose list price this is.
    pub vendor: PriceVendor,
    /// Canonical model identifier.
    pub model: String,
    /// Base rate card.
    pub rates: ModelRates,
    /// Long-context rate card, when the vendor publishes one.
    pub long_context: Option<LongContextTier>,
    /// Scheduled change to the base rates, such as the end of an introductory price.
    pub change: Option<RateChange>,
}

impl ModelPrice {
    /// Creates a price with only base rates.
    pub fn new(vendor: PriceVendor, model: impl Into<String>, rates: ModelRates) -> Self {
        Self { vendor, model: model.into(), rates, long_context: None, change: None }
    }

    /// Adds a long-context tier.
    pub fn with_long_context(mut self, min_prompt_tokens: u64, rates: ModelRates) -> Self {
        self.long_context = Some(LongContextTier { min_prompt_tokens, rates });
        self
    }

    /// Schedules new base rates from `effective` onward.
    pub fn with_change(mut self, effective: NaiveDate, rates: ModelRates) -> Self {
        self.change = Some(RateChange { effective, rates });
        self
    }

    /// The rate card for a prompt of `prompt_tokens` on `date`.
    pub fn rates_for(&self, prompt_tokens: u64, date: NaiveDate) -> ModelRates {
        if let Some(tier) = self.long_context
            && prompt_tokens >= tier.min_prompt_tokens
        {
            return tier.rates;
        }
        match self.change {
            Some(change) if date >= change.effective => change.rates,
            _ => self.rates,
        }
    }

    /// Cost in USD of one response's usage, priced on `date`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::UsageMetadata;
    /// use adk_model::pricing::{ModelPrice, ModelRates, PriceVendor};
    ///
    /// let price = ModelPrice::new(PriceVendor::Google, "my-model", ModelRates::new(1.0, 4.0));
    /// let usage = UsageMetadata {
    ///     prompt_token_count: 500_000,
    ///     candidates_token_count: 250_000,
    ///     ..Default::default()
    /// };
    /// let today = chrono::Utc::now().date_naive();
    /// assert!((price.cost_usd(&usage, today) - 1.5).abs() < 1e-9);
    /// ```
    pub fn cost_usd(&self, usage: &UsageMetadata, date: NaiveDate) -> f64 {
        let prompt = non_negative(usage.prompt_token_count);
        let cache_read = non_negative(usage.cache_read_input_token_count.unwrap_or(0));
        let cache_write = non_negative(usage.cache_creation_input_token_count.unwrap_or(0));
        let write_1h = one_hour_cache_writes(usage).min(cache_write);
        let write_5m = cache_write - write_1h;
        let uncached = prompt.saturating_sub(cache_read.saturating_add(cache_write));
        let output = non_negative(usage.candidates_token_count);
        let rates = self.rates_for(prompt, date);
        (uncached as f64 * rates.input
            + cache_read as f64 * rates.cache_read
            + write_5m as f64 * rates.cache_write_5m
            + write_1h as f64 * rates.cache_write_1h
            + output as f64 * rates.output)
            / 1_000_000.0
    }
}

/// A table of model prices, looked up by provider and model identifier.
///
/// [`PricingCatalog::standard`] is the table every `adk-model` provider uses.
/// Build a custom table with [`PricingCatalog::new`] and
/// [`with_price`](Self::with_price) to price fine-tuned or self-hosted models
/// in your own [`Llm`](adk_core::Llm) implementations.
///
/// # Example
///
/// ```
/// use adk_core::UsageMetadata;
/// use adk_model::pricing::{ModelPrice, ModelRates, PriceVendor, PricingCatalog};
///
/// let catalog = PricingCatalog::new("internal-2026-10")
///     .with_price(ModelPrice::new(PriceVendor::OpenAI, "ft-support-bot", ModelRates::new(3.0, 12.0)));
/// let usage = UsageMetadata { prompt_token_count: 1_000_000, ..Default::default() };
/// assert_eq!(catalog.cost_usd(None, "ft-support-bot", &usage), Some(3.0));
/// assert_eq!(catalog.version(), "internal-2026-10");
/// ```
#[derive(Debug, Clone)]
pub struct PricingCatalog {
    version: String,
    prices: Vec<ModelPrice>,
}

impl PricingCatalog {
    /// Creates an empty catalog labelled `version`.
    pub fn new(version: impl Into<String>) -> Self {
        Self { version: version.into(), prices: Vec::new() }
    }

    /// The standard catalog of published list prices, verified on
    /// [`PRICING_EFFECTIVE_DATE`].
    pub fn standard() -> &'static PricingCatalog {
        static STANDARD: LazyLock<PricingCatalog> = LazyLock::new(standard_catalog);
        &STANDARD
    }

    /// Adds or replaces the price for `price.model`.
    pub fn with_price(mut self, price: ModelPrice) -> Self {
        self.prices.retain(|existing| existing.model != price.model);
        self.prices.push(price);
        self
    }

    /// Version label; for the standard catalog, the verification date.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Every price in the catalog.
    pub fn prices(&self) -> &[ModelPrice] {
        &self.prices
    }

    /// Resolves the price for `model` as served by `provider`.
    ///
    /// `provider` is the [`LlmResponse::provider`](adk_core::LlmResponse::provider)
    /// identifier. `None` matches any vendor, for callers that price a model
    /// they know to be billed at list price.
    pub fn lookup(&self, provider: Option<&str>, model: &str) -> Option<&ModelPrice> {
        let canonical = canonical_model_id(model);
        let candidate =
            self.prices.iter().find(|price| price.model == canonical).or_else(|| {
                self.prices
                    .iter()
                    .filter(|price| {
                        canonical.strip_prefix(price.model.as_str()).is_some_and(is_version_suffix)
                    })
                    .max_by_key(|price| price.model.len())
            })?;
        match provider {
            Some(provider) if !candidate.vendor.billed_by(provider) => None,
            _ => Some(candidate),
        }
    }

    /// Cost in USD of one response's usage, or `None` when the model is unpriced.
    ///
    /// Local Ollama models cost zero.
    pub fn cost_usd(
        &self,
        provider: Option<&str>,
        model: &str,
        usage: &UsageMetadata,
    ) -> Option<f64> {
        if provider == Some("ollama") {
            return (!model.contains("cloud")).then_some(0.0);
        }
        let price = self.lookup(provider, model)?;
        Some(price.cost_usd(usage, chrono::Utc::now().date_naive()))
    }
}

fn non_negative(tokens: i32) -> u64 {
    u64::try_from(tokens).unwrap_or(0)
}

/// Anthropic reports the 1-hour share of `cache_creation_input_tokens`
/// separately; it is carried in the retained provider usage.
fn one_hour_cache_writes(usage: &UsageMetadata) -> u64 {
    let Some(provider_usage) = usage.provider_usage.as_ref() else {
        return 0;
    };
    provider_usage
        .get("cache_creation_input_tokens_1h")
        .or_else(|| provider_usage.pointer("/cache_creation/ephemeral_1h_input_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Strips transport prefixes and suffixes so a served identifier matches the catalog.
fn canonical_model_id(model: &str) -> String {
    let lowered = model.trim().to_ascii_lowercase();
    let mut id = lowered.rsplit('/').next().unwrap_or(&lowered);
    // Bedrock cross-region inference profiles and vendor prefixes.
    for region in ["us.", "eu.", "apac.", "global.", "us-gov.", "jp.", "au.", "ca."] {
        if let Some(rest) = id.strip_prefix(region) {
            id = rest;
            break;
        }
    }
    if let Some(rest) = id.strip_prefix("anthropic.") {
        id = rest;
    }
    // Bedrock model versions: `-v1:0`, `-v2`.
    let id = match id.rfind("-v") {
        Some(index)
            if id[index + 2..]
                .split(':')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) =>
        {
            &id[..index]
        }
        _ => id,
    };
    // Vertex AI pins Claude versions with `@`; OpenRouter writes Claude versions with dots.
    let mut id = id.replace('@', "-");
    if id.starts_with("claude-") {
        id = id.replace('.', "-");
    }
    id
}

/// A dated or revision suffix: `-20250929`, `-2025-04-14`, `-001` or `-latest`.
fn is_version_suffix(rest: &str) -> bool {
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    if rest == "latest" {
        return true;
    }
    let digits =
        |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    if rest.len() >= 3 && rest.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    let parts: Vec<&str> = rest.split('-').collect();
    matches!(parts.as_slice(), [year, month, day] if digits(year, 4) && digits(month, 2) && digits(day, 2))
}

fn standard_catalog() -> PricingCatalog {
    use PriceVendor::{Anthropic, DeepSeek, Google, OpenAI};

    let gemini = |model: &str, input: f64, output: f64, cache_read: f64| {
        ModelPrice::new(Google, model, ModelRates::new(input, output).with_cache_read(cache_read))
    };
    let openai = |model: &str, input: f64, output: f64, cache_read: f64| {
        ModelPrice::new(OpenAI, model, ModelRates::new(input, output).with_cache_read(cache_read))
    };
    let claude = |model: &str, input: f64, output: f64, read: f64, write_5m: f64, write_1h: f64| {
        ModelPrice::new(
            Anthropic,
            model,
            ModelRates::new(input, output)
                .with_cache_read(read)
                .with_cache_writes(write_5m, write_1h),
        )
    };
    // Gemini's long-context rates apply to prompts over 200K tokens.
    const GEMINI_LONG: u64 = 200_001;
    // OpenAI's long-context rates apply from 272K tokens.
    const OPENAI_LONG: u64 = 272_000;
    // Gemini 3.7 and 3.6 Flash introductory rates end on 2026-12-31.
    let flash_2027 = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap_or(NaiveDate::MAX);
    let flash_2027_rates = ModelRates::new(1.50, 7.50).with_cache_read(0.15);

    let prices = vec![
        // ── Google Gemini ──
        gemini("gemini-3.7-flash", 0.75, 3.75, 0.075).with_change(flash_2027, flash_2027_rates),
        gemini("gemini-3.6-flash", 0.75, 3.75, 0.075).with_change(flash_2027, flash_2027_rates),
        gemini("gemini-3.5-flash", 1.50, 9.00, 0.15),
        gemini("gemini-3.5-flash-lite", 0.30, 2.50, 0.03),
        gemini("gemini-3.1-pro-preview", 2.00, 12.00, 0.20)
            .with_long_context(GEMINI_LONG, ModelRates::new(4.00, 18.00).with_cache_read(0.40)),
        gemini("gemini-3-pro-preview", 2.00, 12.00, 0.20)
            .with_long_context(GEMINI_LONG, ModelRates::new(4.00, 18.00).with_cache_read(0.40)),
        gemini("gemini-3.1-flash-lite", 0.25, 1.50, 0.025),
        gemini("gemini-3-flash-preview", 0.50, 3.00, 0.05),
        gemini("gemini-2.5-pro", 1.25, 10.00, 0.125)
            .with_long_context(GEMINI_LONG, ModelRates::new(2.50, 15.00).with_cache_read(0.25)),
        gemini("gemini-2.5-flash", 0.30, 2.50, 0.03),
        gemini("gemini-2.5-flash-preview-09-2025", 0.30, 2.50, 0.03),
        gemini("gemini-2.5-flash-lite", 0.10, 0.40, 0.01),
        gemini("gemini-2.5-flash-lite-preview-09-2025", 0.10, 0.40, 0.01),
        // Image output is billed at this rate; text output costs less.
        gemini("gemini-2.5-flash-image", 0.30, 30.00, 0.30),
        gemini("gemini-2.5-flash-image-preview", 0.30, 30.00, 0.30),
        gemini("gemini-2.5-computer-use-preview-10-2025", 1.25, 10.00, 1.25)
            .with_long_context(GEMINI_LONG, ModelRates::new(2.50, 15.00)),
        gemini("gemini-2.5-flash-preview-tts", 0.50, 10.00, 0.50),
        gemini("gemini-2.5-pro-preview-tts", 1.00, 20.00, 1.00),
        gemini("gemini-embedding-2", 0.20, 0.0, 0.20),
        gemini("gemini-embedding-001", 0.15, 0.0, 0.15),
        // ── OpenAI ──
        openai("gpt-5.6", 4.00, 20.00, 0.40),
        openai("gpt-5.6-sol", 4.00, 20.00, 0.40),
        openai("daybreak-blue-latest", 4.00, 20.00, 0.40),
        openai("gpt-5.6-terra", 2.00, 12.00, 0.20),
        openai("gpt-5.6-luna", 0.20, 1.20, 0.02),
        openai("gpt-5.6-cyber", 12.50, 75.00, 1.25),
        openai("daybreak-red-latest", 12.50, 75.00, 1.25),
        openai("gpt-5.5", 5.00, 30.00, 0.50)
            .with_long_context(OPENAI_LONG, ModelRates::new(10.00, 45.00).with_cache_read(1.00)),
        openai("gpt-5.5-pro", 30.00, 180.00, 30.00)
            .with_long_context(OPENAI_LONG, ModelRates::new(60.00, 270.00)),
        openai("gpt-5.5-cyber", 12.50, 75.00, 1.25),
        openai("gpt-5.4", 2.50, 15.00, 0.25)
            .with_long_context(OPENAI_LONG, ModelRates::new(5.00, 22.50).with_cache_read(0.50)),
        openai("gpt-5.4-mini", 0.75, 4.50, 0.075),
        openai("gpt-5.4-nano", 0.20, 1.25, 0.02),
        openai("gpt-5.4-pro", 30.00, 180.00, 30.00)
            .with_long_context(OPENAI_LONG, ModelRates::new(60.00, 270.00)),
        openai("gpt-5.3-codex", 1.75, 14.00, 0.175),
        openai("gpt-5.2", 1.75, 14.00, 0.175),
        openai("gpt-5.2-pro", 21.00, 168.00, 21.00),
        openai("gpt-5.1", 1.25, 10.00, 0.125),
        openai("gpt-5", 1.25, 10.00, 0.125),
        openai("gpt-5-mini", 0.25, 2.00, 0.025),
        openai("gpt-5-nano", 0.05, 0.40, 0.005),
        openai("gpt-5-pro", 15.00, 120.00, 15.00),
        openai("gpt-5-search-api", 1.25, 10.00, 0.125),
        openai("chat-latest", 5.00, 30.00, 0.50),
        openai("gpt-4.1", 2.00, 8.00, 0.50),
        openai("gpt-4.1-mini", 0.40, 1.60, 0.10),
        openai("gpt-4.1-nano", 0.10, 0.40, 0.025),
        openai("o3", 2.00, 8.00, 0.50),
        openai("o4-mini", 1.10, 4.40, 0.275),
        openai("o3-mini", 1.10, 4.40, 0.55),
        openai("o1", 15.00, 60.00, 7.50),
        openai("gpt-4o", 2.50, 10.00, 1.25),
        openai("gpt-4o-mini", 0.15, 0.60, 0.075),
        // ── Anthropic ──
        claude("claude-fable-5-1", 10.0, 50.0, 0.25, 12.5, 20.0),
        claude("claude-mythos-5-1", 10.0, 50.0, 0.25, 12.5, 20.0),
        claude("claude-fable-5", 10.0, 50.0, 1.0, 12.5, 20.0),
        claude("claude-mythos-5", 10.0, 50.0, 1.0, 12.5, 20.0),
        claude("claude-opus-5-5", 4.0, 20.0, 0.20, 5.0, 8.0),
        claude("claude-opus-5", 5.0, 25.0, 0.50, 6.25, 10.0),
        claude("claude-sonnet-5-5", 2.0, 10.0, 0.10, 2.5, 4.0),
        claude("claude-sonnet-5", 2.0, 10.0, 0.20, 2.5, 4.0),
        // Haiku 5.5 bills the whole request at the long-prompt rate over 100K tokens.
        claude("claude-haiku-5-5", 0.10, 0.50, 0.01, 0.125, 0.20).with_long_context(
            100_001,
            ModelRates::new(0.50, 2.50).with_cache_read(0.05).with_cache_writes(0.625, 1.0),
        ),
        claude("claude-opus-4-8", 5.0, 25.0, 0.50, 6.25, 10.0),
        claude("claude-opus-4-7", 5.0, 25.0, 0.50, 6.25, 10.0),
        claude("claude-opus-4-6", 5.0, 25.0, 0.50, 6.25, 10.0),
        claude("claude-opus-4-5", 5.0, 25.0, 0.50, 6.25, 10.0),
        claude("claude-opus-4-1", 15.0, 75.0, 1.50, 18.75, 30.0),
        claude("claude-opus-4", 15.0, 75.0, 1.50, 18.75, 30.0),
        claude("claude-sonnet-4-6", 3.0, 15.0, 0.30, 3.75, 6.0),
        claude("claude-sonnet-4-5", 3.0, 15.0, 0.30, 3.75, 6.0),
        claude("claude-sonnet-4", 3.0, 15.0, 0.30, 3.75, 6.0),
        claude("claude-haiku-4-5", 1.0, 5.0, 0.10, 1.25, 2.0),
        claude("claude-haiku-3-5", 0.80, 4.0, 0.08, 1.0, 1.60),
        claude("claude-3-5-haiku", 0.80, 4.0, 0.08, 1.0, 1.60),
        // ── DeepSeek (peak, cache-miss rates) ──
        ModelPrice::new(DeepSeek, "deepseek-flash", ModelRates::new(0.44, 1.32)),
        ModelPrice::new(DeepSeek, "deepseek-v4-flash", ModelRates::new(0.44, 1.32)),
        ModelPrice::new(DeepSeek, "deepseek-chat", ModelRates::new(0.44, 1.32)),
        ModelPrice::new(DeepSeek, "deepseek-v4-pro", ModelRates::new(1.32, 3.96)),
    ];
    PricingCatalog { version: PRICING_EFFECTIVE_DATE.to_string(), prices }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).unwrap()
    }

    fn usage(prompt: i32, output: i32) -> UsageMetadata {
        UsageMetadata {
            prompt_token_count: prompt,
            candidates_token_count: output,
            total_token_count: prompt + output,
            ..Default::default()
        }
    }

    fn price(provider: &str, model: &str) -> &'static ModelPrice {
        PricingCatalog::standard()
            .lookup(Some(provider), model)
            .unwrap_or_else(|| panic!("{provider}/{model} should be priced"))
    }

    #[test]
    fn gemini_cached_tokens_are_billed_once() {
        // promptTokenCount includes cachedContentTokenCount.
        let usage = UsageMetadata {
            cache_read_input_token_count: Some(400_000),
            ..usage(1_000_000, 200_000)
        };
        let cost = price("gemini", "gemini-2.5-flash").cost_usd(&usage, today());
        // 600K × $0.30 + 400K × $0.03 + 200K × $2.50
        assert!((cost - (0.18 + 0.012 + 0.50)).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn gemini_long_context_switches_the_whole_request() {
        let price = price("gemini", "models/gemini-2.5-pro");
        let short = price.cost_usd(&usage(200_000, 0), today());
        let long = price.cost_usd(&usage(200_001, 0), today());
        assert!((short - 0.25).abs() < 1e-9, "{short}");
        assert!((long - 200_001.0 * 2.50 / 1e6).abs() < 1e-9, "{long}");
    }

    #[test]
    fn gemini_flash_introductory_rates_end_in_2027() {
        let price = price("gemini", "gemini-3.7-flash");
        let before = price.cost_usd(&usage(1_000_000, 0), today());
        let after =
            price.cost_usd(&usage(1_000_000, 0), NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
        assert!((before - 0.75).abs() < 1e-9);
        assert!((after - 1.50).abs() < 1e-9);
    }

    #[test]
    fn anthropic_cache_writes_split_by_ttl() {
        // Normalized prompt = input + cache reads + cache writes.
        let usage = UsageMetadata {
            prompt_token_count: 1_000 + 200_000 + 100_000,
            candidates_token_count: 2_000,
            cache_read_input_token_count: Some(200_000),
            cache_creation_input_token_count: Some(100_000),
            provider_usage: Some(serde_json::json!({ "cache_creation_input_tokens_1h": 40_000 })),
            ..Default::default()
        };
        let cost = price("anthropic", "claude-sonnet-4-5-20250929").cost_usd(&usage, today());
        let expected =
            (1_000.0 * 3.0 + 200_000.0 * 0.30 + 60_000.0 * 3.75 + 40_000.0 * 6.0 + 2_000.0 * 15.0)
                / 1e6;
        assert!((cost - expected).abs() < 1e-9, "{cost} vs {expected}");
    }

    #[test]
    fn haiku_55_long_prompt_counts_cached_tokens() {
        let usage =
            UsageMetadata { cache_read_input_token_count: Some(90_000), ..usage(100_001, 0) };
        let cost = price("anthropic", "claude-haiku-5-5").cost_usd(&usage, today());
        let expected = (10_001.0 * 0.50 + 90_000.0 * 0.05) / 1e6;
        assert!((cost - expected).abs() < 1e-12, "{cost}");
    }

    #[test]
    fn openai_long_context_starts_at_272k() {
        let price = price("openai", "gpt-5.5");
        assert_eq!(price.rates_for(271_999, today()).input, 5.00);
        assert_eq!(price.rates_for(272_000, today()).input, 10.00);
    }

    #[test]
    fn dated_aliases_resolve_and_unlisted_releases_do_not() {
        let catalog = PricingCatalog::standard();
        for (provider, model, expected) in [
            ("openai", "gpt-4.1-2025-04-14", "gpt-4.1"),
            ("openai", "gpt-4o-mini-2024-07-18", "gpt-4o-mini"),
            ("anthropic", "claude-opus-4-1-20250805", "claude-opus-4-1"),
            ("anthropic", "claude-3-5-haiku-20241022", "claude-3-5-haiku"),
            ("bedrock", "us.anthropic.claude-sonnet-4-5-20250929-v1:0", "claude-sonnet-4-5"),
            ("bedrock", "anthropic.claude-haiku-4-5-20251001-v1:0", "claude-haiku-4-5"),
            ("openrouter", "anthropic/claude-sonnet-4.5", "claude-sonnet-4-5"),
            ("openrouter", "google/gemini-3.7-flash", "gemini-3.7-flash"),
            ("gemini", "models/gemini-2.5-flash-001", "gemini-2.5-flash"),
            ("anthropic", "claude-opus-5-5-latest", "claude-opus-5-5"),
        ] {
            assert_eq!(
                catalog.lookup(Some(provider), model).map(|price| price.model.as_str()),
                Some(expected),
                "{provider}/{model}"
            );
        }
        for (provider, model) in [
            ("anthropic", "claude-opus-4-9"),
            ("openai", "gpt-5-turbo"),
            ("openai", "gpt-image-2"),
            ("gemini", "gemini-3.1-flash-live-preview"),
            ("groq", "openai/gpt-oss-120b"),
            ("together", "gpt-4.1"),
            ("deepseek", "gpt-4.1"),
            ("opencode", "claude-sonnet-5"),
        ] {
            assert!(catalog.lookup(Some(provider), model).is_none(), "{provider}/{model}");
        }
    }

    #[test]
    fn local_ollama_is_free_and_cloud_ollama_is_unpriced() {
        let catalog = PricingCatalog::standard();
        assert_eq!(catalog.cost_usd(Some("ollama"), "qwen3.5", &usage(10, 10)), Some(0.0));
        assert_eq!(catalog.cost_usd(Some("ollama"), "gpt-oss:120b-cloud", &usage(10, 10)), None);
    }

    #[test]
    fn negative_counts_never_produce_negative_cost() {
        let usage = UsageMetadata { cache_read_input_token_count: Some(5_000), ..usage(-10, -10) };
        assert_eq!(price("openai", "gpt-4.1").cost_usd(&usage, today()), 5_000.0 * 0.5 / 1e6);
    }

    #[cfg(feature = "gemini")]
    #[test]
    fn gemini_rates_match_adk_gemini_pricing() {
        for price in PricingCatalog::standard().prices() {
            if price.vendor != PriceVendor::Google {
                continue;
            }
            let Some(vendor) = adk_gemini::pricing::GeminiPricing::for_model_id(&price.model)
            else {
                continue;
            };
            assert_eq!(price.rates.input, vendor.input, "{}", price.model);
            assert_eq!(price.rates.output, vendor.output, "{}", price.model);
            if vendor.cache_input > 0.0 {
                assert_eq!(price.rates.cache_read, vendor.cache_input, "{}", price.model);
            }
            match price.long_context {
                Some(tier) => assert_eq!(tier.rates.input, vendor.input_long, "{}", price.model),
                None => assert_eq!(vendor.input_long, vendor.input, "{}", price.model),
            }
        }
    }

    #[cfg(feature = "openai")]
    #[test]
    fn openai_rates_match_openai_pricing() {
        for price in PricingCatalog::standard().prices() {
            if price.vendor != PriceVendor::OpenAI {
                continue;
            }
            let vendor = crate::openai::pricing::lookup_pricing(&price.model)
                .unwrap_or_else(|| panic!("{} is missing from openai::pricing", price.model));
            assert_eq!(price.rates.input, vendor.input, "{}", price.model);
            assert_eq!(price.rates.output, vendor.output, "{}", price.model);
            assert_eq!(price.rates.cache_read, vendor.cached_input, "{}", price.model);
        }
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn anthropic_rates_match_adk_anthropic_pricing() {
        for price in PricingCatalog::standard().prices() {
            if price.vendor != PriceVendor::Anthropic {
                continue;
            }
            let vendor = adk_anthropic::pricing::ModelPricing::for_model_id(&price.model)
                .unwrap_or_else(|| {
                    panic!("{} is missing from adk_anthropic::pricing", price.model)
                });
            assert_eq!(price.rates.input, vendor.input, "{}", price.model);
            assert_eq!(price.rates.output, vendor.output, "{}", price.model);
            assert_eq!(price.rates.cache_read, vendor.cache_read, "{}", price.model);
            assert_eq!(price.rates.cache_write_5m, vendor.cache_write_5m, "{}", price.model);
            assert_eq!(price.rates.cache_write_1h, vendor.cache_write_1h, "{}", price.model);
        }
    }
}
