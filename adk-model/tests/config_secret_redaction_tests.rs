//! Provider configs never expose their API key through `Debug` or `Serialize`.
#![cfg(any(
    feature = "openai",
    feature = "anthropic",
    feature = "groq",
    feature = "deepseek",
    feature = "openrouter",
    feature = "azure-ai"
))]

const SECRET: &str = "sk-test-secret-value-1234";

/// Asserts that neither `Debug` output nor JSON serialization contains the key,
/// and that the serialized form omits the `api_key` field.
fn assert_redacted<T>(config: &T)
where
    T: std::fmt::Debug + serde::Serialize,
{
    let debug = format!("{config:?}");
    assert!(!debug.contains(SECRET), "api key leaked through Debug: {debug}");
    assert!(debug.contains("[REDACTED]"), "Debug should mark the redacted key: {debug}");

    let json = serde_json::to_value(config).expect("config serializes");
    assert!(!json.to_string().contains(SECRET), "api key leaked through Serialize: {json}");
    assert!(json.get("api_key").is_none(), "api_key should be omitted: {json}");
}

#[cfg(feature = "openai")]
#[test]
fn openai_configs_redact_api_key() {
    use adk_model::openai::OpenAIResponsesConfig;
    use adk_model::{AzureConfig, OpenAICompatibleConfig, OpenAIConfig};

    assert_redacted(&OpenAIConfig::new(SECRET, "gpt-5-mini"));
    assert_redacted(&AzureConfig::new(SECRET, "https://example.openai.azure.com", "2024", "dep"));
    assert_redacted(&OpenAIResponsesConfig::new(SECRET, "o3"));
    assert_redacted(&OpenAICompatibleConfig::new(SECRET, "model"));
}

#[cfg(feature = "anthropic")]
#[test]
fn anthropic_config_redacts_api_key() {
    assert_redacted(&adk_model::anthropic::AnthropicConfig::new(SECRET, "claude-sonnet-4-6"));
}

#[cfg(feature = "groq")]
#[test]
fn groq_config_redacts_api_key() {
    assert_redacted(&adk_model::GroqConfig::new(SECRET, "llama"));
}

#[cfg(feature = "deepseek")]
#[test]
fn deepseek_config_redacts_api_key() {
    assert_redacted(&adk_model::DeepSeekConfig::new(SECRET, "deepseek-v4-flash"));
}

#[cfg(feature = "openrouter")]
#[test]
fn openrouter_config_redacts_api_key() {
    assert_redacted(&adk_model::OpenRouterConfig::new(SECRET, "openai/gpt-5"));
}

#[cfg(feature = "azure-ai")]
#[test]
fn azure_ai_config_redacts_api_key() {
    assert_redacted(&adk_model::AzureAIConfig::new("https://example.ai.azure.com", SECRET, "m"));
}

#[cfg(feature = "groq")]
#[test]
fn config_without_api_key_still_deserializes() {
    let config: adk_model::GroqConfig =
        serde_json::from_value(serde_json::json!({"model": "llama"})).expect("config parses");
    assert_eq!(config.api_key, "");
    assert_eq!(config.model, "llama");
}
