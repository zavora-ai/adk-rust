use adk_gemini::generation::model::{GenerationConfig, ThinkingConfig, ThinkingLevel};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn arb_thinking_level() -> impl Strategy<Value = ThinkingLevel> {
    prop_oneof![
        Just(ThinkingLevel::Minimal),
        Just(ThinkingLevel::Low),
        Just(ThinkingLevel::Medium),
        Just(ThinkingLevel::High),
    ]
}

// ---------------------------------------------------------------------------
// ThinkingConfig property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: pro-hardening, Property: ThinkingConfig rejects dual mode**
    /// *For any* (budget, level) pair, setting both must return Err.
    /// **Validates: Requirements 2.1, 3.1, 3.2**
    #[test]
    fn prop_thinking_config_rejects_dual_mode(budget in any::<i32>(), level in arb_thinking_level()) {
        let config = ThinkingConfig {
            thinking_budget: Some(budget),
            include_thoughts: None,
            thinking_level: Some(level),
        };
        prop_assert!(config.validate().is_err(), "expected Err when both budget and level are set");
    }

    /// **Feature: pro-hardening, Property: ThinkingConfig accepts single mode**
    /// *For any* budget-only or level-only config, validate must return Ok.
    /// **Validates: Requirements 3.1, 3.2**
    #[test]
    fn prop_thinking_config_accepts_single_mode(
        budget in any::<i32>(),
        level in arb_thinking_level(),
        use_budget in any::<bool>(),
    ) {
        let config = if use_budget {
            ThinkingConfig {
                thinking_budget: Some(budget),
                include_thoughts: None,
                thinking_level: None,
            }
        } else {
            ThinkingConfig {
                thinking_budget: None,
                include_thoughts: None,
                thinking_level: Some(level),
            }
        };
        prop_assert!(config.validate().is_ok(), "expected Ok when only one mode is set");
    }

    /// **Feature: pro-hardening, Property: ThinkingConfig accepts empty**
    /// All None fields must return Ok.
    /// **Validates: Requirements 3.4**
    #[test]
    fn prop_thinking_config_accepts_empty(_ in 0..100u32) {
        let config = ThinkingConfig {
            thinking_budget: None,
            include_thoughts: None,
            thinking_level: None,
        };
        prop_assert!(config.validate().is_ok(), "expected Ok when all fields are None");
    }
}

// ---------------------------------------------------------------------------
// GenerationConfig property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: pro-hardening, Property: GenerationConfig rejects invalid temperature**
    /// *For any* t outside [0.0, 2.0], validate must return Err.
    /// **Validates: Requirements 2.2**
    #[test]
    fn prop_generation_config_rejects_invalid_temperature(t in prop_oneof![
        (f32::MIN..0.0f32),
        // Exclude exact 2.0 boundary — use values strictly above 2.0
        (2.001f32..f32::MAX),
    ]) {
        let config = GenerationConfig {
            temperature: Some(t),
            ..Default::default()
        };
        prop_assert!(config.validate().is_err(), "expected Err for temperature {t}");
    }

    /// **Feature: pro-hardening, Property: GenerationConfig rejects invalid top_p**
    /// *For any* p outside [0.0, 1.0], validate must return Err.
    /// **Validates: Requirements 2.3**
    #[test]
    fn prop_generation_config_rejects_invalid_top_p(p in prop_oneof![
        (f32::MIN..0.0f32),
        (1.001f32..f32::MAX),
    ]) {
        let config = GenerationConfig {
            top_p: Some(p),
            ..Default::default()
        };
        prop_assert!(config.validate().is_err(), "expected Err for top_p {p}");
    }

    /// **Feature: pro-hardening, Property: GenerationConfig rejects invalid top_k**
    /// *For any* k <= 0, validate must return Err.
    /// **Validates: Requirements 2.4**
    #[test]
    fn prop_generation_config_rejects_invalid_top_k(k in i32::MIN..=0) {
        let config = GenerationConfig {
            top_k: Some(k),
            ..Default::default()
        };
        prop_assert!(config.validate().is_err(), "expected Err for top_k {k}");
    }

    /// **Feature: pro-hardening, Property: GenerationConfig rejects invalid max_tokens**
    /// *For any* m <= 0, validate must return Err.
    /// **Validates: Requirements 2.5**
    #[test]
    fn prop_generation_config_rejects_invalid_max_tokens(m in i32::MIN..=0) {
        let config = GenerationConfig {
            max_output_tokens: Some(m),
            ..Default::default()
        };
        prop_assert!(config.validate().is_err(), "expected Err for max_output_tokens {m}");
    }

    /// **Feature: pro-hardening, Property: GenerationConfig accepts valid**
    /// *For any* values within valid ranges, validate must return Ok.
    /// **Validates: Requirements 3.3**
    #[test]
    fn prop_generation_config_accepts_valid(
        t in 0.0f32..=2.0f32,
        p in 0.0f32..=1.0f32,
        k in 1..=10000i32,
        m in 1..=100000i32,
    ) {
        let config = GenerationConfig {
            temperature: Some(t),
            top_p: Some(p),
            top_k: Some(k),
            max_output_tokens: Some(m),
            ..Default::default()
        };
        prop_assert!(config.validate().is_ok(), "expected Ok for valid config");
    }

    /// **Feature: pro-hardening, Property: GenerationConfig accepts all None**
    /// All None fields must return Ok.
    /// **Validates: Requirements 3.4**
    #[test]
    fn prop_generation_config_accepts_all_none(_ in 0..100u32) {
        let config = GenerationConfig::default();
        prop_assert!(config.validate().is_ok(), "expected Ok when all fields are None");
    }
}
