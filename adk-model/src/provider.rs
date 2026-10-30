use std::fmt::{Display, Formatter};
use std::str::FromStr;

/// Canonical identifiers and metadata for every provider `adk-model` has its own client for.
///
/// Each variant belongs to one provider feature of this crate ([`ModelProvider::feature`]), and
/// [`ModelProvider::ALL`] covers every feature `all-providers` enables. The OpenAI-compatible
/// presets (`xai`, `fireworks`, `together`, `mistral`, `perplexity`, `cerebras`, `sambanova`)
/// run on the `openai` feature's `OpenAICompatible` client and are not variants. OpenCode has
/// one variant per service, named after the provider its responses carry: `opencode` (Zen)
/// and `opencode-go` (Go).
///
/// # Example
///
/// ```
/// use adk_model::ModelProvider;
/// use adk_model::catalog::OPENROUTER_DEFAULT;
///
/// let provider: ModelProvider = "openrouter".parse()?;
/// assert_eq!(provider.feature(), "openrouter");
/// assert_eq!(provider.env_var(), "OPENROUTER_API_KEY");
/// assert_eq!(provider.default_model(), Some(OPENROUTER_DEFAULT));
///
/// // Deployment-scoped providers have no portable default model.
/// assert_eq!(ModelProvider::Bedrock.default_model(), None);
/// # Ok::<(), String>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelProvider {
    /// Google Gemini models.
    Gemini,
    /// OpenAI models (GPT, o-series).
    Openai,
    /// Anthropic Claude models.
    Anthropic,
    /// DeepSeek models.
    Deepseek,
    /// Groq ultra-fast inference.
    Groq,
    /// Ollama local models.
    Ollama,
    /// Meta Muse Spark models through the Meta Model API.
    Muse,
    /// OpenRouter, one API across many model providers.
    Openrouter,
    /// OpenCode Zen, the pay-as-you-go model gateway.
    Opencode,
    /// OpenCode Go, the subscription coding service.
    OpencodeGo,
    /// Amazon Bedrock, on the AWS credential chain or a Bedrock API key.
    Bedrock,
    /// Azure AI Inference endpoints.
    AzureAi,
}

impl ModelProvider {
    /// All providers in UI/display order.
    pub const ALL: [Self; 12] = [
        Self::Gemini,
        Self::Openai,
        Self::Anthropic,
        Self::Deepseek,
        Self::Groq,
        Self::Ollama,
        Self::Muse,
        Self::Openrouter,
        Self::Opencode,
        Self::OpencodeGo,
        Self::Bedrock,
        Self::AzureAi,
    ];

