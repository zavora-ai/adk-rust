//! Tests for OpenAI reasoning effort configuration.

#[cfg(feature = "openai")]
mod openai_reasoning {
    use adk_model::openai::OpenAIConfig;
    use adk_model::openai::ReasoningEffort;

    #[test]
    fn reasoning_effort_serialization_round_trip() {
        for (variant, expected_str) in [
            (ReasoningEffort::Low, "\"low\""),
            (ReasoningEffort::Medium, "\"medium\""),
            (ReasoningEffort::High, "\"high\""),
        ] {
            let serialized = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialized, expected_str);
            let deserialized: ReasoningEffort = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn config_with_reasoning_effort_builder() {
        let config =
            OpenAIConfig::new("test-key", "o3-mini").with_reasoning_effort(ReasoningEffort::High);

        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(config.model, "o3-mini");
    }

    #[test]
    fn config_without_reasoning_effort_is_none() {
        let config = OpenAIConfig::new("test-key", "gpt-5-mini");
        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    fn config_reasoning_effort_serializes_to_json() {
        let config =
            OpenAIConfig::new("test-key", "o3-mini").with_reasoning_effort(ReasoningEffort::Medium);

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["reasoning_effort"], serde_json::json!("medium"));
    }

    #[test]
    fn config_without_reasoning_effort_omits_field() {
        let config = OpenAIConfig::new("test-key", "gpt-5-mini");
        let json = serde_json::to_value(&config).unwrap();
        assert!(json.get("reasoning_effort").is_none());
    }
}
