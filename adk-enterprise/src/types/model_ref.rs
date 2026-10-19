//! ModelRef — provider-neutral model reference.
//!
//! Supports shorthand string (e.g., `"gemini-3.7-flash"`) or a structured
//! object with explicit provider, model config, and optional speed hint.

use serde::{Deserialize, Serialize};

/// A provider-neutral model reference.
///
/// Supports shorthand string (e.g., `"gemini-3.7-flash"`) or a structured
/// object with provider and model details.
///
/// Uses `#[serde(untagged)]` so that a plain JSON string deserializes to
/// `Shorthand`, while an object with `provider` + `model` fields
/// deserializes to `Structured`.
///
/// # Examples
///
/// ```rust
/// use adk_enterprise::ModelRef;
///
/// // Shorthand from &str
/// let m: ModelRef = "gemini-3.7-flash".into();
///
/// // Structured
/// use adk_enterprise::Provider;
/// let m = ModelRef::structured(Provider::Openai, "gpt-5.6-terra");
///
/// // Compatible endpoint
/// let m = ModelRef::compatible("deepseek-chat", "https://api.deepseek.com");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ModelRef {
    /// Shorthand model name (e.g., `"gemini-3.7-flash"`, `"gpt-5.6-terra"`).
    Shorthand(String),
    /// Structured model reference with provider details.
    Structured {
        /// The LLM provider.
        provider: Provider,
        /// Model configuration (name or compatible endpoint).
        model: ModelConfig,
        /// Optional speed hint for routing (e.g., `"fast"`, `"balanced"`).
        #[serde(skip_serializing_if = "Option::is_none")]
        speed: Option<String>,
    },
}

impl Default for ModelRef {
    fn default() -> Self {
        Self::Shorthand(String::new())
    }
}

impl From<&str> for ModelRef {
    fn from(s: &str) -> Self {
        Self::Shorthand(s.to_string())
    }
}

impl From<String> for ModelRef {
    fn from(s: String) -> Self {
        Self::Shorthand(s)
    }
}

impl ModelRef {
    /// Create a structured model reference with a named model.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_enterprise::{ModelRef, Provider};
    ///
    /// let model_ref = ModelRef::structured(Provider::Openai, "gpt-5.6-terra");
    /// ```
    pub fn structured(provider: Provider, model: impl Into<String>) -> Self {
        Self::Structured { provider, model: ModelConfig::Name(model.into()), speed: None }
    }

    /// Create a structured model reference with a speed hint.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_enterprise::{ModelRef, Provider};
    ///
    /// let model_ref = ModelRef::structured_with_speed(
    ///     Provider::Gemini,
    ///     "gemini-3.7-flash",
    ///     "fast",
    /// );
    /// ```
    pub fn structured_with_speed(
        provider: Provider,
        model: impl Into<String>,
        speed: impl Into<String>,
    ) -> Self {
        Self::Structured {
            provider,
            model: ModelConfig::Name(model.into()),
            speed: Some(speed.into()),
        }
    }

    /// Create a model reference for an OpenAI-compatible endpoint.
    ///
    /// Uses `Provider::OpenaiCompatible` and `ModelConfig::Compatible` with
    /// the given model name and base URL.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_enterprise::ModelRef;
    ///
    /// let model_ref = ModelRef::compatible(
    ///     "deepseek-chat",
    ///     "https://api.deepseek.com",
    /// );
    /// ```
    pub fn compatible(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::Structured {
            provider: Provider::OpenaiCompatible,
            model: ModelConfig::Compatible {
                model: model.into(),
                base_url: base_url.into(),
                api_key: None,
            },
            speed: None,
        }
    }

    /// Create a model reference for an OpenAI-compatible endpoint with an API key.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_enterprise::ModelRef;
    ///
    /// let model_ref = ModelRef::compatible_with_key(
    ///     "deepseek-chat",
    ///     "https://api.deepseek.com",
    ///     "sk-...",
    /// );
    /// ```
    pub fn compatible_with_key(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::Structured {
            provider: Provider::OpenaiCompatible,
            model: ModelConfig::Compatible {
                model: model.into(),
                base_url: base_url.into(),
                api_key: Some(api_key.into()),
            },
            speed: None,
        }
    }
}

/// LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    /// Google Gemini.
    Gemini,
    /// OpenAI.
    Openai,
    /// Anthropic.
    Anthropic,
    /// Ollama (local).
    Ollama,
    /// Any OpenAI-compatible endpoint.
    OpenaiCompatible,
}

/// Model configuration within a structured reference.
///
/// Either a simple model name string, or a compatible endpoint
/// with custom base URL and optional API key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ModelConfig {
    /// Simple model name (e.g., `"gpt-5.6-terra"`).
    Name(String),
    /// Compatible endpoint with custom base URL and optional API key.
    Compatible {
        /// The model identifier at the compatible endpoint.
        model: String,
        /// Base URL of the compatible API.
        base_url: String,
        /// Optional API key for the compatible endpoint.
        #[serde(skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Shorthand round-trip ────────────────────────────────────────

    #[test]
    fn shorthand_serializes_to_string() {
        let model_ref = ModelRef::Shorthand("gemini-2.5-flash".to_string());
        let json = serde_json::to_string(&model_ref).unwrap();
        assert_eq!(json, r#""gemini-2.5-flash""#);
    }

    #[test]
    fn shorthand_deserializes_from_string() {
        let json = r#""gpt-4.1""#;
        let model_ref: ModelRef = serde_json::from_str(json).unwrap();
        assert_eq!(model_ref, ModelRef::Shorthand("gpt-4.1".to_string()));
    }

    #[test]
    fn shorthand_round_trip() {
        let original = ModelRef::from("claude-sonnet-4-6");
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelRef = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    // ─── Structured round-trip ───────────────────────────────────────

    #[test]
    fn structured_serializes_to_object() {
        let model_ref = ModelRef::structured(Provider::Openai, "gpt-4.1");
        let json = serde_json::to_value(&model_ref).unwrap();

        assert_eq!(json["provider"], "openai");
        assert_eq!(json["model"], "gpt-4.1");
        // speed is None, should not be present
        assert!(json.get("speed").is_none());
    }

    #[test]
    fn structured_with_speed_serializes() {
        let model_ref =
            ModelRef::structured_with_speed(Provider::Gemini, "gemini-2.5-flash", "fast");
        let json = serde_json::to_value(&model_ref).unwrap();

        assert_eq!(json["provider"], "gemini");
        assert_eq!(json["model"], "gemini-2.5-flash");
        assert_eq!(json["speed"], "fast");
    }

    #[test]
    fn structured_round_trip() {
        let original = ModelRef::structured(Provider::Anthropic, "claude-sonnet-4-6");
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelRef = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn structured_with_speed_round_trip() {
        let original = ModelRef::structured_with_speed(Provider::Ollama, "llama3", "balanced");
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelRef = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn structured_deserializes_from_object() {
        let json = r#"{"provider":"openai","model":"gpt-4.1"}"#;
        let model_ref: ModelRef = serde_json::from_str(json).unwrap();
        assert_eq!(
            model_ref,
            ModelRef::Structured {
                provider: Provider::Openai,
                model: ModelConfig::Name("gpt-4.1".to_string()),
                speed: None,
            }
        );
    }

    #[test]
    fn structured_with_speed_deserializes_from_object() {
        let json = r#"{"provider":"gemini","model":"gemini-2.5-flash","speed":"fast"}"#;
        let model_ref: ModelRef = serde_json::from_str(json).unwrap();
        assert_eq!(
            model_ref,
            ModelRef::Structured {
                provider: Provider::Gemini,
                model: ModelConfig::Name("gemini-2.5-flash".to_string()),
                speed: Some("fast".to_string()),
            }
        );
    }

    // ─── Compatible round-trip ───────────────────────────────────────

    #[test]
    fn compatible_serializes_to_object() {
        let model_ref = ModelRef::compatible("deepseek-chat", "https://api.deepseek.com");
        let json = serde_json::to_value(&model_ref).unwrap();

        assert_eq!(json["provider"], "openaiCompatible");
        assert_eq!(json["model"]["model"], "deepseek-chat");
        assert_eq!(json["model"]["base_url"], "https://api.deepseek.com");
        // api_key is None, should not be present
        assert!(json["model"].get("api_key").is_none());
    }

    #[test]
    fn compatible_with_key_serializes() {
        let model_ref = ModelRef::compatible_with_key(
            "deepseek-chat",
            "https://api.deepseek.com",
            "sk-test-key",
        );
        let json = serde_json::to_value(&model_ref).unwrap();

        assert_eq!(json["provider"], "openaiCompatible");
        assert_eq!(json["model"]["model"], "deepseek-chat");
        assert_eq!(json["model"]["base_url"], "https://api.deepseek.com");
        assert_eq!(json["model"]["api_key"], "sk-test-key");
    }

    #[test]
    fn compatible_round_trip() {
        let original = ModelRef::compatible("deepseek-chat", "https://api.deepseek.com");
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelRef = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn compatible_with_key_round_trip() {
        let original = ModelRef::compatible_with_key(
            "mixtral-8x7b",
            "https://api.together.xyz",
            "sk-together-key",
        );
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelRef = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn compatible_deserializes_from_object() {
        let json = r#"{"provider":"openaiCompatible","model":{"model":"deepseek-chat","base_url":"https://api.deepseek.com"}}"#;
        let model_ref: ModelRef = serde_json::from_str(json).unwrap();
        assert_eq!(
            model_ref,
            ModelRef::Structured {
                provider: Provider::OpenaiCompatible,
                model: ModelConfig::Compatible {
                    model: "deepseek-chat".to_string(),
                    base_url: "https://api.deepseek.com".to_string(),
                    api_key: None,
                },
                speed: None,
            }
        );
    }

    // ─── Provider serialization ──────────────────────────────────────

    #[test]
    fn provider_serializes_camel_case() {
        assert_eq!(serde_json::to_string(&Provider::Gemini).unwrap(), r#""gemini""#);
        assert_eq!(serde_json::to_string(&Provider::Openai).unwrap(), r#""openai""#);
        assert_eq!(serde_json::to_string(&Provider::Anthropic).unwrap(), r#""anthropic""#);
        assert_eq!(serde_json::to_string(&Provider::Ollama).unwrap(), r#""ollama""#);
        assert_eq!(
            serde_json::to_string(&Provider::OpenaiCompatible).unwrap(),
            r#""openaiCompatible""#
        );
    }

    #[test]
    fn provider_round_trip() {
        let providers = [
            Provider::Gemini,
            Provider::Openai,
            Provider::Anthropic,
            Provider::Ollama,
            Provider::OpenaiCompatible,
        ];
        for provider in &providers {
            let json = serde_json::to_string(provider).unwrap();
            let deserialized: Provider = serde_json::from_str(&json).unwrap();
            assert_eq!(provider, &deserialized);
        }
    }

    // ─── ModelConfig serialization ───────────────────────────────────

    #[test]
    fn model_config_name_serializes_to_string() {
        let config = ModelConfig::Name("gpt-4.1".to_string());
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(json, r#""gpt-4.1""#);
    }

    #[test]
    fn model_config_compatible_serializes_to_object() {
        let config = ModelConfig::Compatible {
            model: "deepseek-chat".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            api_key: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["model"], "deepseek-chat");
        assert_eq!(json["base_url"], "https://api.deepseek.com");
        assert!(json.get("api_key").is_none());
    }

    #[test]
    fn model_config_name_round_trip() {
        let original = ModelConfig::Name("llama3".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn model_config_compatible_round_trip() {
        let original = ModelConfig::Compatible {
            model: "deepseek-chat".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            api_key: Some("sk-key".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    // ─── From impls ─────────────────────────────────────────────────

    #[test]
    fn from_str_creates_shorthand() {
        let model_ref: ModelRef = "gemini-2.5-flash".into();
        assert_eq!(model_ref, ModelRef::Shorthand("gemini-2.5-flash".to_string()));
    }

    #[test]
    fn from_string_creates_shorthand() {
        let model_ref: ModelRef = String::from("gpt-4.1").into();
        assert_eq!(model_ref, ModelRef::Shorthand("gpt-4.1".to_string()));
    }

    // ─── Default ─────────────────────────────────────────────────────

    #[test]
    fn default_is_empty_shorthand() {
        let model_ref = ModelRef::default();
        assert_eq!(model_ref, ModelRef::Shorthand(String::new()));
    }
}
