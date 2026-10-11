//! Token prices for every provider and cost calculation from [`UsageMetadata`](adk_core::UsageMetadata).
//!
//! [`PricingCatalog`] is the single table `adk-model` uses to fill
//! [`UsageMetadata::cost`](adk_core::UsageMetadata::cost) on provider responses that do not report a cost
//! themselves. It consolidates the vendor tables in `adk_gemini::pricing`,
//! `adk_model::openai::pricing` and `adk_anthropic::pricing` (tests keep the
//! rates in step) and adds DeepSeek, Groq, xAI and Mistral list prices. Every
//! rate is USD per one million tokens.
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
//! `bedrock`, `azure-ai` and `openrouter`; DeepSeek rates to `deepseek`; Groq
//! rates to `groq`; xAI rates to `xai` and `openrouter`; Mistral rates to
//! `mistral` and `openrouter`. `ollama` runs locally and costs zero, except
//! `*cloud*` models.
//!
//! # Limitations
//!
//! - Standard tier only: batch, flex, priority, fast mode, data-residency and
//!   regional uplifts are not applied.
//! - Image, audio and realtime output rates are not modelled; models whose
//!   output is billed by modality are left out, so they resolve to `None`.
//! - GPT-5.6 long-context rates are not applied: the threshold is unpublished.
//! - Cache storage (Gemini per-hour storage) is not a per-request cost.
//! - Built-in tool fees ([`BuiltinToolFee`](crate::pricing::BuiltinToolFee))
//!   apply on the vendor's own API only, ignore free allowances, and leave
//!   container-billed tools unpriced.
//! - DeepSeek's off-peak discount ([`OffPeak`](crate::pricing::OffPeak)) applies outside the published
//!   UTC peak hours on weekdays; Chinese public holidays, which are off-peak in
//!   full, are billed at the peak rate, which over-estimates.
//! - Models a vendor prices only on request ("Contact Sales") or at a
//!   temporary sale price are left out: Groq's Llama and MiniMax models, and
//!   Mistral Large 4.
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

use adk_core::{LlmRequest, Part, UsageMetadata};
use chrono::NaiveDate;
use std::sync::LazyLock;

/// Date the standard catalog's rates were last verified against the vendors'
/// published price lists.
pub const PRICING_EFFECTIVE_DATE: &str = "2026-10-11";

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
    /// Groq, which hosts open-weight models at its own prices.
    Groq,
    /// xAI Grok.
    XAi,
    /// Mistral AI.
    Mistral,
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
            Self::Groq => provider == "groq",
            Self::XAi => matches!(provider, "xai" | "openrouter"),
            Self::Mistral => matches!(provider, "mistral" | "openrouter"),
        }
    }
}

impl PriceVendor {
    /// Returns `true` when `provider` is the vendor's own API, the only place its
    /// published built-in tool fees apply.
    pub fn first_party(self, provider: &str) -> bool {
        match self {
            Self::Google => provider == "gemini",
            Self::OpenAI => matches!(provider, "openai" | "openai-responses"),
            Self::Anthropic => provider == "anthropic",
            Self::DeepSeek => provider == "deepseek",
            Self::Groq => provider == "groq",
            Self::XAi => provider == "xai",
            Self::Mistral => provider == "mistral",
        }
    }
}

/// How a [`BuiltinToolFee`] is charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeUnit {
    /// Once per reported use, such as one search query.
    PerUse,
    /// Once per response that used the tool at all, such as one grounded prompt.
    PerResponse,
}

/// A vendor's fee for a provider-executed built-in tool, on top of token costs.
///
/// `kind` is a [`BuiltinToolUse::kind`](adk_core::BuiltinToolUse::kind). The fee
/// applies to every model of the vendor unless restricted with
/// [`for_models`](Self::for_models).
///
/// # Example
///
/// ```
/// use adk_model::pricing::{BuiltinToolFee, PriceVendor};
///
/// let fee = BuiltinToolFee::per_use(PriceVendor::Google, "google_search", 0.014)
///     .for_models(["gemini-3*"]);
/// assert!(fee.applies_to("gemini-3.7-flash"));
/// assert!(!fee.applies_to("gemini-2.5-flash"));
/// assert!((fee.cost_usd(3) - 0.042).abs() < 1e-12);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinToolFee {
    /// Vendor whose published fee this is.
    pub vendor: PriceVendor,
    /// Built-in tool kind the fee applies to.
    pub kind: String,
    /// Model identifiers the fee applies to; empty means every model. An entry
    /// ending in `*` is a prefix; any other entry matches exactly or as a dated alias.
    pub models: Vec<String>,
    /// Fee in USD per [`unit`](Self::unit).
    pub usd: f64,
    /// What one fee is charged for.
    pub unit: FeeUnit,
}

impl BuiltinToolFee {
    /// A fee of `usd` per use of `kind`.
    pub fn per_use(vendor: PriceVendor, kind: impl Into<String>, usd: f64) -> Self {
        Self { vendor, kind: kind.into(), models: Vec::new(), usd, unit: FeeUnit::PerUse }
    }

    /// A fee of `usd` per response that used `kind` at least once.
    pub fn per_response(vendor: PriceVendor, kind: impl Into<String>, usd: f64) -> Self {
        Self { unit: FeeUnit::PerResponse, ..Self::per_use(vendor, kind, usd) }
    }

    /// Restricts the fee to `models`.
    pub fn for_models(mut self, models: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.models = models.into_iter().map(Into::into).collect();
        self
    }

