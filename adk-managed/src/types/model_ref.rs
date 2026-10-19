//! Model reference types for provider-neutral model declaration.
//!
//! These types enable a `ManagedAgentDef` to specify which model to use
//! without being tied to a specific provider's API.

use serde::{Deserialize, Serialize};

/// Provider-neutral model reference.
///
/// Supports two forms:
/// - **Shorthand**: a plain string like `"gemini-3.7-flash"` or `"gpt-5.6-terra"`
/// - **Structured**: an explicit provider + model config + optional speed hint
///
/// # Examples
///
/// ```rust
/// use adk_managed::types::ModelRef;
///
/// // Shorthand form
/// let json = serde_json::json!("gemini-3.7-flash");
/// let model_ref: ModelRef = serde_json::from_value(json).unwrap();
///
/// // Structured form
/// let json = serde_json::json!({
///     "provider": "openai",
///     "model": "gpt-5.6-terra"
/// });
/// let model_ref: ModelRef = serde_json::from_value(json).unwrap();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelRef {
    /// A plain model name string (e.g. `"gemini-3.7-flash"`).
    Shorthand(String),
    /// A structured model reference with explicit provider.
    Structured {
        /// The LLM provider.
        provider: Provider,
        /// The model identifier or compatible configuration.
        model: ModelConfig,
        /// Optional speed hint (e.g. `"fast"`, `"balanced"`).
        #[serde(skip_serializing_if = "Option::is_none")]
        speed: Option<String>,
    },
}

/// Supported LLM providers.
///
/// Serializes to/from lowercase snake_case strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Google Gemini.
    Gemini,
    /// OpenAI (GPT family).
    Openai,
    /// Anthropic (Claude family).
    Anthropic,
    /// Ollama (local models).
    Ollama,
    /// OpenAI-compatible endpoint with custom base URL.
    OpenaiCompatible,
}

