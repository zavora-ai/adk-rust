//! Curated model identifiers, lifecycle metadata, and provider defaults.
//!
//! Provider catalogs change independently and often expose deployment-scoped or
//! account-scoped identifiers. This module therefore distinguishes curated ADK
//! recommendations from exhaustive provider discovery:
//!
//! - [`crate::catalog::recommended_model`] returns a stable ADK default only where a portable
//!   provider-level model ID exists.
//! - [`crate::catalog::lookup_model`] describes model IDs whose lifecycle ADK knows about.
//! - [`crate::catalog::validate_model_selection`] accepts unknown IDs so private, fine-tuned,
//!   newly released, and deployment-scoped models remain usable, but rejects
//!   identifiers known to be retired.
//!
//! Azure AI, Amazon Bedrock, and Volcano Engine Ark intentionally have no
//! universal default. Their model or deployment identifiers vary by resource,
//! region, account, or endpoint and must be supplied explicitly.

use serde::{Deserialize, Serialize};

/// Date on which the bundled catalog was verified against provider documentation.
pub const CATALOG_AS_OF: &str = "2026-11-17";

/// Recommended Google Gemini model for general agent workloads.
pub const GEMINI_DEFAULT: &str = "gemini-3.8-flash";
/// Recommended OpenAI model balancing capability, latency, and cost.
pub const OPENAI_DEFAULT: &str = "gpt-6.1-sol";
/// Recommended Anthropic model balancing capability, latency, and cost.
pub const ANTHROPIC_DEFAULT: &str = "claude-sonnet-5-5";
/// Output-token cap ADK requests from Claude 4 and later models when the caller sets none.
///
/// Thinking counts toward the cap, and Claude 5 models think on every turn, so a
/// small cap ends turns with `max_tokens` before the answer. Every Claude 4 and
/// later model accepts at least 32,000 output tokens.
pub const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 32_000;
/// Output-token cap ADK requests from older or unrecognized Anthropic models.
pub const ANTHROPIC_LEGACY_MAX_TOKENS: u32 = 4_096;
/// Recommended DeepSeek model for general agent workloads.
///
/// This is the id the API reports back. The legacy `deepseek-v4-flash` is still
/// accepted and is served by this same model.
pub const DEEPSEEK_DEFAULT: &str = "deepseek-flash";
/// Recommended Groq production model.
pub const GROQ_DEFAULT: &str = "openai/gpt-oss-120b";
/// Recommended Meta Muse model for general agent workloads.
pub const MUSE_DEFAULT: &str = "muse-spark-1.3";
/// Suggested local Ollama model. The model must already be installed locally.
pub const OLLAMA_DEFAULT: &str = "qwen3.5";
/// Recommended OpenRouter model. Callers should use model discovery for user-facing pickers.
pub const OPENROUTER_DEFAULT: &str = "qwen/qwen3.7-max";
/// Recommended OpenCode model, routed on both Zen (`opencode`) and Go (`opencode-go`).
pub const OPENCODE_DEFAULT: &str = "deepseek-v4.1-flash";
/// Recommended Fireworks balanced model.
pub const FIREWORKS_DEFAULT: &str = "accounts/fireworks/models/kimi-k3";
/// Recommended Together AI balanced model.
pub const TOGETHER_DEFAULT: &str = "moonshotai/Kimi-K3.1-Pro";
/// Recommended Mistral general-purpose model alias.
pub const MISTRAL_DEFAULT: &str = "mistral-large-latest";
/// Recommended Perplexity Sonar model.
pub const PERPLEXITY_DEFAULT: &str = "sonar-pro";
/// Recommended Cerebras production model.
pub const CEREBRAS_DEFAULT: &str = "gpt-oss-120b";
/// Recommended SambaNova production model.
pub const SAMBANOVA_DEFAULT: &str = "gpt-oss-120b";
/// Recommended xAI model.
pub const XAI_DEFAULT: &str = "grok-4.7";
/// Recommended MiniMax model. Model IDs are case-sensitive.
pub const MINIMAX_DEFAULT: &str = "MiniMax-M3";
/// Recommended Zhipu model.
pub const ZHIPU_DEFAULT: &str = "glm-5.3";
/// Recommended Baidu Qianfan model.
pub const BAIDU_DEFAULT: &str = "ernie-5.1";
/// Recommended Cohere model.
pub const COHERE_DEFAULT: &str = "command-a-plus-05-2026";

/// Recommended OpenAI Realtime model.
pub const OPENAI_REALTIME_DEFAULT: &str = "gpt-realtime-2.1";
/// Recommended Gemini Live model.
pub const GEMINI_LIVE_DEFAULT: &str = "gemini-3.1-flash-live-preview";
/// Recommended OpenAI live transcription model.
pub const OPENAI_LIVE_TRANSCRIPTION_DEFAULT: &str = "gpt-live-transcribe";
/// Recommended Gemini speech-to-text model.
pub const GEMINI_TRANSCRIPTION_DEFAULT: &str = GEMINI_DEFAULT;
/// Recommended Deepgram speech-to-text model.
pub const DEEPGRAM_DEFAULT: &str = "nova-3";
/// Recommended Cartesia text-to-speech model.
pub const CARTESIA_DEFAULT: &str = "sonic-3.5";
/// Recommended Gemini text-to-speech model.
pub const GEMINI_TTS_DEFAULT: &str = "gemini-3.1-flash-tts-preview";
/// Recommended Gemini embedding model.
pub const GEMINI_EMBEDDING_DEFAULT: &str = "gemini-embedding-2";
/// Recommended OpenAI embedding model.
pub const OPENAI_EMBEDDING_DEFAULT: &str = "text-embedding-3-small";