    /// Whether the fee applies to `model`.
    pub fn applies_to(&self, model: &str) -> bool {
        if self.models.is_empty() {
            return true;
        }
        let canonical = canonical_model_id(model);
        self.models.iter().any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => canonical.starts_with(prefix),
            None => {
                canonical == *pattern
                    || canonical.strip_prefix(pattern.as_str()).is_some_and(is_version_suffix)
            }
        })
    }

    /// Fee in USD for `count` uses in one response.
    pub fn cost_usd(&self, count: u32) -> f64 {
        match self.unit {
            FeeUnit::PerUse => f64::from(count) * self.usd,
            FeeUnit::PerResponse if count > 0 => self.usd,
            FeeUnit::PerResponse => 0.0,
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
    /// Largest output, reasoning included, the model produces for one request; used to
    /// bound a request that sets no output cap.
    pub max_output_tokens: Option<u64>,
    /// Discounted rates outside the vendor's peak hours.
    pub off_peak: Option<OffPeak>,
}

/// A time-of-day discount: outside the peak windows every rate is multiplied by `factor`.
///
/// # Example
///
/// ```
/// use adk_model::pricing::OffPeak;
/// use chrono::{TimeZone, Utc};
///
/// // DeepSeek: peak 01:00–04:00 and 06:00–10:00 UTC on weekdays, half price otherwise.
/// let off_peak = OffPeak::weekdays(0.5, &[(1, 4), (6, 10)]);
/// assert!(off_peak.is_peak(Utc.with_ymd_and_hms(2026, 10, 12, 7, 30, 0).unwrap()));
/// assert!(!off_peak.is_peak(Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap()));
/// assert!(!off_peak.is_peak(Utc.with_ymd_and_hms(2026, 10, 11, 7, 30, 0).unwrap()));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct OffPeak {
    /// Multiplier applied to every rate off peak.
    pub factor: f64,
    /// Peak windows as `[start, end)` UTC hours, Monday through Friday.
    pub peak_hours_utc: Vec<(u32, u32)>,
}

impl OffPeak {
    /// Peak during `peak_hours_utc` on weekdays; `factor` applies at all other times.
    pub fn weekdays(factor: f64, peak_hours_utc: &[(u32, u32)]) -> Self {
        Self { factor, peak_hours_utc: peak_hours_utc.to_vec() }
    }

    /// Whether `at` falls in a peak window.
    pub fn is_peak(&self, at: chrono::DateTime<chrono::Utc>) -> bool {
        use chrono::{Datelike, Timelike, Weekday};
        if matches!(at.weekday(), Weekday::Sat | Weekday::Sun) {
            return false;
        }
        let hour = at.hour();
        self.peak_hours_utc.iter().any(|(start, end)| hour >= *start && hour < *end)
    }
}

impl ModelPrice {
    /// Creates a price with only base rates.
    pub fn new(vendor: PriceVendor, model: impl Into<String>, rates: ModelRates) -> Self {
        Self {
            vendor,
            model: model.into(),
            rates,
            long_context: None,
            change: None,
            max_output_tokens: None,
            off_peak: None,
        }
    }

    /// Discounts every rate outside the peak windows of `off_peak`.
    pub fn with_off_peak(mut self, off_peak: OffPeak) -> Self {
        self.off_peak = Some(off_peak);
        self
    }

    /// Sets the model's maximum output tokens per request.
    pub fn with_max_output_tokens(mut self, tokens: u64) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Upper bound in USD on a request of at most `prompt_tokens` input tokens and
    /// `output_tokens` output tokens, priced on `date`.
    ///
    /// Every input token is priced at the highest input-side rate (uncached, cache read
    /// or cache write) of the tier the prompt bound falls in, so cache writes and
    /// long-context switches never push the actual cost above the bound.
    pub fn max_cost_usd(&self, prompt_tokens: u64, output_tokens: u64, date: NaiveDate) -> f64 {
        let rates = self.rates_for(prompt_tokens, date);
        let input =
            rates.input.max(rates.cache_read).max(rates.cache_write_5m).max(rates.cache_write_1h);
        (prompt_tokens as f64 * input + output_tokens as f64 * rates.output) / 1_000_000.0
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
        self.cost_with_rates(usage, self.rates_for(non_negative(usage.prompt_token_count), date))
    }

    /// Cost in USD of one response's usage at the instant `at`, with any off-peak
    /// discount applied.
    pub fn cost_usd_at(&self, usage: &UsageMetadata, at: chrono::DateTime<chrono::Utc>) -> f64 {
        let mut rates = self.rates_for(non_negative(usage.prompt_token_count), at.date_naive());
        if let Some(off_peak) = &self.off_peak
            && !off_peak.is_peak(at)
        {
            rates = ModelRates {
                input: rates.input * off_peak.factor,
                output: rates.output * off_peak.factor,
                cache_read: rates.cache_read * off_peak.factor,
                cache_write_5m: rates.cache_write_5m * off_peak.factor,
                cache_write_1h: rates.cache_write_1h * off_peak.factor,
            };
        }
        self.cost_with_rates(usage, rates)
    }

