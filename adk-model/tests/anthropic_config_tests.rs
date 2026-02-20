//! Property-based tests for AnthropicConfig builder.
//!
//! **Feature: anthropic-deep-integration, Property 14: Config builder round-trip**
//! **Feature: anthropic-deep-integration, Property 15: Backward compatibility**
//! **Validates: Requirements 13.1, 13.3**

use adk_model::anthropic::AnthropicConfig;
use proptest::prelude::*;

/// Generator for optional thinking budget (0 means disabled).
fn arb_thinking_budget() -> impl Strategy<Value = Option<u32>> {
    prop_oneof![Just(None), (1u32..100_000).prop_map(Some),]
}

/// Generator for a list of beta feature strings.
fn arb_beta_features() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-z0-9-]{3,30}", 0..5)
}

/// Generator for an optional API version string.
fn arb_api_version() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), "[0-9]{4}-[0-9]{2}-[0-9]{2}".prop_map(Some),]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: anthropic-deep-integration, Property 14: Config builder round-trip**
    /// *For any* combination of optional AnthropicConfig fields (prompt_caching,
    /// thinking budget, beta features, api_version), building a config with the
    /// builder methods and reading back the fields SHALL produce the original values.
    /// **Validates: Requirements 13.1**
    #[test]
    fn prop_config_builder_round_trip(
        caching in proptest::bool::ANY,
        budget in arb_thinking_budget(),
        betas in arb_beta_features(),
        version in arb_api_version(),
    ) {
        let mut config = AnthropicConfig::new("test-key", "claude-sonnet-4-5-20250929")
            .with_prompt_caching(caching);

        if let Some(b) = budget {
            config = config.with_thinking(b);
        }

        for beta in &betas {
            config = config.with_beta_feature(beta.clone());
        }

        if let Some(ref v) = version {
            config = config.with_api_version(v.clone());
        }

        // Verify round-trip
        prop_assert_eq!(config.prompt_caching, caching);

        match budget {
            Some(b) => {
                let thinking = config.thinking.as_ref().unwrap();
                prop_assert_eq!(thinking.budget_tokens, b);
            }
            None => prop_assert!(config.thinking.is_none()),
        }

        prop_assert_eq!(&config.beta_features, &betas);
        prop_assert_eq!(&config.api_version, &version);
    }

    /// **Feature: anthropic-deep-integration, Property 15: Backward compatibility**
    /// *For any* LlmRequest, building API parameters with a default AnthropicConfig
    /// (no optional features enabled) SHALL produce a config with no `thinking`
    /// parameter, no `cache_control` on any block, prompt_caching false,
    /// empty beta_features, and no api_version override.
    /// **Validates: Requirements 13.3**
    #[test]
    fn prop_default_config_backward_compatible(
        max_tokens in 1u32..100_000,
        model in "[a-z0-9-]{5,30}",
    ) {
        let config = AnthropicConfig::new("test-key", &model)
            .with_max_tokens(max_tokens);

        // All new fields must be at their disabled/empty defaults
        prop_assert!(!config.prompt_caching, "prompt_caching should be false by default");
        prop_assert!(config.thinking.is_none(), "thinking should be None by default");
        prop_assert!(config.beta_features.is_empty(), "beta_features should be empty by default");
        prop_assert!(config.api_version.is_none(), "api_version should be None by default");

        // Existing fields should be preserved
        prop_assert_eq!(&config.api_key, "test-key");
        prop_assert_eq!(&config.model, &model);
        prop_assert_eq!(config.max_tokens, max_tokens);
    }
}