/// Lifecycle state for a model identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelLifecycle {
    /// Generally available and suitable for production defaults.
    Active,
    /// Available as a preview and subject to shorter lifecycle guarantees.
    Preview,
    /// Still available in at least one supported tier, but migration is recommended.
    Deprecated,
    /// No longer available on the provider surface represented by the entry.
    Retired,
}

/// Intended workload role of a curated model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelRole {
    /// Balanced default for general agent workloads.
    Balanced,
    /// Highest-quality or flagship workload tier.
    Flagship,
    /// Cost- or latency-oriented workload tier.
    Economy,
    /// Low-latency bidirectional audio model.
    Realtime,
    /// Speech recognition or transcription model.
    Transcription,
    /// Speech generation model.
    Speech,
    /// Embedding model.
    Embedding,
    /// Image generation model.
    Image,
}

/// One curated model-catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    /// Provider machine identifier.
    pub provider: &'static str,
    /// Exact provider model identifier.
    pub model: &'static str,
    /// Intended workload role.
    pub role: ModelRole,
    /// Current lifecycle state.
    pub lifecycle: ModelLifecycle,
    /// Recommended replacement for deprecated or retired entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement: Option<&'static str>,
    /// Known shutdown date in ISO 8601 form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown_date: Option<&'static str>,
    /// Whether ADK uses this entry as the provider's portable default.
    pub recommended_default: bool,
}

impl ModelCatalogEntry {
    const fn active(provider: &'static str, model: &'static str, role: ModelRole) -> Self {
        Self {
            provider,
            model,
            role,
            lifecycle: ModelLifecycle::Active,
            replacement: None,
            shutdown_date: None,
            recommended_default: false,
        }
    }

    const fn default(provider: &'static str, model: &'static str, role: ModelRole) -> Self {
        Self { recommended_default: true, ..Self::active(provider, model, role) }
    }

    const fn preview(provider: &'static str, model: &'static str, role: ModelRole) -> Self {
        Self {
            provider,
            model,
            role,
            lifecycle: ModelLifecycle::Preview,
            replacement: None,
            shutdown_date: None,
            recommended_default: false,
        }
    }

    const fn default_preview(provider: &'static str, model: &'static str, role: ModelRole) -> Self {
        Self { recommended_default: true, ..Self::preview(provider, model, role) }
    }

    const fn obsolete(
        provider: &'static str,
        model: &'static str,
        lifecycle: ModelLifecycle,
        replacement: &'static str,
        shutdown_date: Option<&'static str>,
    ) -> Self {
        Self {
            provider,
            model,
            role: ModelRole::Balanced,
            lifecycle,
            replacement: Some(replacement),
            shutdown_date,
            recommended_default: false,
        }
    }
}

/// Providers known to the catalog, including deployment-scoped providers.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "gemini",
    "openai",
    "anthropic",
    "deepseek",
    "groq",
    "muse",
    "ollama",
    "openrouter",
    "opencode",
    "opencode-go",
    "fireworks",
    "together",
    "mistral",
    "perplexity",
    "cerebras",
    "sambanova",
    "xai",
    "minimax",
    "zhipu",
    "baidu",
    "cohere",
    "azure-ai",
    "bedrock",
    "bytedance",
    "openai-realtime",
    "gemini-live",
    "openai-transcription",
    "gemini-transcription",
    "deepgram",
    "cartesia",
    "gemini-tts",
    "gemini-embedding",
    "openai-embedding",
];