    fn cost_with_rates(&self, usage: &UsageMetadata, rates: ModelRates) -> f64 {
        let prompt = non_negative(usage.prompt_token_count);
        let cache_read = non_negative(usage.cache_read_input_token_count.unwrap_or(0));
        let cache_write = non_negative(usage.cache_creation_input_token_count.unwrap_or(0));
        let write_1h = one_hour_cache_writes(usage).min(cache_write);
        let write_5m = cache_write - write_1h;
        let uncached = prompt.saturating_sub(cache_read.saturating_add(cache_write));
        let output = non_negative(usage.candidates_token_count);
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
    fees: Vec<BuiltinToolFee>,
}

impl PricingCatalog {
    /// Creates an empty catalog labelled `version`.
    pub fn new(version: impl Into<String>) -> Self {
        Self { version: version.into(), prices: Vec::new(), fees: Vec::new() }
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

    /// Adds a built-in tool fee. Fees are matched in the order they were added.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::{BuiltinToolUse, UsageMetadata};
    /// use adk_model::pricing::{
    ///     BuiltinToolFee, ModelPrice, ModelRates, PriceVendor, PricingCatalog,
    /// };
    ///
    /// let catalog = PricingCatalog::new("internal")
    ///     .with_price(ModelPrice::new(PriceVendor::Anthropic, "my-claude", ModelRates::new(1.0, 5.0)))
    ///     .with_builtin_tool_fee(BuiltinToolFee::per_use(PriceVendor::Anthropic, "web_search", 0.01));
    /// let usage = UsageMetadata {
    ///     builtin_tool_uses: vec![BuiltinToolUse::new("web_search", 2)],
    ///     ..Default::default()
    /// };
    /// assert_eq!(catalog.cost_usd(Some("anthropic"), "my-claude", &usage), Some(0.02));
    /// ```
    pub fn with_builtin_tool_fee(mut self, fee: BuiltinToolFee) -> Self {
        self.fees.push(fee);
        self
    }

    /// Every built-in tool fee in the catalog.
    pub fn builtin_tool_fees(&self) -> &[BuiltinToolFee] {
        &self.fees
    }

    /// Resolves the fee for `kind` on `model` as served by `provider`.
    ///
    /// Fees apply only on the vendor's own API ([`PriceVendor::first_party`]);
    /// `None` for `provider` matches any vendor.
    pub fn builtin_tool_fee(
        &self,
        provider: Option<&str>,
        model: &str,
        kind: &str,
    ) -> Option<&BuiltinToolFee> {
        let vendor = self.lookup(provider, model).map(|price| price.vendor);
        self.fees.iter().find(|fee| {
            fee.kind == kind
                && vendor.is_none_or(|vendor| vendor == fee.vendor)
                && provider.is_none_or(|provider| fee.vendor.first_party(provider))
                && fee.applies_to(model)
        })
    }

    /// Fees in USD for the built-in tool uses in `usage`. Unpriced kinds add nothing.
    pub fn builtin_tool_cost_usd(
        &self,
        provider: Option<&str>,
        model: &str,
        usage: &UsageMetadata,
    ) -> f64 {
        usage
            .builtin_tool_uses
            .iter()
            .filter_map(|used| match self.builtin_tool_fee(provider, model, &used.kind) {
                Some(fee) => Some(fee.cost_usd(used.count)),
                None => {
                    tracing::debug!(
                        builtin_tool.kind = %used.kind,
                        gen_ai.request.model = %model,
                        "no published fee for built-in tool; cost covers tokens only"
                    );
                    None
                }
            })
            .sum()
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

    /// Upper bound in micro-USD on what `request` can cost on `model` as served by
    /// `provider`, or `None` when it cannot be bounded.
    ///
    /// `max_output_tokens` is the output cap the client sends with the request; `None`
    /// uses the model's maximum output ([`ModelPrice::max_output_tokens`]). Built-in tool
    /// fees are included: per-response fees once each, per-use fees up to each declared
    /// built-in tool's `max_uses`. The bound is `None` for an unpriced model, a model with
    /// neither output cap, a request whose input size is unknown
    /// ([`request_input_token_bound`]), or a request declaring a fee-bearing built-in tool
    /// without `max_uses`. Local Ollama models bound at zero.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::{Content, LlmRequest};
    /// use adk_model::pricing::PricingCatalog;
    ///
    /// let request = LlmRequest::new("gpt-4.1", vec![Content::new("user").with_text("hi")]);
    /// let catalog = PricingCatalog::standard();
    /// let bound = catalog.max_cost_micro_usd(Some("openai"), "gpt-4.1", &request, Some(1_000));
    /// // (2,048 + 16 + 2) input tokens × $2.00 + 1,000 output tokens × $8.00, rounded up.
    /// assert_eq!(bound, Some(12_133));
    /// assert_eq!(catalog.max_cost_micro_usd(Some("openai"), "my-fine-tune", &request, Some(1_000)), None);
    /// ```
    pub fn max_cost_micro_usd(
        &self,
        provider: Option<&str>,
        model: &str,
        request: &LlmRequest,
        max_output_tokens: Option<u64>,
    ) -> Option<u64> {
        if provider == Some("ollama") {
            return (!model.contains("cloud")).then_some(0);
        }
        let price = self.lookup(provider, model)?;
        let output = max_output_tokens.or(price.max_output_tokens)?;
        let prompt = request_input_token_bound(request)?;
        let today = chrono::Utc::now().date_naive();
        // A call that starts before a scheduled rate change can be priced after it.
        let tomorrow = today.succ_opt().unwrap_or(today);
        let usd = price
            .max_cost_usd(prompt, output, today)
            .max(price.max_cost_usd(prompt, output, tomorrow))
            + self.builtin_tool_fee_bound_usd(provider, model, price.vendor, request)?;
        // One extra micro-USD absorbs floating-point rounding in the commit.
        adk_core::usd_to_micro_usd(usd).map(|micro| micro.saturating_add(1))
    }

    /// Upper bound in USD on the built-in tool fees one response to `request` can incur.
    ///
    /// Per-response fees count once each. Per-use fees are bounded only when every
    /// built-in tool the request declares caps its uses (`max_uses`); otherwise the
    /// fees, and so the request, cannot be bounded.
    fn builtin_tool_fee_bound_usd(
        &self,
        provider: Option<&str>,
        model: &str,
        vendor: PriceVendor,
        request: &LlmRequest,
    ) -> Option<f64> {
        let declared: Vec<&serde_json::Value> =
            request.tools.values().filter_map(adk_core::builtin_tool_configuration).collect();
        if declared.is_empty() {
            return Some(0.0);
        }
        let applicable = self.fees.iter().filter(|fee| {
            fee.usd > 0.0
                && fee.vendor == vendor
                && provider.is_none_or(|provider| fee.vendor.first_party(provider))
                && fee.applies_to(model)
        });
        let mut per_response = 0.0;
        let mut per_use: Option<f64> = None;
        for fee in applicable {
            match fee.unit {
                FeeUnit::PerResponse => per_response += fee.usd,
                FeeUnit::PerUse => per_use = Some(per_use.map_or(fee.usd, |max| max.max(fee.usd))),
            }
        }
        let Some(per_use) = per_use else {
            return Some(per_response);
        };
        let mut uses = 0u64;
        for configuration in declared {
            uses = uses.saturating_add(configuration.get("max_uses")?.as_u64()?);
        }
        Some(per_response + uses as f64 * per_use)
    }

    /// Cost in USD of one response's usage, or `None` when the model is unpriced.
    ///
    /// The cost is the token cost plus the fees for the response's
    /// [`builtin_tool_uses`](UsageMetadata::builtin_tool_uses) that the catalog
    /// prices. Local Ollama models cost zero.
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
        Some(
            price.cost_usd_at(usage, chrono::Utc::now())
                + self.builtin_tool_cost_usd(provider, model, usage),
        )
    }
}

/// Tokens counted for one inline image, audio or video part when its byte size is smaller.
///
/// Providers bill media by dimensions or duration rather than bytes, so a small but
/// highly compressed file can cost more tokens than it has bytes.
pub const MEDIA_PART_MIN_TOKENS: u64 = 16_384;

/// Fixed input tokens added to every request bound for provider-side framing: role
/// markers, tool-use system prompts and response-format instructions.
pub const REQUEST_OVERHEAD_TOKENS: u64 = 2_048;

/// Upper bound on the input tokens of `request`, or `None` when the request references
/// content whose size is unknown (a `FileData` URI).
///
/// Every tokenizer `adk-model` providers use emits at most one token per UTF-8 byte, so
/// the byte length of each text and JSON part bounds its tokens. Inline media counts its
/// byte length, at least [`MEDIA_PART_MIN_TOKENS`]; [`REQUEST_OVERHEAD_TOKENS`] plus 16
/// tokens per message and per tool cover the framing a provider adds.
///
/// # Example
///
/// ```
/// use adk_core::{Content, LlmRequest};
/// use adk_model::pricing::{REQUEST_OVERHEAD_TOKENS, request_input_token_bound};
///
/// let request = LlmRequest::new("gpt-5.4-mini", vec![Content::new("user").with_text("hello")]);
/// assert_eq!(request_input_token_bound(&request), Some(REQUEST_OVERHEAD_TOKENS + 16 + 5));
/// ```
pub fn request_input_token_bound(request: &LlmRequest) -> Option<u64> {
    let json_len = |value: &dyn erased::Json| value.byte_len();
    let mut tokens = REQUEST_OVERHEAD_TOKENS;
    for content in &request.contents {
        tokens = tokens.saturating_add(16);
        for part in &content.parts {
            let part_tokens = match part {
                Part::Text { text } => text.len() as u64,
                Part::Thinking { thinking, signature } => {
                    (thinking.len() + signature.as_ref().map_or(0, String::len)) as u64
                }
                Part::InlineData { data, .. } => (data.len() as u64).max(MEDIA_PART_MIN_TOKENS),
                Part::FileData { .. } => return None,
                _ => json_len(part),
            };
            tokens = tokens.saturating_add(part_tokens);
        }
    }
    for declaration in request.tools.values() {
        tokens = tokens.saturating_add(16).saturating_add(json_len(declaration));
    }
    if let Some(config) = &request.config {
        if let Some(schema) = &config.response_schema {
            tokens = tokens.saturating_add(json_len(schema));
        }
        tokens = tokens.saturating_add(config.stop_sequences.iter().map(|s| s.len() as u64).sum());
    }
    Some(tokens)
}

/// The output cap a request sets, if any.
#[cfg(any(
    feature = "anthropic",
    feature = "azure-ai",
    feature = "bedrock",
    feature = "gemini",
    feature = "ollama",
    feature = "openai"
))]
pub(crate) fn request_output_cap(request: &LlmRequest) -> Option<u64> {
    request
        .config
        .as_ref()
        .and_then(|config| config.max_output_tokens)
        .and_then(|tokens| u64::try_from(tokens).ok())
}