    /// Return all providers in a stable order.
    pub const fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Machine identifier used in CLIs and configs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Deepseek => "deepseek",
            Self::Groq => "groq",
            Self::Ollama => "ollama",
            Self::Muse => "muse",
            Self::Openrouter => "openrouter",
            Self::Opencode => "opencode",
            Self::OpencodeGo => "opencode-go",
            Self::Bedrock => "bedrock",
            Self::AzureAi => "azure-ai",
        }
    }

    /// The `adk-model` Cargo feature that compiles the provider's client.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::ModelProvider;
    ///
    /// assert_eq!(ModelProvider::OpencodeGo.feature(), "opencode");
    /// assert_eq!(ModelProvider::AzureAi.feature(), "azure-ai");
    /// ```
    pub const fn feature(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Deepseek => "deepseek",
            Self::Groq => "groq",
            Self::Ollama => "ollama",
            Self::Muse => "muse",
            Self::Openrouter => "openrouter",
            Self::Opencode | Self::OpencodeGo => "opencode",
            Self::Bedrock => "bedrock",
            Self::AzureAi => "azure-ai",
        }
    }

    /// Default model for the provider: its [`crate::catalog::recommended_model`].
    ///
    /// `None` for Bedrock and Azure AI, whose model or deployment ID depends on the account,
    /// region or endpoint ([`crate::catalog::requires_explicit_model`]).
    pub const fn default_model(self) -> Option<&'static str> {
        match self {
            Self::Gemini => Some(crate::catalog::GEMINI_DEFAULT),
            Self::Openai => Some(crate::catalog::OPENAI_DEFAULT),
            Self::Anthropic => Some(crate::catalog::ANTHROPIC_DEFAULT),
            Self::Deepseek => Some(crate::catalog::DEEPSEEK_DEFAULT),
            Self::Groq => Some(crate::catalog::GROQ_DEFAULT),
            Self::Ollama => Some(crate::catalog::OLLAMA_DEFAULT),
            Self::Muse => Some(crate::catalog::MUSE_DEFAULT),
            Self::Openrouter => Some(crate::catalog::OPENROUTER_DEFAULT),
            Self::Opencode | Self::OpencodeGo => Some(crate::catalog::OPENCODE_DEFAULT),
            Self::Bedrock | Self::AzureAi => None,
        }
    }

    /// Primary environment variable used for the provider API key; empty for Ollama.
    ///
    /// For Bedrock it is the Bedrock API key, `AWS_BEARER_TOKEN_BEDROCK`; without one the
    /// AWS credential chain applies.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Gemini => "GOOGLE_API_KEY",
            Self::Openai => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Deepseek => "DEEPSEEK_API_KEY",
            Self::Groq => "GROQ_API_KEY",
            Self::Ollama => "",
            Self::Muse => "META_API_KEY",
            Self::Openrouter => "OPENROUTER_API_KEY",
            Self::Opencode | Self::OpencodeGo => "OPENCODE_API_KEY",
            Self::Bedrock => "AWS_BEARER_TOKEN_BEDROCK",
            Self::AzureAi => "AZURE_AI_API_KEY",
        }
    }

    /// Alternate environment variable used for the provider API key.
    pub const fn alt_env_var(self) -> Option<&'static str> {
        match self {
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::Muse => Some("MODEL_API_KEY"),
            Self::Openai
            | Self::Anthropic
            | Self::Deepseek
            | Self::Groq
            | Self::Ollama
            | Self::Openrouter
            | Self::Opencode
            | Self::OpencodeGo
            | Self::Bedrock
            | Self::AzureAi => None,
        }
    }

    /// Whether the provider requires an API key: not Ollama, and not Bedrock, which falls
    /// back to the AWS credential chain.
    pub const fn requires_key(self) -> bool {
        match self {
            Self::Ollama | Self::Bedrock => false,
            Self::Gemini
            | Self::Openai
            | Self::Anthropic
            | Self::Deepseek
            | Self::Groq
            | Self::Muse
            | Self::Openrouter
            | Self::Opencode
            | Self::OpencodeGo
            | Self::AzureAi => true,
        }
    }

    /// Display name for interactive prompts and help text.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini (Google)",
            Self::Openai => "OpenAI",
            Self::Anthropic => "Anthropic (Claude)",
            Self::Deepseek => "DeepSeek",
            Self::Groq => "Groq",
            Self::Ollama => "Ollama (local, no key needed)",
            Self::Muse => "Meta Muse",
            Self::Openrouter => "OpenRouter",
            Self::Opencode => "OpenCode Zen",
            Self::OpencodeGo => "OpenCode Go",
            Self::Bedrock => "Amazon Bedrock",
            Self::AzureAi => "Azure AI Inference",
        }
    }
}

impl Display for ModelProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ModelProvider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini" => Ok(Self::Gemini),
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "deepseek" => Ok(Self::Deepseek),
            "groq" => Ok(Self::Groq),
            "ollama" => Ok(Self::Ollama),
            "muse" => Ok(Self::Muse),
            "openrouter" => Ok(Self::Openrouter),
            "opencode" => Ok(Self::Opencode),
            "opencode-go" => Ok(Self::OpencodeGo),
            "bedrock" => Ok(Self::Bedrock),
            "azure-ai" => Ok(Self::AzureAi),
            other => {
                let known: Vec<&str> = Self::ALL.iter().map(|provider| provider.as_str()).collect();
                Err(format!("unsupported provider: {other} (expected one of {})", known.join(", ")))
            }
        }
    }
}