/// Model configuration — either a simple name or a full compatible endpoint config.
///
/// # Examples
///
/// ```rust
/// use adk_managed::types::ModelConfig;
///
/// // Simple name
/// let json = serde_json::json!("gpt-5.6-terra");
/// let config: ModelConfig = serde_json::from_value(json).unwrap();
///
/// // Compatible endpoint
/// let json = serde_json::json!({
///     "model": "deepseek-chat",
///     "base_url": "https://api.deepseek.com/v1",
///     "api_key": "sk-xxx"
/// });
/// let config: ModelConfig = serde_json::from_value(json).unwrap();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelConfig {
    /// A simple model name string.
    Name(String),
    /// A full compatible endpoint configuration with model, base URL, and API key.
    Compatible {
        /// The model identifier.
        model: String,
        /// The base URL for the compatible API endpoint.
        base_url: String,
        /// The resolved API key (plaintext — platform resolves refs before passing to runtime).
        api_key: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shorthand_parses() {
        let json = serde_json::json!("gemini-2.5-flash");
        let model_ref: ModelRef = serde_json::from_value(json).unwrap();

        match model_ref {
            ModelRef::Shorthand(name) => assert_eq!(name, "gemini-2.5-flash"),
            _ => panic!("expected Shorthand variant"),
        }
    }

    #[test]
    fn test_shorthand_round_trip() {
        let original = ModelRef::Shorthand("gpt-4.1".to_string());
        let json = serde_json::to_value(&original).unwrap();
        assert_eq!(json, serde_json::json!("gpt-4.1"));

        let deserialized: ModelRef = serde_json::from_value(json).unwrap();
        match deserialized {
            ModelRef::Shorthand(name) => assert_eq!(name, "gpt-4.1"),
            _ => panic!("expected Shorthand variant"),
        }
    }

    #[test]
    fn test_structured_parses() {
        let json = serde_json::json!({
            "provider": "openai",
            "model": "gpt-4.1"
        });
        let model_ref: ModelRef = serde_json::from_value(json).unwrap();

        match model_ref {
            ModelRef::Structured { provider, model, speed } => {
                assert_eq!(provider, Provider::Openai);
                match model {
                    ModelConfig::Name(name) => assert_eq!(name, "gpt-4.1"),
                    _ => panic!("expected Name variant"),
                }
                assert_eq!(speed, None);
            }
            _ => panic!("expected Structured variant"),
        }
    }

    #[test]
    fn test_structured_with_speed() {
        let json = serde_json::json!({
            "provider": "gemini",
            "model": "gemini-2.5-flash",
            "speed": "fast"
        });
        let model_ref: ModelRef = serde_json::from_value(json).unwrap();

        match model_ref {
            ModelRef::Structured { provider, model, speed } => {
                assert_eq!(provider, Provider::Gemini);
                match model {
                    ModelConfig::Name(name) => assert_eq!(name, "gemini-2.5-flash"),
                    _ => panic!("expected Name variant"),
                }
                assert_eq!(speed, Some("fast".to_string()));
            }
            _ => panic!("expected Structured variant"),
        }
    }

    #[test]
    fn test_openai_compatible_with_base_url() {
        let json = serde_json::json!({
            "provider": "openai_compatible",
            "model": {
                "model": "deepseek-chat",
                "base_url": "https://api.deepseek.com/v1",
                "api_key": "sk-test-key-123"
            }
        });
        let model_ref: ModelRef = serde_json::from_value(json).unwrap();

        match model_ref {
            ModelRef::Structured { provider, model, speed } => {
                assert_eq!(provider, Provider::OpenaiCompatible);
                match model {
                    ModelConfig::Compatible { model, base_url, api_key } => {
                        assert_eq!(model, "deepseek-chat");
                        assert_eq!(base_url, "https://api.deepseek.com/v1");
                        assert_eq!(api_key, "sk-test-key-123");
                    }
                    _ => panic!("expected Compatible variant"),
                }
                assert_eq!(speed, None);
            }
            _ => panic!("expected Structured variant"),
        }
    }

    #[test]
    fn test_provider_serialization() {
        assert_eq!(serde_json::to_value(Provider::Gemini).unwrap(), serde_json::json!("gemini"));
        assert_eq!(serde_json::to_value(Provider::Openai).unwrap(), serde_json::json!("openai"));
        assert_eq!(
            serde_json::to_value(Provider::Anthropic).unwrap(),
            serde_json::json!("anthropic")
        );
        assert_eq!(serde_json::to_value(Provider::Ollama).unwrap(), serde_json::json!("ollama"));
        assert_eq!(
            serde_json::to_value(Provider::OpenaiCompatible).unwrap(),
            serde_json::json!("openai_compatible")
        );
    }

    #[test]
    fn test_model_config_name() {
        let json = serde_json::json!("claude-3.5-sonnet");
        let config: ModelConfig = serde_json::from_value(json).unwrap();

        match config {
            ModelConfig::Name(name) => assert_eq!(name, "claude-3.5-sonnet"),
            _ => panic!("expected Name variant"),
        }
    }

    #[test]
    fn test_model_config_compatible() {
        let json = serde_json::json!({
            "model": "local-llama",
            "base_url": "http://localhost:11434/v1",
            "api_key": "ollama"
        });
        let config: ModelConfig = serde_json::from_value(json).unwrap();

        match config {
            ModelConfig::Compatible { model, base_url, api_key } => {
                assert_eq!(model, "local-llama");
                assert_eq!(base_url, "http://localhost:11434/v1");
                assert_eq!(api_key, "ollama");
            }
            _ => panic!("expected Compatible variant"),
        }
    }

    #[test]
    fn test_structured_speed_omitted_in_serialization() {
        let model_ref = ModelRef::Structured {
            provider: Provider::Anthropic,
            model: ModelConfig::Name("claude-3.5-sonnet".to_string()),
            speed: None,
        };
        let json = serde_json::to_value(&model_ref).unwrap();
        assert!(!json.as_object().unwrap().contains_key("speed"));
    }
}