mod erased {
    /// Serialized JSON length of a value, for token bounds.
    pub trait Json {
        fn byte_len(&self) -> u64;
    }

    impl<T: serde::Serialize> Json for T {
        fn byte_len(&self) -> u64 {
            serde_json::to_vec(self).map_or(u64::MAX / 4, |bytes| bytes.len() as u64)
        }
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
    use PriceVendor::{Anthropic, DeepSeek, Google, Groq, Mistral, OpenAI, XAi};

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
    // xAI bills every token of a request at the long-context rate once the prompt
    // reaches 200K tokens.
    const XAI_LONG: u64 = 200_000;
    let cached = |vendor: PriceVendor, model: &str, input: f64, output: f64, cache_read: f64| {
        ModelPrice::new(vendor, model, ModelRates::new(input, output).with_cache_read(cache_read))
    };
    let deepseek_off_peak = OffPeak::weekdays(0.5, &[(1, 4), (6, 10)]);
    let grok = |model: &str, base: (f64, f64, f64), long: (f64, f64, f64)| {
        cached(XAi, model, base.0, base.2, base.1)
            .with_long_context(XAI_LONG, ModelRates::new(long.0, long.2).with_cache_read(long.1))
    };
    // (input, cached input, output) per million tokens.
    let grok_47 = ((2.00, 0.50, 6.00), (4.00, 1.00, 12.00));
    let grok_45 = ((2.00, 0.30, 6.00), (4.00, 0.60, 12.00));
    let grok_420 = ((1.25, 0.20, 2.50), (2.50, 0.40, 5.00));
    let grok_build = ((1.00, 0.20, 2.00), (2.00, 0.40, 4.00));
    // Gemini 3.7 and 3.6 Flash introductory rates end on 2026-12-31.
    let flash_2027 = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap_or(NaiveDate::MAX);
    let flash_2027_rates = ModelRates::new(1.50, 7.50).with_cache_read(0.15);

