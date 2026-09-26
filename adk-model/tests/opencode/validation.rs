use super::*;
use adk_core::ErrorCategory;
use adk_model::openai::OpenAIReasoningEffort;
use proptest::prelude::*;

#[test]
fn requires_explicit_api_for_unknown_models() {
    let error = OpenCodeClient::new(config("future-model")).err().unwrap();
    assert_eq!(error.code, "model.opencode.invalid_config");
    assert!(OpenCodeClient::new(config("future-model").with_api(OpenCodeApi::Messages)).is_ok());
}

#[test]
fn validates_identity_endpoint_and_reasoning_without_requests() {
    for config in [
        OpenCodeConfig::new(OpenCodeService::Go, "secret", "minimax-m3"),
        config("minimax-m3").with_session_id("\ninvalid"),
        config("minimax-m3").with_user_agent(""),
        config("minimax-m3").with_reasoning_effort(OpenAIReasoningEffort::High),
        config("deepseek-v4.1-flash").with_anthropic_effort(adk_model::anthropic::Effort::High),
        config("deepseek-v4.1-flash").with_base_url("http://example.com/v1"),
        config("deepseek-v4.1-flash").with_base_url("https://opencode.ai/zen/go/v1?secret=secret"),
    ] {
        let error = OpenCodeClient::new(config).err().unwrap();
        assert_eq!(error.category, ErrorCategory::InvalidInput);
        assert!(!error.to_string().contains("secret"));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn debug_omits_credentials(key in "[a-zA-Z0-9]{30,60}") {
        let config = OpenCodeConfig::new(OpenCodeService::Go, &key, "minimax-m3");
        let debug = format!("{config:?}");
        prop_assert!(!debug.contains(&key));
    }
}

#[test]
fn separates_service_routes_and_defaults() {
    for model in ["minimax-m3", "qwen3.8-max"] {
        assert_eq!(OpenCodeService::Go.api(model), Some(OpenCodeApi::Messages));
        assert_eq!(OpenCodeService::Zen.api(model), Some(OpenCodeApi::ChatCompletions));
    }
    assert_eq!(OpenCodeService::Go.base_url(), "https://opencode.ai/zen/go/v1");
    assert_eq!(OpenCodeService::Zen.base_url(), "https://opencode.ai/zen/v1");
    assert_eq!(OpenCodeService::Go.api("gemini-3.8-flash"), None);
    assert_eq!(OpenCodeService::Zen.api("gemini-3.8-flash"), Some(OpenCodeApi::GenerateContent));
    assert_eq!(OpenCodeService::Zen.api("jev-1.13"), None);
    assert!(OpenCodeClient::new(config_for(OpenCodeService::Zen, "unknown")).is_err());
    for config in [
        config_for(OpenCodeService::Zen, "gemini-3.8-flash")
            .with_reasoning_effort(OpenAIReasoningEffort::High),
        config("deepseek-v4.1-flash")
            .with_gemini_thinking(adk_model::gemini::ThinkingConfig::default()),
    ] {
        assert!(OpenCodeClient::new(config).is_err());
    }
}