/// The host of `url` in lowercase, without scheme, user information, port or path; the
/// endpoint a client sends to decides which provider-native tools it serves
/// (spec 3.1 §7.20).
#[cfg(any(feature = "anthropic", feature = "openai"))]
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = host.split(':').next()?.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::ModelProvider;
    use std::collections::BTreeSet;
    use std::str::FromStr;

    #[cfg(any(feature = "anthropic", feature = "openai"))]
    #[test]
    fn url_host_is_the_lowercase_host_alone() {
        use super::url_host;
        assert_eq!(
            url_host("https://API.DeepSeek.com/anthropic").as_deref(),
            Some("api.deepseek.com")
        );
        assert_eq!(url_host("https://api.openai.com/v1").as_deref(), Some("api.openai.com"));
        assert_eq!(url_host("http://user@localhost:8080/v1").as_deref(), Some("localhost"));
        assert_eq!(url_host("https://api.anthropic.com.").as_deref(), Some("api.anthropic.com"));
        assert_eq!(url_host("https://"), None);
    }

    #[test]
    fn provider_roundtrips_from_machine_name() {
        for provider in ModelProvider::all() {
            let parsed = ModelProvider::from_str(provider.as_str()).expect("provider should parse");
            assert_eq!(*provider, parsed);
            assert_eq!(provider.to_string(), provider.as_str());
        }
    }

    #[test]
    fn an_unknown_provider_lists_the_known_ones() {
        assert_eq!(
            ModelProvider::from_str("xai"),
            Err("unsupported provider: xai (expected one of gemini, openai, anthropic, deepseek, \
                 groq, ollama, muse, openrouter, opencode, opencode-go, bedrock, azure-ai)"
                .to_string())
        );
    }

    #[test]
    fn every_provider_names_its_feature_key_and_default() {
        use crate::catalog::*;
        let metadata: Vec<_> = ModelProvider::all()
            .iter()
            .map(|p| {
                (
                    p.as_str(),
                    p.feature(),
                    p.env_var(),
                    p.alt_env_var(),
                    p.requires_key(),
                    p.default_model(),
                )
            })
            .collect();
        assert_eq!(
            metadata,
            [
                (
                    "gemini",
                    "gemini",
                    "GOOGLE_API_KEY",
                    Some("GEMINI_API_KEY"),
                    true,
                    Some(GEMINI_DEFAULT)
                ),
                ("openai", "openai", "OPENAI_API_KEY", None, true, Some(OPENAI_DEFAULT)),
                (
                    "anthropic",
                    "anthropic",
                    "ANTHROPIC_API_KEY",
                    None,
                    true,
                    Some(ANTHROPIC_DEFAULT)
                ),
                ("deepseek", "deepseek", "DEEPSEEK_API_KEY", None, true, Some(DEEPSEEK_DEFAULT)),
                ("groq", "groq", "GROQ_API_KEY", None, true, Some(GROQ_DEFAULT)),
                ("ollama", "ollama", "", None, false, Some(OLLAMA_DEFAULT)),
                ("muse", "muse", "META_API_KEY", Some("MODEL_API_KEY"), true, Some(MUSE_DEFAULT)),
                (
                    "openrouter",
                    "openrouter",
                    "OPENROUTER_API_KEY",
                    None,
                    true,
                    Some(OPENROUTER_DEFAULT)
                ),
                ("opencode", "opencode", "OPENCODE_API_KEY", None, true, Some(OPENCODE_DEFAULT)),
                ("opencode-go", "opencode", "OPENCODE_API_KEY", None, true, Some(OPENCODE_DEFAULT)),
                ("bedrock", "bedrock", "AWS_BEARER_TOKEN_BEDROCK", None, false, None),
                ("azure-ai", "azure-ai", "AZURE_AI_API_KEY", None, true, None),
            ]
        );
    }

    #[test]
    fn provider_defaults_are_current_catalog_entries() {
        for provider in ModelProvider::all() {
            let name = provider.as_str();
            assert_eq!(provider.default_model(), crate::catalog::recommended_model(name), "{name}");
            let Some(model) = provider.default_model() else {
                assert!(crate::catalog::requires_explicit_model(name), "{name}");
                continue;
            };
            let entry = crate::catalog::lookup_model(name, model)
                .expect("provider default should be catalogued");
            assert_eq!(entry.lifecycle, crate::catalog::ModelLifecycle::Active);
        }
    }

    /// The variants and the provider features cannot drift apart: `all-providers` in this
    /// crate's manifest lists exactly the features the variants belong to.
    #[test]
    fn variants_cover_every_provider_feature() {
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        let listed = manifest
            .lines()
            .find_map(|line| line.trim().strip_prefix("all-providers = ["))
            .expect("adk-model declares the all-providers feature on one line");
        let features: BTreeSet<&str> = listed
            .trim_end_matches(']')
            .split(',')
            .map(|name| name.trim().trim_matches('"'))
            .filter(|name| !name.is_empty())
            .collect();
        let covered: BTreeSet<&str> =
            ModelProvider::all().iter().map(|provider| provider.feature()).collect();
        assert_eq!(covered, features);
    }
}