    let prices = vec![
        // ── Google Gemini ──
        // https://ai.google.dev/gemini-api/docs/pricing, retrieved 2026-10-11: Gemini 3.8
        // Flash at the 3.6 Flash rates. `gemini-3.7-flash` is not listed there; requests for
        // it are routed to and reported as `gemini-3.8-flash`.
        gemini("gemini-3.8-flash", 0.75, 3.75, 0.075).with_change(flash_2027, flash_2027_rates),
        gemini("gemini-3.7-flash", 0.75, 3.75, 0.075).with_change(flash_2027, flash_2027_rates),
        gemini("gemini-3.6-flash", 0.75, 3.75, 0.075).with_change(flash_2027, flash_2027_rates),
        gemini("gemini-3.5-flash", 1.50, 9.00, 0.15),
        gemini("gemini-3.5-flash-lite", 0.30, 2.50, 0.03),
        gemini("gemini-3.1-pro-preview-customtools", 2.00, 12.00, 0.20)
            .with_long_context(GEMINI_LONG, ModelRates::new(4.00, 18.00).with_cache_read(0.40)),
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
        // ── DeepSeek ──
        // Source: https://api-docs.deepseek.com/quick_start/pricing, retrieved 2026-10-11.
        // Peak rates (cache miss, cache hit, output); "Peak hours are 01:00 - 04:00 and
        // 06:00 - 10:00 UTC, Monday through Friday" and "Off-peak rates are half of the
        // peak rates". The legacy `deepseek-v4-flash` is "billed at the Flash price";
        // `deepseek-chat` and `deepseek-reasoner` are no longer listed and stay unpriced.
        cached(DeepSeek, "deepseek-flash", 0.30, 1.20, 0.006)
            .with_off_peak(deepseek_off_peak.clone()),
        cached(DeepSeek, "deepseek-v4-flash", 0.30, 1.20, 0.006)
            .with_off_peak(deepseek_off_peak.clone()),
        cached(DeepSeek, "deepseek-v4-pro", 1.32, 3.96, 0.044).with_off_peak(deepseek_off_peak),
        // ── Groq ──
        // Source: https://console.groq.com/docs/models and the per-model pages under
        // https://console.groq.com/docs/model/, retrieved 2026-10-11. Cached-input rates
        // are listed for the GPT-OSS models; https://console.groq.com/docs/prompt-caching
        // states a 50% cached-input discount for gpt-oss-safeguard-20b.
        cached(Groq, "gpt-oss-120b", 0.15, 0.60, 0.075),
        cached(Groq, "gpt-oss-20b", 0.075, 0.30, 0.037),
        cached(Groq, "gpt-oss-safeguard-20b", 0.075, 0.30, 0.0375),
        ModelPrice::new(Groq, "qwen3.8-27b", ModelRates::new(0.80, 4.00)),
        // ── xAI ──
        // Source: https://docs.x.ai/docs/models, retrieved 2026-10-11. Aliases are the ones
        // the page lists for each model; `-latest` and dated suffixes resolve automatically.
        grok("grok-4.7", grok_47.0, grok_47.1),
        grok("grok-4.6", grok_47.0, grok_47.1),
        grok("grok-4.5", grok_45.0, grok_45.1),
        grok("grok-build-latest", grok_45.0, grok_45.1),
        grok("grok-4.3", grok_420.0, grok_420.1),
        grok("grok-4.20", grok_420.0, grok_420.1),
        grok("grok-4.20-0309-reasoning", grok_420.0, grok_420.1),
        grok("grok-4.20-reasoning", grok_420.0, grok_420.1),
        grok("grok-4.20-0309-non-reasoning", grok_420.0, grok_420.1),
        grok("grok-4.20-non-reasoning", grok_420.0, grok_420.1),
        grok("grok-4.20-multi-agent-0309", grok_420.0, grok_420.1),
        grok("grok-4.20-multi-agent", grok_420.0, grok_420.1),
        grok("grok-build-0.1", grok_build.0, grok_build.1),
        grok("grok-code-fast-1", grok_build.0, grok_build.1),
        grok("grok-code-fast", grok_build.0, grok_build.1),
        // ── Mistral ──
        // Source: https://docs.mistral.ai/inference/pricing, with API ids from the model
        // pages under https://docs.mistral.ai/models/, retrieved 2026-10-11. Only the
        // `-latest` aliases the pages list are priced, so a retired dated id never
        // resolves to a current model's rate.
        cached(Mistral, "mistral-medium-3-5", 1.5, 7.5, 0.15),
        cached(Mistral, "mistral-medium-3", 1.5, 7.5, 0.15),
        cached(Mistral, "mistral-medium-latest", 1.5, 7.5, 0.15),
        cached(Mistral, "mistral-large-2512", 0.5, 1.5, 0.05),
        cached(Mistral, "mistral-large-latest", 0.5, 1.5, 0.05),
        cached(Mistral, "mistral-small-2603", 0.15, 0.6, 0.015),
        cached(Mistral, "mistral-small-latest", 0.15, 0.6, 0.015),
        cached(Mistral, "ministral-14b-2512", 0.2, 0.2, 0.02),
        cached(Mistral, "ministral-14b-latest", 0.2, 0.2, 0.02),
        cached(Mistral, "ministral-8b-2512", 0.15, 0.15, 0.015),
        cached(Mistral, "ministral-8b-latest", 0.15, 0.15, 0.015),
        cached(Mistral, "ministral-3b-2512", 0.1, 0.1, 0.01),
        cached(Mistral, "ministral-3b-latest", 0.1, 0.1, 0.01),
        cached(Mistral, "codestral-2508", 0.3, 0.9, 0.03),
        cached(Mistral, "codestral-latest", 0.3, 0.9, 0.03),
    ];

