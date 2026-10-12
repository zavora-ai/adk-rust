//! Model resolution — maps a [`ModelRef`] to a live `Arc<dyn Llm>`.
//!
//! The [`ModelResolver`] trait abstracts over model construction so that
//! the runtime can resolve any provider declaration into a callable model.
//! [`DefaultModelResolver`] implements the standard resolution logic:
//!
//! - **Shorthand** names are mapped to providers by prefix (`gemini-*` → Gemini,
//!   `gpt-*` → OpenAI, `claude-*` → Anthropic, etc.)
//! - **Structured** refs use the explicit `provider` field directly
//! - **OpenAI-compatible** refs construct a client with the given `base_url` + `api_key`
//!
//! Each provider's client is built only when its feature (`gemini`, `openai`, `anthropic`,
//! `ollama`) is enabled.

use std::sync::Arc;

use async_trait::async_trait;

use adk_core::Llm;

use crate::types::{ModelConfig, ModelRef, Provider};

/// Errors that can occur during model resolution.
#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    /// The model name prefix could not be mapped to a known provider.
    #[error(
        "cannot infer provider from model name \"{name}\". Expected prefix: gemini, gpt, claude, llama, mistral, or deepseek"
    )]
    UnknownProvider { name: String },

    /// The provider is recognized but model construction failed.
    #[error("failed to construct model for provider {provider:?}: {reason}")]
    ConstructionFailed { provider: Provider, reason: String },
}

/// Result alias for resolver operations.
pub type ResolverResult<T> = std::result::Result<T, ResolverError>;

/// Resolves a [`ModelRef`] into a live `Arc<dyn Llm>`.
///
/// Implementations may construct real provider clients or return pre-built
/// instances. The trait is async because construction may involve network
/// calls (e.g., verifying API keys or fetching model metadata).
///
/// # Example
///
/// ```rust,ignore
/// use adk_managed::resolver::{ModelResolver, DefaultModelResolver};
/// use adk_managed::types::ModelRef;
///
/// let resolver = DefaultModelResolver::new();
/// let model_ref = ModelRef::Shorthand("gemini-2.5-flash".to_string());
/// let llm = resolver.resolve(&model_ref).await?;
/// ```
#[async_trait]
pub trait ModelResolver: Send + Sync {
    /// Resolve a model reference into a callable LLM instance.
    async fn resolve(&self, model_ref: &ModelRef) -> ResolverResult<Arc<dyn Llm>>;
}

/// Infers the [`Provider`] from a shorthand model name by prefix matching.
///
/// # Mapping
///
/// | Prefix | Provider |
/// |--------|----------|
/// | `gemini` | Gemini |
/// | `gpt` | OpenAI |
/// | `claude` | Anthropic |
/// | `llama` | Ollama |
/// | `mistral` | Ollama |
/// | `deepseek` | Ollama |
///
/// Returns `Err(ResolverError::UnknownProvider)` if no prefix matches.
pub fn infer_provider(name: &str) -> ResolverResult<Provider> {
    let lower = name.to_lowercase();
    if lower.starts_with("gemini") {
        Ok(Provider::Gemini)
    } else if lower.starts_with("gpt") {
        Ok(Provider::Openai)
    } else if lower.starts_with("claude") {
        Ok(Provider::Anthropic)
    } else if lower.starts_with("llama")
        || lower.starts_with("mistral")
        || lower.starts_with("deepseek")
    {
        Ok(Provider::Ollama)
    } else {
        Err(ResolverError::UnknownProvider { name: name.to_string() })
    }
}

/// Default model resolver: infers the provider from a shorthand name or takes it from a
/// structured reference, then constructs that provider's client.
///
/// | Provider | Feature | Credentials |
/// |----------|---------|-------------|
/// | Gemini | `gemini` | `GOOGLE_API_KEY` or `GEMINI_API_KEY`; Vertex AI through `GOOGLE_GENAI_USE_VERTEXAI` |
/// | OpenAI | `openai` | `OPENAI_API_KEY` |
/// | Anthropic | `anthropic` | `ANTHROPIC_API_KEY` |
/// | Ollama | `ollama` | none; `OLLAMA_HOST` overrides the host |
/// | OpenAI-compatible | `openai` | the reference's `api_key` |
///
/// [`with_api_key`](Self::with_api_key) sets a key instead of reading the environment.
/// A provider whose feature is not built resolves to
/// [`ResolverError::ConstructionFailed`] naming the feature to enable. Construction makes
/// no network call, so a wrong key surfaces on the first model call.
///
/// # Example
///
/// ```rust
/// use adk_managed::resolver::DefaultModelResolver;
/// use adk_managed::types::Provider;
///
/// let resolver = DefaultModelResolver::new().with_api_key(Provider::Openai, "sk-test");
/// # let _ = resolver;
/// ```
#[derive(Debug, Clone, Default)]
pub struct DefaultModelResolver {
    api_keys: std::collections::HashMap<ProviderKey, String>,
}