/// Curated model entries bundled with ADK-Rust.
pub const MODEL_CATALOG: &[ModelCatalogEntry] = &[
    // Gemini: https://aistudio.google.com/docs/models and /docs/deprecations, 2026-11-17.
    ModelCatalogEntry::default("gemini", GEMINI_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("gemini", "gemini-4-hydrogen", ModelRole::Balanced),
    ModelCatalogEntry::active("gemini", "gemini-4-carbon", ModelRole::Flagship),
    ModelCatalogEntry::active("gemini", "gemini-4-argon", ModelRole::Flagship),
    ModelCatalogEntry::active("gemini", "gemini-3.6-flash", ModelRole::Balanced),
    ModelCatalogEntry::active("gemini", "gemini-3.5-flash-lite", ModelRole::Economy),
    ModelCatalogEntry::active("gemini", "gemini-2.5-flash-lite", ModelRole::Economy),
    ModelCatalogEntry::preview("gemini", "gemini-3.1-pro-preview", ModelRole::Flagship),
    ModelCatalogEntry::active("gemini", "gemini-nano-banana-2.1", ModelRole::Image),
    ModelCatalogEntry::active("gemini", "gemini-3.1-flash-lite-image", ModelRole::Image),
    ModelCatalogEntry::active("gemini", "gemini-3-pro-image", ModelRole::Image),
    ModelCatalogEntry::default("gemini-embedding", GEMINI_EMBEDDING_DEFAULT, ModelRole::Embedding),
    // OpenAI: https://developers.openai.com/api/docs/models and /docs/deprecations, 2026-11-17.
    ModelCatalogEntry::default("openai", OPENAI_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("openai", "gpt-6-astra", ModelRole::Flagship),
    ModelCatalogEntry::active("openai", "gpt-6.1-terra", ModelRole::Balanced),
    ModelCatalogEntry::active("openai", "gpt-6-luna", ModelRole::Economy),
    ModelCatalogEntry::active("openai", "gpt-6-sol", ModelRole::Balanced),
    ModelCatalogEntry::active("openai", "gpt-5.6-terra", ModelRole::Balanced),
    ModelCatalogEntry::active("openai", "gpt-5.6-sol", ModelRole::Flagship),
    ModelCatalogEntry::active("openai", "gpt-5.6-luna", ModelRole::Economy),
    ModelCatalogEntry::active("openai", "gpt-5.6", ModelRole::Flagship),
    // Anthropic: https://platform.claude.com/docs/en/about-claude/models/overview and
    // /model-deprecations, 2026-11-17.
    ModelCatalogEntry::default("anthropic", ANTHROPIC_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("anthropic", "claude-fable-5-2", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-mythos-5-2", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-opus-5-5", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-haiku-5-5", ModelRole::Economy),
    ModelCatalogEntry::active("anthropic", "claude-fable-5-1", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-mythos-5-1", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-fable-5", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-mythos-5", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-opus-5", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-sonnet-5", ModelRole::Balanced),
    ModelCatalogEntry::active("anthropic", "claude-opus-4-8", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-opus-4-7", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-opus-4-6", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-sonnet-4-6", ModelRole::Balanced),
    ModelCatalogEntry::active("anthropic", "claude-haiku-4-5", ModelRole::Economy),
    // DeepSeek: https://api-docs.deepseek.com/quick_start/pricing, 2026-11-17.
    ModelCatalogEntry::default("deepseek", DEEPSEEK_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("deepseek", "deepseek-v4-pro", ModelRole::Flagship),
    // Groq: https://console.groq.com/docs/models, 2026-11-17.
    ModelCatalogEntry::default("groq", GROQ_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("groq", "openai/gpt-oss-20b", ModelRole::Economy),
    ModelCatalogEntry::preview("groq", "openai/gpt-oss-safeguard-20b", ModelRole::Economy),
    ModelCatalogEntry::preview("groq", "qwen/qwen3.8-27b", ModelRole::Balanced),
    // Meta Muse: https://dev.meta.ai/docs/pricing-rate-limits, 2026-11-17.
    ModelCatalogEntry::default("muse", MUSE_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("muse", "muse-spark-1.2", ModelRole::Balanced),
    ModelCatalogEntry::active("muse", "muse-spark-1.1", ModelRole::Balanced),
    ModelCatalogEntry::active("muse", "muse-ember-1.0", ModelRole::Economy),
    ModelCatalogEntry::active("muse", "muse-spark-1.3-contributor", ModelRole::Economy),
    ModelCatalogEntry::active("muse", "muse-spark-1.2-contributor", ModelRole::Economy),
    ModelCatalogEntry::preview("muse", "muse-wonder-1.0", ModelRole::Flagship),
    ModelCatalogEntry::active("muse", "muse-image-1.0", ModelRole::Image),
    ModelCatalogEntry::active("muse", "muse-voice-transcribe-1.0", ModelRole::Transcription),
    ModelCatalogEntry::active("muse", "sam-3.1", ModelRole::Image),
    ModelCatalogEntry::default("ollama", OLLAMA_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("openrouter", OPENROUTER_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("opencode", OPENCODE_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("opencode-go", OPENCODE_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("fireworks", FIREWORKS_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("together", TOGETHER_DEFAULT, ModelRole::Balanced),
    // Mistral: https://docs.mistral.ai/models/, 2026-11-17.
    ModelCatalogEntry::default("mistral", MISTRAL_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("mistral", "mistral-large-4", ModelRole::Flagship),
    ModelCatalogEntry::active("mistral", "mistral-small-latest", ModelRole::Economy),
    ModelCatalogEntry::default("perplexity", PERPLEXITY_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("cerebras", CEREBRAS_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("sambanova", SAMBANOVA_DEFAULT, ModelRole::Balanced),
    // xAI: https://docs.x.ai/docs/models, 2026-11-17.
    ModelCatalogEntry::default("xai", XAI_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("xai", "grok-4.6", ModelRole::Balanced),
    ModelCatalogEntry::active("xai", "grok-4.3", ModelRole::Economy),
    ModelCatalogEntry::default("minimax", MINIMAX_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("minimax", "MiniMax-M3.1-Flash", ModelRole::Economy),
    ModelCatalogEntry::default("zhipu", ZHIPU_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("zhipu", "glm-5.2", ModelRole::Balanced),
    ModelCatalogEntry::default("baidu", BAIDU_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("cohere", COHERE_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("openai-realtime", OPENAI_REALTIME_DEFAULT, ModelRole::Realtime),
    ModelCatalogEntry::default_preview("gemini-live", GEMINI_LIVE_DEFAULT, ModelRole::Realtime),
    ModelCatalogEntry::active("gemini-live", "gemini-3.8-live", ModelRole::Realtime),
    ModelCatalogEntry::active(
        "gemini-live",
        "gemini-3.8-live-extended-thinking",
        ModelRole::Realtime,
    ),
    ModelCatalogEntry::default(
        "openai-transcription",
        OPENAI_LIVE_TRANSCRIPTION_DEFAULT,
        ModelRole::Transcription,
    ),
    ModelCatalogEntry::default(
        "gemini-transcription",
        GEMINI_TRANSCRIPTION_DEFAULT,
        ModelRole::Transcription,
    ),
    ModelCatalogEntry::default("deepgram", DEEPGRAM_DEFAULT, ModelRole::Transcription),
    ModelCatalogEntry::default("cartesia", CARTESIA_DEFAULT, ModelRole::Speech),
    ModelCatalogEntry::default_preview("gemini-tts", GEMINI_TTS_DEFAULT, ModelRole::Speech),
    ModelCatalogEntry::active("gemini-tts", "gemini-3.8-flash-tts", ModelRole::Speech),
    ModelCatalogEntry::active("gemini-tts", "gemini-3.8-flash-lite-tts", ModelRole::Speech),
    ModelCatalogEntry::default("openai-embedding", OPENAI_EMBEDDING_DEFAULT, ModelRole::Embedding),
    // ── Deprecated: still served, migration recommended ──
    // Gemini 3.7 Flash and 3.5 Flash are routed to their successors on the Gemini API;
    // Vertex AI retires Gemini 3.7 Flash on 2027-01-28.
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.7-flash",
        ModelLifecycle::Deprecated,
        GEMINI_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.5-flash",
        ModelLifecycle::Deprecated,
        "gemini-3.6-flash",
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-lite",
        ModelLifecycle::Deprecated,
        "gemini-3.5-flash-lite",
        Some("2027-05-07"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3-flash-preview",
        ModelLifecycle::Deprecated,
        "gemini-3.6-flash",
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-pro",
        ModelLifecycle::Deprecated,
        GEMINI_DEFAULT,
        Some("2026-12-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-flash",
        ModelLifecycle::Deprecated,
        "gemini-3.5-flash-lite",
        Some("2026-12-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-image",
        ModelLifecycle::Deprecated,
        "gemini-nano-banana-2.1",
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-flash-image",
        ModelLifecycle::Deprecated,
        "gemini-3.1-flash-lite-image",
        Some("2027-03-15"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini-tts",
        "gemini-2.5-flash-preview-tts",
        ModelLifecycle::Deprecated,
        "gemini-3.8-flash-tts",
        Some("2026-12-18"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini-tts",
        "gemini-2.5-pro-preview-tts",
        ModelLifecycle::Deprecated,
        "gemini-3.8-flash-tts",
        Some("2026-12-18"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini-embedding",
        "gemini-embedding-001",
        ModelLifecycle::Deprecated,
        GEMINI_EMBEDDING_DEFAULT,
        Some("2028-05-14"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5",
        ModelLifecycle::Deprecated,
        OPENAI_DEFAULT,
        Some("2026-12-11"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5-mini",
        ModelLifecycle::Deprecated,
        "gpt-6.1-terra",
        Some("2026-12-11"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5-nano",
        ModelLifecycle::Deprecated,
        "gpt-6-luna",
        Some("2026-12-11"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5-pro",
        ModelLifecycle::Deprecated,
        OPENAI_DEFAULT,
        Some("2026-12-11"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "o3",
        ModelLifecycle::Deprecated,
        OPENAI_DEFAULT,
        Some("2026-12-11"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-4.1",
        ModelLifecycle::Deprecated,
        "gpt-6-luna",
        Some("2027-01-14"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-4.1-mini",
        ModelLifecycle::Deprecated,
        "gpt-6-luna",
        Some("2027-01-14"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-4o",
        ModelLifecycle::Deprecated,
        "gpt-6-luna",
        Some("2027-01-14"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-4o-mini",
        ModelLifecycle::Deprecated,
        "gpt-6-luna",
        Some("2027-01-14"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5.1",
        ModelLifecycle::Deprecated,
        "gpt-6-sol",
        Some("2027-04-01"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5.3-codex",
        ModelLifecycle::Deprecated,
        "gpt-6-sol",
        Some("2027-04-01"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-5.4-nano",
        ModelLifecycle::Deprecated,
        "gpt-6-luna",
        Some("2027-04-01"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-sonnet-4-5",
        ModelLifecycle::Deprecated,
        ANTHROPIC_DEFAULT,
        Some("2026-11-30"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-sonnet-4-5-20250929",
        ModelLifecycle::Deprecated,
        ANTHROPIC_DEFAULT,
        Some("2026-11-30"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-mythos-preview",
        ModelLifecycle::Deprecated,
        "claude-mythos-5-2",
        None,
    ),
    ModelCatalogEntry::obsolete(
        "deepseek",
        "deepseek-v4-flash",
        ModelLifecycle::Deprecated,
        DEEPSEEK_DEFAULT,
        None,
    ),
    // Groq shut these down for free and developer tiers on 2026-08-16; enterprise
    // contracts still run them.
    ModelCatalogEntry::obsolete(
        "groq",
        "llama-3.3-70b-versatile",
        ModelLifecycle::Deprecated,
        GROQ_DEFAULT,
        Some("2026-08-16"),
    ),
    ModelCatalogEntry::obsolete(
        "groq",
        "llama-3.1-8b-instant",
        ModelLifecycle::Deprecated,
        "openai/gpt-oss-20b",
        Some("2026-08-16"),
    ),
    ModelCatalogEntry::obsolete(
        "mistral",
        "mistral-medium-latest",
        ModelLifecycle::Deprecated,
        MISTRAL_DEFAULT,
        Some("2027-04-30"),
    ),
    ModelCatalogEntry::obsolete(
        "mistral",
        "mistral-medium-3-5",
        ModelLifecycle::Deprecated,
        MISTRAL_DEFAULT,
        Some("2027-04-30"),
    ),
    ModelCatalogEntry::obsolete(
        "mistral",
        "mistral-medium-3",
        ModelLifecycle::Deprecated,
        MISTRAL_DEFAULT,
        Some("2027-04-30"),
    ),
    ModelCatalogEntry::obsolete(
        "mistral",
        "mistral-large-2512",
        ModelLifecycle::Deprecated,
        MISTRAL_DEFAULT,
        Some("2027-04-30"),
    ),
    ModelCatalogEntry::obsolete(
        "xai",
        "grok-code-fast-1",
        ModelLifecycle::Deprecated,
        "grok-build-0.1",
        Some("2026-05-15"),
    ),
    // Fireworks stopped serving Kimi K2.6 serverless on 2026-09-25; on-demand
    // deployments still run it.
    ModelCatalogEntry::obsolete(
        "fireworks",
        "accounts/fireworks/models/kimi-k2p6",
        ModelLifecycle::Deprecated,
        FIREWORKS_DEFAULT,
        Some("2026-09-25"),
    ),
    ModelCatalogEntry::obsolete(
        "together",
        "MiniMaxAI/MiniMax-M2.7",
        ModelLifecycle::Deprecated,
        TOGETHER_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "minimax",
        "MiniMax-M2.7",
        ModelLifecycle::Deprecated,
        MINIMAX_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "cartesia",
        "sonic-2",
        ModelLifecycle::Deprecated,
        CARTESIA_DEFAULT,
        None,
    ),
    // Google names `gemini-3.8-live` as the replacement of each of these.
    ModelCatalogEntry::obsolete(
        "gemini-live",
        "gemini-2.5-flash-native-audio-preview-12-2025",
        ModelLifecycle::Deprecated,
        "gemini-3.8-live",
        Some("2027-03-31"),
    ),
    // Vertex AI's name; AI Studio answers it with an error.
    ModelCatalogEntry::obsolete(
        "gemini-live",
        "gemini-live-2.5-flash-native-audio",
        ModelLifecycle::Deprecated,
        "gemini-3.8-live",
        Some("2026-12-13"),
    ),
    // ── Retired: requests fail ──
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-lite-preview",
        ModelLifecycle::Retired,
        "gemini-3.1-flash-lite",
        Some("2026-05-25"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3-pro-preview",
        ModelLifecycle::Retired,
        "gemini-3.1-pro-preview",
        Some("2026-03-09"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-image-preview",
        ModelLifecycle::Retired,
        "gemini-nano-banana-2.1",
        Some("2026-06-25"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3-pro-image-preview",
        ModelLifecycle::Retired,
        "gemini-3-pro-image",
        Some("2026-06-25"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-computer-use-preview-10-2025",
        ModelLifecycle::Retired,
        GEMINI_DEFAULT,
        Some("2026-07-28"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-flash-preview-09-2025",
        ModelLifecycle::Retired,
        "gemini-3.6-flash",
        Some("2026-02-17"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-flash-lite-preview-09-2025",
        ModelLifecycle::Retired,
        "gemini-3.5-flash-lite",
        Some("2026-03-31"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.0-flash",
        ModelLifecycle::Retired,
        "gemini-3.6-flash",
        Some("2026-06-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.0-flash-001",
        ModelLifecycle::Retired,
        "gemini-3.6-flash",
        Some("2026-06-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.0-flash-lite",
        ModelLifecycle::Retired,
        "gemini-3.5-flash-lite",
        Some("2026-06-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.0-flash-lite-001",
        ModelLifecycle::Retired,
        "gemini-3.5-flash-lite",
        Some("2026-06-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-flash-image-preview",
        ModelLifecycle::Retired,
        "gemini-nano-banana-2.1",
        Some("2026-01-15"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "gpt-4.1-nano",
        ModelLifecycle::Retired,
        "gpt-5.6-luna",
        Some("2026-10-23"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "o4-mini",
        ModelLifecycle::Retired,
        "gpt-5.6-terra",
        Some("2026-10-23"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "o3-mini",
        ModelLifecycle::Retired,
        "gpt-5.6-sol",
        Some("2026-10-23"),
    ),
    ModelCatalogEntry::obsolete(
        "openai",
        "o1",
        ModelLifecycle::Retired,
        "gpt-5.6-sol",
        Some("2026-10-23"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-opus-4-1",
        ModelLifecycle::Retired,
        "claude-opus-4-8",
        Some("2026-08-05"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-opus-4-1-20250805",
        ModelLifecycle::Retired,
        "claude-opus-4-8",
        Some("2026-08-05"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-opus-4-0",
        ModelLifecycle::Retired,
        "claude-opus-4-8",
        Some("2026-06-15"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-opus-4-20250514",
        ModelLifecycle::Retired,
        "claude-opus-4-8",
        Some("2026-06-15"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-sonnet-4-0",
        ModelLifecycle::Retired,
        ANTHROPIC_DEFAULT,
        Some("2026-06-15"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-sonnet-4-20250514",
        ModelLifecycle::Retired,
        ANTHROPIC_DEFAULT,
        Some("2026-06-15"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-3-7-sonnet-20250219",
        ModelLifecycle::Retired,
        ANTHROPIC_DEFAULT,
        Some("2026-02-19"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-3-5-haiku-20241022",
        ModelLifecycle::Retired,
        "claude-haiku-5-5",
        Some("2026-02-19"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-3-haiku-20240307",
        ModelLifecycle::Retired,
        "claude-haiku-5-5",
        Some("2026-04-20"),
    ),
    ModelCatalogEntry::obsolete(
        "anthropic",
        "claude-3-opus-20240229",
        ModelLifecycle::Retired,
        "claude-opus-4-8",
        Some("2026-01-05"),
    ),
    ModelCatalogEntry::obsolete(
        "groq",
        "meta-llama/llama-4-scout-17b-16e-instruct",
        ModelLifecycle::Retired,
        GROQ_DEFAULT,
        Some("2026-07-17"),
    ),
    ModelCatalogEntry::obsolete(
        "groq",
        "qwen/qwen3-32b",
        ModelLifecycle::Retired,
        GROQ_DEFAULT,
        Some("2026-07-17"),
    ),
    ModelCatalogEntry::obsolete(
        "deepseek",
        "deepseek-chat",
        ModelLifecycle::Retired,
        DEEPSEEK_DEFAULT,
        Some("2026-07-24"),
    ),
    ModelCatalogEntry::obsolete(
        "deepseek",
        "deepseek-reasoner",
        ModelLifecycle::Retired,
        "deepseek-v4-pro",
        Some("2026-07-24"),
    ),
    ModelCatalogEntry::obsolete(
        "mistral",
        "mistral-medium-2508",
        ModelLifecycle::Retired,
        MISTRAL_DEFAULT,
        Some("2026-08-31"),
    ),
    ModelCatalogEntry::obsolete(
        "mistral",
        "mistral-medium-2505",
        ModelLifecycle::Retired,
        MISTRAL_DEFAULT,
        Some("2026-08-31"),
    ),
    ModelCatalogEntry::obsolete(
        "cerebras",
        "llama-3.3-70b",
        ModelLifecycle::Retired,
        CEREBRAS_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini-live",
        "gemini-live-2.5-flash-preview",
        ModelLifecycle::Retired,
        "gemini-3.8-live",
        Some("2025-12-09"),
    ),
    ModelCatalogEntry::obsolete(
        "minimax",
        "minimax-m2.7",
        ModelLifecycle::Retired,
        MINIMAX_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete("baidu", "ernie-5", ModelLifecycle::Retired, BAIDU_DEFAULT, None),
];

/// Return the curated portable default for a provider.
///
/// Returns `None` for deployment-scoped providers (`azure-ai`, `bedrock`, and
/// `bytedance`) and unknown providers.
pub fn recommended_model(provider: &str) -> Option<&'static str> {
    match provider {
        "gemini" => Some(GEMINI_DEFAULT),
        "openai" => Some(OPENAI_DEFAULT),
        "anthropic" => Some(ANTHROPIC_DEFAULT),
        "deepseek" => Some(DEEPSEEK_DEFAULT),
        "groq" => Some(GROQ_DEFAULT),
        "muse" => Some(MUSE_DEFAULT),
        "ollama" => Some(OLLAMA_DEFAULT),
        "openrouter" => Some(OPENROUTER_DEFAULT),
        "opencode" | "opencode-go" => Some(OPENCODE_DEFAULT),
        "fireworks" => Some(FIREWORKS_DEFAULT),
        "together" => Some(TOGETHER_DEFAULT),
        "mistral" => Some(MISTRAL_DEFAULT),
        "perplexity" => Some(PERPLEXITY_DEFAULT),
        "cerebras" => Some(CEREBRAS_DEFAULT),
        "sambanova" => Some(SAMBANOVA_DEFAULT),
        "xai" => Some(XAI_DEFAULT),
        "minimax" => Some(MINIMAX_DEFAULT),
        "zhipu" => Some(ZHIPU_DEFAULT),
        "baidu" => Some(BAIDU_DEFAULT),
        "cohere" => Some(COHERE_DEFAULT),
        "openai-realtime" => Some(OPENAI_REALTIME_DEFAULT),
        "gemini-live" => Some(GEMINI_LIVE_DEFAULT),
        "openai-transcription" => Some(OPENAI_LIVE_TRANSCRIPTION_DEFAULT),
        "gemini-transcription" => Some(GEMINI_TRANSCRIPTION_DEFAULT),
        "deepgram" => Some(DEEPGRAM_DEFAULT),
        "cartesia" => Some(CARTESIA_DEFAULT),
        "gemini-tts" => Some(GEMINI_TTS_DEFAULT),
        "gemini-embedding" => Some(GEMINI_EMBEDDING_DEFAULT),
        "openai-embedding" => Some(OPENAI_EMBEDDING_DEFAULT),
        _ => None,
    }
}

/// Returns the output-token cap ADK requests from an Anthropic model when the
/// caller sets none.
///
/// Claude 4 and later models (`claude-opus-*`, `claude-sonnet-*`, `claude-haiku-*`,
/// `claude-fable-*`, `claude-mythos-*`, with or without a platform prefix such as
/// `us.anthropic.`) get [`ANTHROPIC_DEFAULT_MAX_TOKENS`]; older and unrecognized
/// identifiers get [`ANTHROPIC_LEGACY_MAX_TOKENS`], which every model accepts.
///
/// # Example
///
/// ```
/// use adk_model::catalog::{
///     ANTHROPIC_DEFAULT_MAX_TOKENS, ANTHROPIC_LEGACY_MAX_TOKENS, anthropic_default_max_tokens,
/// };
///
/// assert_eq!(anthropic_default_max_tokens("claude-opus-5-5"), ANTHROPIC_DEFAULT_MAX_TOKENS);
/// assert_eq!(anthropic_default_max_tokens("claude-3-5-haiku-20241022"), ANTHROPIC_LEGACY_MAX_TOKENS);
/// ```
pub fn anthropic_default_max_tokens(model: &str) -> u32 {
    let current_family = model.find("claude-").is_some_and(|start| {
        let family = &model[start + "claude-".len()..];
        ["opus-", "sonnet-", "haiku-", "fable-", "mythos-"]
            .iter()
            .any(|prefix| family.starts_with(prefix))
    });
    if current_family { ANTHROPIC_DEFAULT_MAX_TOKENS } else { ANTHROPIC_LEGACY_MAX_TOKENS }
}

/// Return whether a provider requires an account-, endpoint-, or deployment-specific model ID.
pub fn requires_explicit_model(provider: &str) -> bool {
    matches!(provider, "azure-ai" | "bedrock" | "bytedance")
}

/// Look up lifecycle metadata for a provider model ID.
///
/// Gemini's optional `models/` resource prefix is ignored for catalog lookup.
pub fn lookup_model(provider: &str, model: &str) -> Option<&'static ModelCatalogEntry> {
    let normalized = if provider == "gemini" || provider == "gemini-live" {
        model.strip_prefix("models/").unwrap_or(model)
    } else {
        model
    };
    MODEL_CATALOG.iter().find(|entry| entry.provider == provider && entry.model == normalized)
}

/// Validate a model selection without preventing newly released or private IDs.
///
/// Unknown IDs are accepted deliberately. Known retired IDs return an error;
/// known deprecated IDs remain usable so provider-plan exceptions and staged
/// migrations are not broken.
pub fn validate_model_selection(provider: &str, model: &str) -> adk_core::Result<()> {
    if model.trim().is_empty() {
        return Err(adk_core::AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::InvalidInput,
            "model.catalog.empty_model",
            format!(
                "model ID for provider '{provider}' is empty; pass an explicit model or deployment ID"
            ),
        )
        .with_provider(provider));
    }
    if let Some(entry) = lookup_model(provider, model)
        && entry.lifecycle == ModelLifecycle::Retired
    {
        let replacement = entry.replacement.unwrap_or("a current provider model");
        return Err(adk_core::AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::InvalidInput,
            "model.catalog.retired_model",
            format!(
                "model '{model}' is retired for provider '{provider}'; use '{replacement}' instead"
            ),
        )
        .with_provider(provider));
    }
    Ok(())
}

/// Emit a structured warning for a known deprecated or retired model.
///
/// Runtime constructors call this advisory helper instead of rejecting IDs to
/// preserve existing applications and provider-plan exceptions. Scaffolding
/// and validation tools should use [`crate::catalog::validate_model_selection`] to prevent new
/// projects from starting on a retired model.
pub fn warn_if_obsolete(provider: &str, model: &str) {
    if let Some(entry) = lookup_model(provider, model)
        && matches!(entry.lifecycle, ModelLifecycle::Deprecated | ModelLifecycle::Retired)
    {
        tracing::warn!(
            provider,
            model,
            lifecycle = ?entry.lifecycle,
            replacement = entry.replacement,
            shutdown_date = entry.shutdown_date,
            "configured model is obsolete"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn entries_are_unique() {
        let mut seen = HashSet::new();
        for entry in MODEL_CATALOG {
            assert!(
                seen.insert((entry.provider, entry.model)),
                "duplicate catalog entry: {entry:?}"
            );
        }
    }

    #[test]
    fn every_portable_default_is_active_and_catalogued() {
        for provider in KNOWN_PROVIDERS {
            let Some(model) = recommended_model(provider) else {
                assert!(requires_explicit_model(provider));
                continue;
            };
            let entry = lookup_model(provider, model)
                .unwrap_or_else(|| panic!("default {provider}/{model} is missing from catalog"));
            assert!(
                matches!(entry.lifecycle, ModelLifecycle::Active | ModelLifecycle::Preview),
                "obsolete default: {entry:?}"
            );
            assert!(entry.recommended_default, "default entry is not marked as default: {entry:?}");
        }
    }

    #[test]
    fn current_claude_models_are_active() {
        for (model, role) in [
            ("claude-fable-5-2", ModelRole::Flagship),
            ("claude-mythos-5-2", ModelRole::Flagship),
            ("claude-fable-5-1", ModelRole::Flagship),
            ("claude-opus-5-5", ModelRole::Flagship),
            ("claude-haiku-5-5", ModelRole::Economy),
        ] {
            assert_eq!(
                lookup_model("anthropic", model),
                Some(&ModelCatalogEntry::active("anthropic", model, role)),
                "{model}"
            );
            assert!(validate_model_selection("anthropic", model).is_ok());
        }
        assert_eq!(
            lookup_model("anthropic", "claude-sonnet-5-5"),
            Some(&ModelCatalogEntry::default(
                "anthropic",
                "claude-sonnet-5-5",
                ModelRole::Balanced
            ))
        );
    }

    #[test]
    fn superseded_models_name_a_current_replacement() {
        for (provider, model, lifecycle, replacement) in [
            ("gemini", "gemini-3.7-flash", ModelLifecycle::Deprecated, "gemini-3.8-flash"),
            ("openai", "gpt-5", ModelLifecycle::Deprecated, "gpt-6.1-sol"),
            ("openai", "o4-mini", ModelLifecycle::Retired, "gpt-5.6-terra"),
            (
                "anthropic",
                "claude-sonnet-4-5-20250929",
                ModelLifecycle::Deprecated,
                "claude-sonnet-5-5",
            ),
            ("anthropic", "claude-opus-4-1-20250805", ModelLifecycle::Retired, "claude-opus-4-8"),
            (
                "mistral",
                "mistral-medium-latest",
                ModelLifecycle::Deprecated,
                "mistral-large-latest",
            ),
            ("groq", "qwen/qwen3-32b", ModelLifecycle::Retired, "openai/gpt-oss-120b"),
        ] {
            let entry = lookup_model(provider, model).unwrap_or_else(|| panic!("{model}"));
            assert_eq!(
                (entry.lifecycle, entry.replacement),
                (lifecycle, Some(replacement)),
                "{provider}/{model}"
            );
        }
        // Every replacement is itself a catalogued model that is not retired.
        for entry in MODEL_CATALOG {
            let Some(replacement) = entry.replacement else { continue };
            let target = lookup_model(entry.provider, replacement);
            assert!(
                target.is_none_or(|target| target.lifecycle != ModelLifecycle::Retired),
                "{} replaces {} with a retired model",
                entry.provider,
                entry.model
            );
        }
    }

    #[test]
    fn anthropic_max_tokens_default_fits_the_model_generation() {
        for model in [
            ANTHROPIC_DEFAULT,
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-haiku-5-5",
            "claude-mythos-preview",
            "claude-opus-4-1-20250805",
            "claude-haiku-4-5",
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ] {
            assert_eq!(
                anthropic_default_max_tokens(model),
                ANTHROPIC_DEFAULT_MAX_TOKENS,
                "{model}"
            );
        }
        for model in ["claude-3-5-haiku-20241022", "claude-3-opus-20240229", "my-proxy-model"] {
            assert_eq!(anthropic_default_max_tokens(model), ANTHROPIC_LEGACY_MAX_TOKENS, "{model}");
        }
    }

    #[test]
    fn unknown_and_private_models_remain_usable() {
        assert!(validate_model_selection("openai", "ft:gpt-private:team:model").is_ok());
        assert!(validate_model_selection("azure-ai", "my-production-deployment").is_ok());
    }

    #[test]
    fn retired_model_reports_replacement() {
        let error = validate_model_selection("gemini", "models/gemini-2.0-flash")
            .expect_err("retired model must be rejected by explicit validation");
        assert!(error.to_string().contains("gemini-3.6-flash"));
    }

    #[test]
    fn live_catalog_dates_the_vertex_model_and_rejects_the_retired_studio_model() {
        let vertex = lookup_model("gemini-live", "models/gemini-live-2.5-flash-native-audio")
            .expect("Vertex Live model should be catalogued");
        assert_eq!(
            vertex,
            &ModelCatalogEntry::obsolete(
                "gemini-live",
                "gemini-live-2.5-flash-native-audio",
                ModelLifecycle::Deprecated,
                "gemini-3.8-live",
                Some("2026-12-13"),
            )
        );

        let error = validate_model_selection("gemini-live", "gemini-live-2.5-flash-preview")
            .expect_err("retired AI Studio Live model must be rejected");
        assert!(error.to_string().contains("gemini-3.8-live"));
    }

    /// The default stays the 3.1 preview, which Google gives no shutdown date; both 3.8 Live
    /// models are catalogued as generally available.
    #[test]
    fn live_catalog_lists_the_default_and_both_3_8_models() {
        for (model, entry) in [
            (
                GEMINI_LIVE_DEFAULT,
                ModelCatalogEntry::default_preview(
                    "gemini-live",
                    "gemini-3.1-flash-live-preview",
                    ModelRole::Realtime,
                ),
            ),
            (
                "models/gemini-3.8-live",
                ModelCatalogEntry::active("gemini-live", "gemini-3.8-live", ModelRole::Realtime),
            ),
            (
                "gemini-3.8-live-extended-thinking",
                ModelCatalogEntry::active(
                    "gemini-live",
                    "gemini-3.8-live-extended-thinking",
                    ModelRole::Realtime,
                ),
            ),
        ] {
            assert_eq!(lookup_model("gemini-live", model), Some(&entry), "{model}");
            assert!(validate_model_selection("gemini-live", model).is_ok(), "{model}");
        }
    }

    #[test]
    fn catalog_is_serializable() {
        let value = serde_json::to_value(MODEL_CATALOG).expect("catalog should serialize");
        assert!(value.as_array().is_some_and(|entries| !entries.is_empty()));
    }
}