    // Maximum output tokens per request, reasoning included, used to bound requests that
    // set no output cap. Sources, retrieved 2026-10-11:
    // - Gemini: https://ai.google.dev/gemini-api/docs/models/<id> ("Output token limit");
    //   https://ai.google.dev/gemini-api/docs/thinking counts thought tokens in the limit.
    // - OpenAI: https://developers.openai.com/api/docs/models/<id> ("max output tokens");
    //   https://developers.openai.com/api/docs/guides/reasoning counts reasoning tokens.
    // - DeepSeek: https://api-docs.deepseek.com/quick_start/pricing ("MAXIMUM: 384K").
    // - Groq: https://console.groq.com/docs/models ("Max Completion Tokens").
    // - Mistral: https://docs.mistral.ai/models/<slug> states only a context window, which
    //   Mistral defines as input plus output tokens, so the window bounds the output.
    // xAI publishes no output limit, so Grok requests bound only with an explicit cap.
    const GEMINI_MAX_OUTPUT: u64 = 65_536;
    let max_output: &[(&str, u64)] = &[
        ("gemini-3.8-flash", GEMINI_MAX_OUTPUT),
        ("gemini-3.7-flash", GEMINI_MAX_OUTPUT),
        ("gemini-3.6-flash", GEMINI_MAX_OUTPUT),
        ("gemini-3.5-flash", GEMINI_MAX_OUTPUT),
        ("gemini-3.5-flash-lite", GEMINI_MAX_OUTPUT),
        ("gemini-3.1-pro-preview", GEMINI_MAX_OUTPUT),
        ("gemini-3.1-flash-lite", GEMINI_MAX_OUTPUT),
        ("gemini-3-flash-preview", GEMINI_MAX_OUTPUT),
        ("gemini-2.5-pro", GEMINI_MAX_OUTPUT),
        ("gemini-2.5-flash", GEMINI_MAX_OUTPUT),
        ("gemini-2.5-flash-lite", GEMINI_MAX_OUTPUT),
        ("gpt-5.6", 128_000),
        ("gpt-5.6-sol", 128_000),
        ("gpt-5.6-terra", 128_000),
        ("gpt-5.6-luna", 128_000),
        ("gpt-5.6-cyber", 128_000),
        ("gpt-5.5", 128_000),
        ("gpt-5.5-pro", 128_000),
        ("gpt-5.4", 128_000),
        ("gpt-5.4-pro", 128_000),
        ("gpt-5.4-mini", 128_000),
        ("gpt-5.4-nano", 128_000),
        ("gpt-5.3-codex", 128_000),
        ("gpt-5.2", 128_000),
        ("gpt-5.2-pro", 128_000),
        ("gpt-5.1", 128_000),
        ("gpt-5", 128_000),
        ("gpt-5-mini", 128_000),
        ("gpt-5-nano", 128_000),
        ("gpt-5-pro", 272_000),
        ("gpt-4.1", 32_768),
        ("gpt-4.1-mini", 32_768),
        ("gpt-4.1-nano", 32_768),
        ("o3", 100_000),
        ("o4-mini", 100_000),
        ("o3-mini", 100_000),
        ("o1", 100_000),
        ("gpt-4o", 16_384),
        ("gpt-4o-mini", 16_384),
        ("deepseek-flash", 393_216),
        ("deepseek-v4-flash", 393_216),
        ("deepseek-v4-pro", 393_216),
        ("gpt-oss-120b", 65_536),
        ("gpt-oss-20b", 65_536),
        ("gpt-oss-safeguard-20b", 65_536),
        ("qwen3.8-27b", 16_384),
        ("mistral-medium-3-5", 262_144),
        ("mistral-medium-3", 262_144),
        ("mistral-medium-latest", 262_144),
        ("mistral-large-2512", 262_144),
        ("mistral-large-latest", 262_144),
        ("mistral-small-2603", 262_144),
        ("mistral-small-latest", 262_144),
        ("ministral-14b-2512", 262_144),
        ("ministral-14b-latest", 262_144),
        ("ministral-8b-2512", 262_144),
        ("ministral-8b-latest", 262_144),
        ("ministral-3b-2512", 262_144),
        ("ministral-3b-latest", 262_144),
        ("codestral-2508", 131_072),
        ("codestral-latest", 131_072),
    ];
    let prices = prices
        .into_iter()
        .map(|price| match max_output.iter().find(|(model, _)| *model == price.model) {
            Some((_, tokens)) => price.with_max_output_tokens(*tokens),
            None => price,
        })
        .collect();
    PricingCatalog { version: PRICING_EFFECTIVE_DATE.to_string(), prices, fees: standard_fees() }
}

/// Published built-in tool fees, verified on 2026-10-11.
///
/// Free monthly or daily allowances are not tracked, so a run inside an allowance
/// is over-estimated. Kinds without a per-use fee here (Anthropic and OpenAI code
/// execution, billed per container; OpenAI image generation and MCP; Gemini 3 Maps,
/// billed per Maps query, which responses do not report) add no fee.
fn standard_fees() -> Vec<BuiltinToolFee> {
    use PriceVendor::{Anthropic, Google, OpenAI};
    vec![
        // https://platform.claude.com/docs/en/about-claude/pricing (fetched 2026-10-11):
        // web search "$10 per 1,000 searches"; web fetch "no additional cost".
        BuiltinToolFee::per_use(Anthropic, "web_search", 0.010),
        BuiltinToolFee::per_use(Anthropic, "web_fetch", 0.0),
        // https://developers.openai.com/api/docs/pricing (fetched 2026-10-11):
        // web search "$10.00 / 1k calls"; file search tool call "$2.50 / 1k calls".
        // `web_search_preview` on non-reasoning models costs $25.00 / 1k calls and is
        // under-estimated, because the response does not name the tool version.
        BuiltinToolFee::per_use(OpenAI, "web_search", 0.010),
        BuiltinToolFee::per_use(OpenAI, "file_search", 0.0025),
        // https://ai.google.dev/gemini-api/docs/pricing (fetched 2026-10-11):
        // Gemini 3 and newer, Google Search "$14 per 1,000 requests" (search queries).
        BuiltinToolFee::per_use(Google, "google_search", 0.014).for_models(["gemini-3*"]),
        // Gemini 2.5 Pro and Flash, Google Search "$35 / 1,000 grounded prompts" and
        // Google Maps "$25 / 1,000 grounded prompts".
        BuiltinToolFee::per_response(Google, "google_search", 0.035)
            .for_models(["gemini-2.5-pro", "gemini-2.5-flash"]),
        BuiltinToolFee::per_response(Google, "google_maps", 0.025)
            .for_models(["gemini-2.5-pro", "gemini-2.5-flash"]),
        // "Code execution is billed at the standard token rates for the selected model."
        BuiltinToolFee::per_use(Google, "code_execution", 0.0),
    ]
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

    fn tool_usage(uses: &[(&str, u32)]) -> UsageMetadata {
        UsageMetadata {
            builtin_tool_uses: uses
                .iter()
                .map(|(kind, count)| adk_core::BuiltinToolUse::new(*kind, *count))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn builtin_tool_fees_are_added_on_the_vendor_api_only() {
        let catalog = PricingCatalog::standard();
        let searched = tool_usage(&[("web_search", 3), ("web_fetch", 2), ("code_execution", 1)]);
        let cost = |provider: &str, model: &str, usage: &UsageMetadata| {
            catalog.cost_usd(Some(provider), model, usage).unwrap()
        };
        assert!((cost("anthropic", "claude-sonnet-4-6", &searched) - 0.03).abs() < 1e-12);
        assert_eq!(cost("bedrock", "claude-sonnet-4-6", &searched), 0.0);
        assert!(
            (cost("openai", "gpt-5.6-luna", &tool_usage(&[("web_search", 2), ("file_search", 4)]))
                - 0.03)
                .abs()
                < 1e-12
        );

        let gemini3 =
            tool_usage(&[("google_search", 4), ("code_execution", 2), ("google_maps", 1)]);
        assert!((cost("gemini", "gemini-3.7-flash", &gemini3) - 0.056).abs() < 1e-12);
        let gemini25 = tool_usage(&[("google_search", 4), ("google_maps", 1)]);
        assert!((cost("gemini", "gemini-2.5-flash", &gemini25) - 0.06).abs() < 1e-12);
        assert!(
            catalog
                .builtin_tool_fee(Some("gemini"), "gemini-2.5-flash-lite", "google_search")
                .is_none()
        );
        assert!(
            catalog
                .builtin_tool_fee(Some("anthropic"), "claude-haiku-4-5", "code_execution")
                .is_none()
        );
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
            ("groq", "openai/gpt-oss-120b", "gpt-oss-120b"),
            ("groq", "qwen/qwen3.8-27b", "qwen3.8-27b"),
            ("xai", "grok-4.5-latest", "grok-4.5"),
            ("xai", "grok-4.20-0309", "grok-4.20"),
            ("xai", "grok-code-fast-1-0825", "grok-code-fast-1"),
            ("openrouter", "x-ai/grok-4.6", "grok-4.6"),
            ("mistral", "mistral-medium-latest", "mistral-medium-latest"),
            ("mistral", "codestral-2508", "codestral-2508"),
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
            ("groq", "llama-3.3-70b-versatile"),
            ("groq", "minimaxai/minimax-m2.7"),
            ("cerebras", "gpt-oss-120b"),
            ("xai", "grok-5"),
            ("mistral", "mistral-large-4"),
            ("mistral", "mistral-medium-2508"),
            ("together", "gpt-4.1"),
            ("deepseek", "gpt-4.1"),
            ("opencode", "claude-sonnet-5"),
        ] {
            assert!(catalog.lookup(Some(provider), model).is_none(), "{provider}/{model}");
        }
    }

    #[test]
    fn xai_long_context_starts_at_200k_and_bills_the_whole_request() {
        let price = price("xai", "grok-4.6");
        let short = price.cost_usd(&usage(199_999, 1_000), today());
        let long = price.cost_usd(&usage(200_000, 1_000), today());
        assert!((short - (199_999.0 * 2.00 + 1_000.0 * 6.00) / 1e6).abs() < 1e-12, "{short}");
        assert!((long - (200_000.0 * 4.00 + 1_000.0 * 12.00) / 1e6).abs() < 1e-12, "{long}");
    }

    #[test]
    fn groq_cached_input_is_billed_at_the_cached_rate() {
        let usage =
            UsageMetadata { cache_read_input_token_count: Some(500_000), ..usage(1_000_000, 0) };
        let cost = price("groq", "openai/gpt-oss-20b").cost_usd(&usage, today());
        assert!((cost - (500_000.0 * 0.075 + 500_000.0 * 0.037) / 1e6).abs() < 1e-12, "{cost}");
    }

    fn text_request(model: &str, text: &str) -> LlmRequest {
        LlmRequest::new(model, vec![adk_core::Content::new("user").with_text(text)])
    }

    #[test]
    fn the_cost_bound_covers_the_most_expensive_usage_the_request_allows() {
        let catalog = PricingCatalog::standard();
        let request = text_request("claude-sonnet-4-5", &"word ".repeat(10_000));
        let prompt = request_input_token_bound(&request).unwrap();
        let bound = catalog
            .max_cost_micro_usd(Some("anthropic"), "claude-sonnet-4-5", &request, Some(4_096))
            .unwrap();
        // Worst case: every prompt token written to the 1-hour cache, full output.
        let worst = UsageMetadata {
            prompt_token_count: prompt as i32,
            candidates_token_count: 4_096,
            cache_creation_input_token_count: Some(prompt as i32),
            provider_usage: Some(serde_json::json!({ "cache_creation_input_tokens_1h": prompt })),
            ..Default::default()
        };
        let cost = catalog.cost_usd(Some("anthropic"), "claude-sonnet-4-5", &worst).unwrap();
        assert!(adk_core::usd_to_micro_usd(cost).unwrap() <= bound, "{cost} vs {bound}");
    }

    #[test]
    fn a_prompt_bound_past_the_long_context_threshold_uses_long_rates() {
        let catalog = PricingCatalog::standard();
        let request = text_request("gemini-2.5-pro", &"x".repeat(300_000));
        let bound = catalog
            .max_cost_micro_usd(Some("gemini"), "gemini-2.5-pro", &request, Some(0))
            .unwrap();
        let prompt = request_input_token_bound(&request).unwrap() as f64;
        assert_eq!(bound, (prompt * 2.50).ceil() as u64 + 1);
    }

    #[test]
    fn requests_without_an_output_cap_use_the_model_limit_or_stay_unbounded() {
        let catalog = PricingCatalog::standard();
        let request = text_request("gpt-4.1", "hi");
        let uncapped = catalog.max_cost_micro_usd(Some("openai"), "gpt-4.1", &request, None);
        let capped = catalog.max_cost_micro_usd(Some("openai"), "gpt-4.1", &request, Some(32_768));
        assert_eq!(uncapped, capped);
        // xAI publishes no output limit.
        assert_eq!(catalog.max_cost_micro_usd(Some("xai"), "grok-4.6", &request, None), None);
        assert!(catalog.max_cost_micro_usd(Some("xai"), "grok-4.6", &request, Some(100)).is_some());
        assert_eq!(
            catalog.max_cost_micro_usd(Some("together"), "gpt-4.1", &request, Some(1)),
            None
        );
        assert_eq!(catalog.max_cost_micro_usd(Some("ollama"), "qwen3.5", &request, None), Some(0));
    }

    #[test]
    fn built_in_tool_fees_are_bounded_by_max_uses_or_left_unbounded() {
        let catalog = PricingCatalog::standard();
        let with_tool = |model: &str, tool: serde_json::Value| {
            let mut request = text_request(model, "search the web");
            request.tools.insert("web_search".to_string(), tool);
            request
        };
        let plain = catalog
            .max_cost_micro_usd(
                Some("anthropic"),
                "claude-sonnet-4-6",
                &text_request("claude-sonnet-4-6", "search the web"),
                Some(1_000),
            )
            .unwrap();
        let capped = with_tool(
            "claude-sonnet-4-6",
            serde_json::json!({ "x-adk-anthropic-tool": { "type": "web_search_20250305", "max_uses": 3 } }),
        );
        let bound = catalog.max_cost_micro_usd(
            Some("anthropic"),
            "claude-sonnet-4-6",
            &capped,
            Some(1_000),
        );
        // Three searches at $0.01 each, plus the declaration's own input bytes.
        assert!(bound.unwrap() >= plain + 30_000, "{bound:?} vs {plain}");
        // Off the vendor's own API the fee does not apply.
        assert!(
            catalog
                .max_cost_micro_usd(Some("bedrock"), "claude-sonnet-4-6", &capped, Some(1_000))
                .unwrap()
                < plain + 30_000
        );

        let uncapped = with_tool(
            "claude-sonnet-4-6",
            serde_json::json!({ "x-adk-anthropic-tool": { "type": "web_search_20250305" } }),
        );
        assert_eq!(
            catalog.max_cost_micro_usd(
                Some("anthropic"),
                "claude-sonnet-4-6",
                &uncapped,
                Some(1_000)
            ),
            None
        );

        // Gemini 2.5 grounding is billed once per response.
        let grounded = with_tool(
            "gemini-2.5-flash",
            serde_json::json!({ "x-adk-gemini-tool": { "googleSearch": {} } }),
        );
        let bound = catalog
            .max_cost_micro_usd(Some("gemini"), "gemini-2.5-flash", &grounded, Some(0))
            .unwrap();
        let tokens = request_input_token_bound(&grounded).unwrap() as f64;
        assert_eq!(bound, (tokens * 0.30 + 35_000.0 + 25_000.0).ceil() as u64 + 1);
    }

    #[test]
    fn a_request_referencing_a_file_uri_cannot_be_bounded() {
        let mut request = text_request("gpt-4.1", "describe");
        request.contents[0].parts.push(Part::FileData {
            mime_type: "video/mp4".to_string(),
            file_uri: "gs://bucket/video.mp4".to_string(),
            annotations: None,
        });
        assert_eq!(request_input_token_bound(&request), None);
        let mut inline = text_request("gpt-4.1", "");
        inline.contents[0].parts.push(Part::InlineData {
            mime_type: "image/png".to_string(),
            data: vec![0; 10],
            uri: None,
            annotations: None,
        });
        assert_eq!(
            request_input_token_bound(&inline),
            Some(REQUEST_OVERHEAD_TOKENS + 16 + MEDIA_PART_MIN_TOKENS)
        );
    }

    #[test]
    fn gemini_reports_37_flash_as_38_flash_and_both_are_priced() {
        let catalog = PricingCatalog::standard();
        let usage = usage(1_000_000, 100_000);
        for model in ["gemini-3.8-flash", "gemini-3.7-flash", "models/gemini-3.8-flash"] {
            let cost = price("gemini", model).cost_usd(&usage, today());
            assert!((cost - (0.75 + 0.375)).abs() < 1e-9, "{model}: {cost}");
        }
        assert_eq!(price("gemini", "gemini-3.8-flash").max_output_tokens, Some(65_536));
        let after = price("gemini", "gemini-3.8-flash")
            .cost_usd(&usage, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
        assert!((after - (1.50 + 0.75)).abs() < 1e-9, "{after}");
        assert!(
            catalog
                .cost_usd(Some("gemini"), "gemini-3.1-pro-preview-customtools", &usage)
                .is_some()
        );
    }

    #[test]
    fn deepseek_bills_peak_rates_in_peak_hours_and_half_off_peak() {
        use chrono::TimeZone;
        let usage = UsageMetadata {
            cache_read_input_token_count: Some(500_000),
            ..usage(1_000_000, 100_000)
        };
        let flash = price("deepseek", "deepseek-flash");
        // Monday 2026-10-12 07:00 UTC is peak; 12:00 UTC and Sunday are off-peak.
        let peak = chrono::Utc.with_ymd_and_hms(2026, 10, 12, 7, 0, 0).unwrap();
        let midday = chrono::Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap();
        let sunday = chrono::Utc.with_ymd_and_hms(2026, 10, 11, 7, 0, 0).unwrap();
        // 500K uncached × $0.30 + 500K cache hits × $0.006 + 100K × $1.20.
        let expected_peak = 0.15 + 0.003 + 0.12;
        assert!((flash.cost_usd_at(&usage, peak) - expected_peak).abs() < 1e-9);
        assert!((flash.cost_usd_at(&usage, midday) - expected_peak / 2.0).abs() < 1e-9);
        assert!((flash.cost_usd_at(&usage, sunday) - expected_peak / 2.0).abs() < 1e-9);
        let pro = price("deepseek", "deepseek-v4-pro");
        assert!((pro.cost_usd_at(&usage, peak) - (0.66 + 0.022 + 0.396)).abs() < 1e-9);
        assert_eq!(price("deepseek", "deepseek-v4-flash").rates, flash.rates);
        assert!(PricingCatalog::standard().lookup(Some("deepseek"), "deepseek-chat").is_none());
        // The bound prices at peak, so it covers an off-peak call too.
        let request = text_request("deepseek-flash", "hi");
        let bound = PricingCatalog::standard()
            .max_cost_micro_usd(Some("deepseek"), "deepseek-flash", &request, None)
            .unwrap();
        let prompt = request_input_token_bound(&request).unwrap() as f64;
        assert_eq!(bound, (prompt * 0.30 + 393_216.0 * 1.20).ceil() as u64 + 1);
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