/// `Provider` as a map key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ProviderKey {
    Gemini,
    Openai,
    Anthropic,
    Ollama,
    OpenaiCompatible,
}

impl From<Provider> for ProviderKey {
    fn from(provider: Provider) -> Self {
        match provider {
            Provider::Gemini => Self::Gemini,
            Provider::Openai => Self::Openai,
            Provider::Anthropic => Self::Anthropic,
            Provider::Ollama => Self::Ollama,
            Provider::OpenaiCompatible => Self::OpenaiCompatible,
        }
    }
}

impl DefaultModelResolver {
    /// A resolver that reads API keys from the environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses `api_key` for `provider` instead of the environment.
    ///
    /// For [`Provider::OpenaiCompatible`] the key applies when the reference's own
    /// `api_key` is empty, as it is in a session rebuilt from stored state.
    #[must_use]
    pub fn with_api_key(mut self, provider: Provider, api_key: impl Into<String>) -> Self {
        self.api_keys.insert(provider.into(), api_key.into());
        self
    }

    /// The configured key for `provider`, else the first non-empty `env` variable.
    #[allow(dead_code)] // unused when no provider feature that needs a key is built
    fn api_key(&self, provider: Provider, env: &[&str]) -> Option<String> {
        self.api_keys.get(&provider.into()).cloned().or_else(|| {
            env.iter()
                .filter_map(|name| std::env::var(name).ok())
                .find(|value| !value.trim().is_empty())
        })
    }

    fn construct(&self, provider: Provider, model: &str) -> ResolverResult<Arc<dyn Llm>> {
        let failed = |reason: String| ResolverError::ConstructionFailed { provider, reason };
        #[allow(unused_variables)]
        let missing_key = |env: &str| {
            failed(format!(
                "no API key for model {model}: set {env} or call \
                 DefaultModelResolver::with_api_key(Provider::{provider:?}, ..)"
            ))
        };
        #[allow(unused_variables)]
        let not_built = |feature: &str| {
            failed(format!(
                "model {model} needs the `{feature}` feature of adk-managed (or of adk-rust \
                 with `managed-runtime`); it is not built"
            ))
        };
        match provider {
            Provider::Gemini => {
                #[cfg(feature = "gemini")]
                {
                    let model = match self.api_key(provider, &[]) {
                        Some(key) => adk_model::GeminiModel::new(key, model),
                        None => adk_model::GeminiModel::from_env(model),
                    }
                    .map_err(|e| failed(e.to_string()))?;
                    Ok(Arc::new(model))
                }
                #[cfg(not(feature = "gemini"))]
                Err(not_built("gemini"))
            }
            Provider::Openai => {
                #[cfg(feature = "openai")]
                {
                    let key = self
                        .api_key(provider, &["OPENAI_API_KEY"])
                        .ok_or_else(|| missing_key("OPENAI_API_KEY"))?;
                    let client =
                        adk_model::OpenAIClient::new(adk_model::OpenAIConfig::new(key, model))
                            .map_err(|e| failed(e.to_string()))?;
                    Ok(Arc::new(client))
                }
                #[cfg(not(feature = "openai"))]
                Err(not_built("openai"))
            }
            Provider::Anthropic => {
                #[cfg(feature = "anthropic")]
                {
                    let key = self
                        .api_key(provider, &["ANTHROPIC_API_KEY"])
                        .ok_or_else(|| missing_key("ANTHROPIC_API_KEY"))?;
                    let client = adk_model::anthropic::AnthropicClient::new(
                        adk_model::anthropic::AnthropicConfig::new(key, model),
                    )
                    .map_err(|e| failed(e.to_string()))?;
                    Ok(Arc::new(client))
                }
                #[cfg(not(feature = "anthropic"))]
                Err(not_built("anthropic"))
            }
            Provider::Ollama => {
                #[cfg(feature = "ollama")]
                {
                    let config = match std::env::var("OLLAMA_HOST") {
                        Ok(host) if !host.trim().is_empty() => {
                            adk_model::OllamaConfig::with_host(host, model)
                        }
                        _ => adk_model::OllamaConfig::new(model),
                    };
                    let client =
                        adk_model::OllamaModel::new(config).map_err(|e| failed(e.to_string()))?;
                    Ok(Arc::new(client))
                }
                #[cfg(not(feature = "ollama"))]
                Err(not_built("ollama"))
            }
            Provider::OpenaiCompatible => Err(failed(format!(
                "model {model} is OpenAI-compatible; use the structured reference with \
                 `base_url` and `api_key`"
            ))),
        }
    }
}

