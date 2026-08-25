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
pub const CATALOG_AS_OF: &str = "2026-08-23";

/// Recommended Google Gemini model for general agent workloads.
pub const GEMINI_DEFAULT: &str = "gemini-3.7-flash";
/// Recommended OpenAI model balancing capability, latency, and cost.
pub const OPENAI_DEFAULT: &str = "gpt-5.6-terra";
/// Recommended Anthropic model balancing capability, latency, and cost.
pub const ANTHROPIC_DEFAULT: &str = "claude-sonnet-5";
/// Recommended DeepSeek model for general agent workloads.
pub const DEEPSEEK_DEFAULT: &str = "deepseek-v4-flash";
/// Recommended Groq production model.
pub const GROQ_DEFAULT: &str = "openai/gpt-oss-120b";
/// Suggested local Ollama model. The model must already be installed locally.
pub const OLLAMA_DEFAULT: &str = "qwen3.5";
/// Recommended OpenRouter model. Callers should use model discovery for user-facing pickers.
pub const OPENROUTER_DEFAULT: &str = "qwen/qwen3.7-max";
/// Recommended Fireworks balanced model.
pub const FIREWORKS_DEFAULT: &str = "accounts/fireworks/models/kimi-k2p6";
/// Recommended Together AI balanced model.
pub const TOGETHER_DEFAULT: &str = "MiniMaxAI/MiniMax-M2.7";
/// Recommended Mistral general-purpose model alias.
pub const MISTRAL_DEFAULT: &str = "mistral-medium-latest";
/// Recommended Perplexity Sonar model.
pub const PERPLEXITY_DEFAULT: &str = "sonar-pro";
/// Recommended Cerebras production model.
pub const CEREBRAS_DEFAULT: &str = "gpt-oss-120b";
/// Recommended SambaNova production model.
pub const SAMBANOVA_DEFAULT: &str = "gpt-oss-120b";
/// Recommended xAI model.
pub const XAI_DEFAULT: &str = "grok-4.6";
/// Recommended MiniMax model. Model IDs are case-sensitive.
pub const MINIMAX_DEFAULT: &str = "MiniMax-M2.7";
/// Recommended Zhipu model.
pub const ZHIPU_DEFAULT: &str = "glm-5.2";
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
    "ollama",
    "openrouter",
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
    ModelCatalogEntry::default("gemini", GEMINI_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("gemini", "gemini-3.6-flash", ModelRole::Balanced),
    ModelCatalogEntry::active("gemini", "gemini-3.5-flash", ModelRole::Balanced),
    ModelCatalogEntry::active("gemini", "gemini-3.5-flash-lite", ModelRole::Economy),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-lite",
        ModelLifecycle::Deprecated,
        "gemini-3.5-flash-lite",
        Some("2027-05-07"),
    ),
    ModelCatalogEntry::preview("gemini", "gemini-3.1-pro-preview", ModelRole::Flagship),
    ModelCatalogEntry::active("gemini", "gemini-3.1-flash-image", ModelRole::Image),
    ModelCatalogEntry::active("gemini", "gemini-3-pro-image", ModelRole::Image),
    ModelCatalogEntry::default("gemini-embedding", GEMINI_EMBEDDING_DEFAULT, ModelRole::Embedding),
    ModelCatalogEntry::default("openai", OPENAI_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("openai", "gpt-5.6-sol", ModelRole::Flagship),
    ModelCatalogEntry::active("openai", "gpt-5.6-luna", ModelRole::Economy),
    ModelCatalogEntry::active("openai", "gpt-5.6", ModelRole::Flagship),
    ModelCatalogEntry::default("anthropic", ANTHROPIC_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("anthropic", "claude-opus-5", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-fable-5", ModelRole::Flagship),
    ModelCatalogEntry::active("anthropic", "claude-haiku-4-5", ModelRole::Economy),
    ModelCatalogEntry::default("deepseek", DEEPSEEK_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("deepseek", "deepseek-v4-pro", ModelRole::Flagship),
    ModelCatalogEntry::default("groq", GROQ_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active("groq", "openai/gpt-oss-20b", ModelRole::Economy),
    ModelCatalogEntry::default("ollama", OLLAMA_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("openrouter", OPENROUTER_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("fireworks", FIREWORKS_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::active(
        "fireworks",
        "accounts/fireworks/models/kimi-k3",
        ModelRole::Flagship,
    ),
    ModelCatalogEntry::default("together", TOGETHER_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("mistral", MISTRAL_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("perplexity", PERPLEXITY_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("cerebras", CEREBRAS_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("sambanova", SAMBANOVA_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("xai", XAI_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("minimax", MINIMAX_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("zhipu", ZHIPU_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("baidu", BAIDU_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("cohere", COHERE_DEFAULT, ModelRole::Balanced),
    ModelCatalogEntry::default("openai-realtime", OPENAI_REALTIME_DEFAULT, ModelRole::Realtime),
    ModelCatalogEntry::default_preview("gemini-live", GEMINI_LIVE_DEFAULT, ModelRole::Realtime),
    ModelCatalogEntry::active(
        "gemini-live",
        "gemini-live-2.5-flash-native-audio",
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
    ModelCatalogEntry::default("openai-embedding", OPENAI_EMBEDDING_DEFAULT, ModelRole::Embedding),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-lite-preview",
        ModelLifecycle::Retired,
        "gemini-3.1-flash-lite",
        Some("2026-05-25"),
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
        "gemini-3-pro-preview",
        ModelLifecycle::Retired,
        "gemini-3.1-pro-preview",
        Some("2026-03-09"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-3.1-flash-image-preview",
        ModelLifecycle::Retired,
        "gemini-3.1-flash-image",
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
        "gemini-3.1-flash-lite",
        Some("2026-06-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.0-flash-lite-001",
        ModelLifecycle::Retired,
        "gemini-3.1-flash-lite",
        Some("2026-06-01"),
    ),
    ModelCatalogEntry::obsolete(
        "gemini",
        "gemini-2.5-flash-image-preview",
        ModelLifecycle::Retired,
        "gemini-3.1-flash-image",
        Some("2026-01-15"),
    ),
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
        "groq",
        "meta-llama/llama-4-scout-17b-16e-instruct",
        ModelLifecycle::Deprecated,
        GROQ_DEFAULT,
        Some("2026-07-17"),
    ),
    ModelCatalogEntry::obsolete(
        "groq",
        "qwen/qwen3-32b",
        ModelLifecycle::Deprecated,
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
        "cerebras",
        "llama-3.3-70b",
        ModelLifecycle::Retired,
        CEREBRAS_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "cartesia",
        "sonic-2",
        ModelLifecycle::Deprecated,
        CARTESIA_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini-live",
        "gemini-2.5-flash-native-audio-preview-12-2025",
        ModelLifecycle::Deprecated,
        GEMINI_LIVE_DEFAULT,
        None,
    ),
    ModelCatalogEntry::obsolete(
        "gemini-live",
        "gemini-live-2.5-flash-preview",
        ModelLifecycle::Retired,
        GEMINI_LIVE_DEFAULT,
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
        "ollama" => Some(OLLAMA_DEFAULT),
        "openrouter" => Some(OPENROUTER_DEFAULT),
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
    fn live_catalog_distinguishes_vertex_ga_from_retired_studio_model() {
        let vertex = lookup_model("gemini-live", "models/gemini-live-2.5-flash-native-audio")
            .expect("Vertex Live GA model should be catalogued");
        assert_eq!(vertex.lifecycle, ModelLifecycle::Active);

        let error = validate_model_selection("gemini-live", "gemini-live-2.5-flash-preview")
            .expect_err("retired AI Studio Live model must be rejected");
        assert!(error.to_string().contains(GEMINI_LIVE_DEFAULT));
    }

    #[test]
    fn catalog_is_serializable() {
        let value = serde_json::to_value(MODEL_CATALOG).expect("catalog should serialize");
        assert!(value.as_array().is_some_and(|entries| !entries.is_empty()));
    }
}
