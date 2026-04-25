use std::fmt::{Display, Formatter};
use std::str::FromStr;

/// Canonical provider identifiers and metadata shared across ADK crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelProvider {
    Gemini,
    Openai,
    Anthropic,
    Deepseek,
    Groq,
    Ollama,
}

impl ModelProvider {
    /// All providers in UI/display order.
    pub const ALL: [Self; 6] =
        [Self::Gemini, Self::Openai, Self::Anthropic, Self::Deepseek, Self::Groq, Self::Ollama];

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
        }
    }

    /// Default model for the provider.
    pub const fn default_model(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-3.1-flash-lite-preview",
            Self::Openai => "gpt-5-mini",
            Self::Anthropic => "claude-sonnet-4-5-20250929",
            Self::Deepseek => "deepseek-v4-flash",
            Self::Groq => "llama-3.3-70b-versatile",
            Self::Ollama => "qwen3.5",
        }
    }

    /// Primary environment variable used for the provider API key.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Gemini => "GOOGLE_API_KEY",
            Self::Openai => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Deepseek => "DEEPSEEK_API_KEY",
            Self::Groq => "GROQ_API_KEY",
            Self::Ollama => "",
        }
    }

    /// Alternate environment variable used for the provider API key.
    pub const fn alt_env_var(self) -> Option<&'static str> {
        match self {
            Self::Gemini => Some("GEMINI_API_KEY"),
            _ => None,
        }
    }

    /// Whether the provider requires an API key.
    pub const fn requires_key(self) -> bool {
        !matches!(self, Self::Ollama)
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
            other => Err(format!("unsupported provider: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModelProvider;
    use std::str::FromStr;

    #[test]
    fn provider_roundtrips_from_machine_name() {
        for provider in ModelProvider::all() {
            let parsed = ModelProvider::from_str(provider.as_str()).expect("provider should parse");
            assert_eq!(*provider, parsed);
        }
    }
}