#[async_trait]
impl ModelResolver for DefaultModelResolver {
    async fn resolve(&self, model_ref: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        match model_ref {
            ModelRef::Shorthand(name) => self.construct(infer_provider(name)?, name),
            ModelRef::Structured { provider, model: ModelConfig::Name(name), .. } => {
                self.construct(*provider, name)
            }
            ModelRef::Structured {
                provider,
                model: ModelConfig::Compatible { model, base_url, api_key },
                ..
            } => {
                #[cfg(feature = "openai")]
                {
                    let key = if api_key.is_empty() {
                        self.api_keys.get(&ProviderKey::OpenaiCompatible).cloned().ok_or_else(
                            || ResolverError::ConstructionFailed {
                                provider: *provider,
                                reason: format!(
                                    "model {model} at {base_url} has no API key: stored sessions \
                                     drop it, so call DefaultModelResolver::with_api_key(\
                                     Provider::OpenaiCompatible, ..)"
                                ),
                            },
                        )?
                    } else {
                        api_key.clone()
                    };
                    let client = adk_model::OpenAIClient::compatible(key, base_url, model)
                        .map_err(|e| ResolverError::ConstructionFailed {
                            provider: *provider,
                            reason: e.to_string(),
                        })?;
                    Ok(Arc::new(client))
                }
                #[cfg(not(feature = "openai"))]
                {
                    let _ = api_key;
                    Err(ResolverError::ConstructionFailed {
                        provider: *provider,
                        reason: format!(
                            "model {model} at {base_url} needs the `openai` feature of \
                             adk-managed; it is not built"
                        ),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- infer_provider tests ---

    #[test]
    fn test_infer_gemini_from_shorthand() {
        assert_eq!(infer_provider("gemini-2.5-flash").unwrap(), Provider::Gemini);
        assert_eq!(infer_provider("gemini-2.5-pro").unwrap(), Provider::Gemini);
        assert_eq!(infer_provider("gemini-3.1-flash-lite-preview").unwrap(), Provider::Gemini);
    }

    #[test]
    fn test_infer_openai_from_shorthand() {
        assert_eq!(infer_provider("gpt-4.1").unwrap(), Provider::Openai);
        assert_eq!(infer_provider("gpt-4o").unwrap(), Provider::Openai);
        assert_eq!(infer_provider("gpt-4.1-mini").unwrap(), Provider::Openai);
    }

    #[test]
    fn test_infer_anthropic_from_shorthand() {
        assert_eq!(infer_provider("claude-3.5-sonnet").unwrap(), Provider::Anthropic);
        assert_eq!(infer_provider("claude-4-opus").unwrap(), Provider::Anthropic);
    }

    #[test]
    fn test_infer_ollama_from_llama() {
        assert_eq!(infer_provider("llama-3.2-70b").unwrap(), Provider::Ollama);
    }

    #[test]
    fn test_infer_ollama_from_mistral() {
        assert_eq!(infer_provider("mistral-7b").unwrap(), Provider::Ollama);
        assert_eq!(infer_provider("mistral-large").unwrap(), Provider::Ollama);
    }

    #[test]
    fn test_infer_ollama_from_deepseek() {
        assert_eq!(infer_provider("deepseek-chat").unwrap(), Provider::Ollama);
        assert_eq!(infer_provider("deepseek-coder").unwrap(), Provider::Ollama);
    }

    #[test]
    fn test_infer_unknown_returns_error() {
        let result = infer_provider("some-random-model");
        assert!(result.is_err());
        match result.unwrap_err() {
            ResolverError::UnknownProvider { name } => {
                assert_eq!(name, "some-random-model");
            }
            _ => panic!("expected UnknownProvider error"),
        }
    }

    #[test]
    fn test_infer_case_insensitive() {
        assert_eq!(infer_provider("Gemini-2.5-flash").unwrap(), Provider::Gemini);
        assert_eq!(infer_provider("GPT-4.1").unwrap(), Provider::Openai);
        assert_eq!(infer_provider("Claude-3.5-sonnet").unwrap(), Provider::Anthropic);
        assert_eq!(infer_provider("LLAMA-3.2").unwrap(), Provider::Ollama);
        assert_eq!(infer_provider("DeepSeek-V3").unwrap(), Provider::Ollama);
    }

    // --- DefaultModelResolver tests ---

    /// The provider a resolution failed for, or `None` when it succeeded.
    async fn resolve_provider(model_ref: ModelRef) -> Result<String, Provider> {
        let resolver = DefaultModelResolver::new()
            .with_api_key(Provider::Gemini, "test-key")
            .with_api_key(Provider::Openai, "test-key")
            .with_api_key(Provider::Anthropic, "test-key");
        match resolver.resolve(&model_ref).await {
            Ok(llm) => Ok(llm.name().to_string()),
            Err(ResolverError::ConstructionFailed { provider, .. }) => Err(provider),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn shorthand_models_resolve_for_each_built_provider() {
        for (name, provider, built) in [
            ("gemini-3.7-flash", Provider::Gemini, cfg!(feature = "gemini")),
            ("gpt-5.4-mini", Provider::Openai, cfg!(feature = "openai")),
            ("claude-haiku-5-5", Provider::Anthropic, cfg!(feature = "anthropic")),
            ("llama-4", Provider::Ollama, cfg!(feature = "ollama")),
        ] {
            let resolved = resolve_provider(ModelRef::Shorthand(name.to_string())).await;
            if built {
                assert!(resolved.is_ok(), "{name} must resolve when its feature is built");
            } else {
                assert_eq!(resolved, Err(provider), "{name} reports the provider it needs");
            }
        }
    }

    #[tokio::test]
    async fn an_unbuilt_provider_names_its_feature() {
        let resolver = DefaultModelResolver::new();
        let missing = [
            ("gemini-3.7-flash", "gemini", cfg!(feature = "gemini")),
            ("gpt-5.4-mini", "openai", cfg!(feature = "openai")),
            ("claude-haiku-5-5", "anthropic", cfg!(feature = "anthropic")),
            ("llama-4", "ollama", cfg!(feature = "ollama")),
        ];
        for (name, feature, built) in missing {
            if built {
                continue;
            }
            match resolver.resolve(&ModelRef::Shorthand(name.to_string())).await {
                Err(ResolverError::ConstructionFailed { reason, .. }) => {
                    assert!(reason.contains(&format!("`{feature}`")), "{reason}");
                }
                Err(other) => panic!("unexpected error: {other}"),
                Ok(_) => panic!("{name} resolved without its feature"),
            }
        }
    }

    #[cfg(feature = "openai")]
    #[tokio::test]
    async fn a_missing_key_says_where_to_set_it() {
        if std::env::var("OPENAI_API_KEY").is_ok() {
            return;
        }
        let result = DefaultModelResolver::new()
            .resolve(&ModelRef::Shorthand("gpt-5.4-mini".to_string()))
            .await;
        match result {
            Err(ResolverError::ConstructionFailed { reason, .. }) => {
                assert!(reason.contains("OPENAI_API_KEY"), "{reason}");
            }
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("resolved without a key"),
        }
    }

    #[tokio::test]
    async fn structured_refs_use_their_provider_field() {
        let resolved = resolve_provider(ModelRef::Structured {
            provider: Provider::Anthropic,
            model: ModelConfig::Name("claude-haiku-5-5".to_string()),
            speed: Some("fast".to_string()),
        })
        .await;
        assert_eq!(resolved.is_ok(), cfg!(feature = "anthropic"));
        if !cfg!(feature = "anthropic") {
            assert_eq!(resolved, Err(Provider::Anthropic));
        }
    }

    #[tokio::test]
    async fn openai_compatible_refs_resolve_with_their_endpoint() {
        let model_ref = |api_key: &str| ModelRef::Structured {
            provider: Provider::OpenaiCompatible,
            model: ModelConfig::Compatible {
                model: "deepseek-v4-flash".to_string(),
                base_url: "https://api.deepseek.com/v1".to_string(),
                api_key: api_key.to_string(),
            },
            speed: None,
        };
        let resolver = DefaultModelResolver::new();
        assert_eq!(resolver.resolve(&model_ref("sk-test")).await.is_ok(), cfg!(feature = "openai"));
        if cfg!(feature = "openai") {
            // A rebuilt session has no stored key; the resolver supplies it.
            assert!(resolver.resolve(&model_ref("")).await.is_err());
            let keyed = resolver.with_api_key(Provider::OpenaiCompatible, "sk-test");
            assert!(keyed.resolve(&model_ref("")).await.is_ok());
        }
    }

    #[tokio::test]
    async fn an_unknown_shorthand_is_an_unknown_provider() {
        let result =
            DefaultModelResolver::new().resolve(&ModelRef::Shorthand("mystery".into())).await;
        assert!(matches!(result, Err(ResolverError::UnknownProvider { .. })));
    }
}
