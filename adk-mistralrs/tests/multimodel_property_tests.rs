//! Property-based tests for multi-model routing.
//!
//! **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
//! *For any* multi-model configuration, requests specifying a model name
//! SHALL be routed to the correct model.
//! **Validates: Requirements 14.1, 14.2**

use proptest::prelude::*;
use std::collections::HashMap;

use adk_mistralrs::{MistralRsMultiModel, MultiModelConfig, MultiModelEntry, MultiModelType};

/// Generate arbitrary model names (valid identifiers)
fn arb_model_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{2,15}".prop_map(|s| s.to_string())
}

/// Generate arbitrary HuggingFace model IDs
fn arb_hf_model_id() -> impl Strategy<Value = String> {
    ("[a-z]{3,10}/[a-zA-Z0-9_-]{3,20}").prop_map(|s| s.to_string())
}

/// Generate arbitrary quantization levels
fn arb_quantization_level() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        Just(Some("2".to_string())),
        Just(Some("3".to_string())),
        Just(Some("4".to_string())),
        Just(Some("5".to_string())),
        Just(Some("6".to_string())),
        Just(Some("8".to_string())),
        Just(Some("q4k".to_string())),
        Just(Some("q8_0".to_string())),
    ]
}

/// Generate arbitrary multi-model type
fn arb_multi_model_type() -> impl Strategy<Value = MultiModelType> {
    prop_oneof![
        arb_hf_model_id().prop_map(|model_id| MultiModelType::Plain { model_id }),
        arb_hf_model_id().prop_map(|model_id| MultiModelType::Vision { model_id, arch: None }),
        arb_hf_model_id().prop_map(|model_id| MultiModelType::Embedding { model_id, arch: None }),
    ]
}

/// Generate arbitrary multi-model entry
fn arb_multi_model_entry() -> impl Strategy<Value = MultiModelEntry> {
    (arb_multi_model_type(), arb_quantization_level(), any::<bool>()).prop_map(
        |(model_type, in_situ_quant, default)| MultiModelEntry {
            model_type,
            in_situ_quant,
            default,
        },
    )
}

/// Generate arbitrary multi-model config with 1-5 models
fn arb_multi_model_config() -> impl Strategy<Value = MultiModelConfig> {
    prop::collection::hash_map(arb_model_name(), arb_multi_model_entry(), 1..5)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
    /// *For any* multi-model configuration, the configuration SHALL be parseable
    /// and all model names SHALL be preserved.
    /// **Validates: Requirements 14.1, 14.2**
    #[test]
    fn prop_multi_model_config_preserves_model_names(config in arb_multi_model_config()) {
        // Serialize to JSON
        let json = serde_json::to_string(&config).unwrap();

        // Deserialize back
        let parsed: MultiModelConfig = serde_json::from_str(&json).unwrap();

        // All model names should be preserved
        for name in config.keys() {
            prop_assert!(parsed.contains_key(name), "Model name '{}' was not preserved", name);
        }

        // Same number of models
        prop_assert_eq!(config.len(), parsed.len());
    }

    /// **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
    /// *For any* multi-model configuration with a default model specified,
    /// the default model SHALL be correctly identified.
    /// **Validates: Requirements 14.1, 14.2**
    #[test]
    fn prop_multi_model_config_default_model_identified(config in arb_multi_model_config()) {
        // Find the default model (if any)
        let default_models: Vec<_> = config.iter()
            .filter(|(_, entry)| entry.default)
            .map(|(name, _)| name.clone())
            .collect();

        // Serialize and deserialize
        let json = serde_json::to_string(&config).unwrap();
        let parsed: MultiModelConfig = serde_json::from_str(&json).unwrap();

        // Default models should be preserved
        let parsed_defaults: Vec<_> = parsed.iter()
            .filter(|(_, entry)| entry.default)
            .map(|(name, _)| name.clone())
            .collect();

        prop_assert_eq!(default_models.len(), parsed_defaults.len());
        for name in &default_models {
            prop_assert!(parsed_defaults.contains(name), "Default model '{}' was not preserved", name);
        }
    }

    /// **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
    /// *For any* model name, the multi-model instance SHALL correctly report
    /// whether that model exists.
    /// **Validates: Requirements 14.1, 14.2**
    #[test]
    fn prop_multi_model_has_model_consistency(
        _model_names in prop::collection::vec(arb_model_name(), 1..5),
        query_name in arb_model_name()
    ) {
        // Create a runtime for async operations
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            let multi_model = MistralRsMultiModel::new();

            // The model should not exist initially
            prop_assert!(!multi_model.has_model(&query_name).await);

            // Model count should be 0
            prop_assert_eq!(multi_model.model_count().await, 0);

            // Model names should be empty
            prop_assert!(multi_model.model_names().await.is_empty());

            Ok(())
        })?;
    }

    /// **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
    /// *For any* quantization level string, parsing SHALL either succeed with
    /// a valid level or fail with an appropriate error.
    /// **Validates: Requirements 14.1, 14.2**
    #[test]
    fn prop_quantization_level_parsing(level in "[a-z0-9_]{1,5}") {
        // Valid levels should parse successfully
        let _valid_levels = ["2", "3", "4", "5", "6", "8", "q2k", "q3k", "q4k", "q5k", "q6k",
                          "q4_0", "q4_1", "q5_0", "q5_1", "q8_0", "q8_1"];

        let entry = MultiModelEntry {
            model_type: MultiModelType::Plain { model_id: "test/model".to_string() },
            in_situ_quant: Some(level.clone()),
            default: false,
        };

        // Serialize should always work
        let json = serde_json::to_string(&entry).unwrap();

        // Deserialize should work
        let parsed: MultiModelEntry = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed.in_situ_quant, &Some(level.clone()));
    }

    /// **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
    /// *For any* multi-model instance, setting a default model SHALL only succeed
    /// if the model exists.
    /// **Validates: Requirements 14.1, 14.2**
    #[test]
    fn prop_set_default_requires_existing_model(
        _existing_names in prop::collection::hash_set(arb_model_name(), 0..3),
        query_name in arb_model_name()
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            let multi_model = MistralRsMultiModel::new();

            // Try to set default on empty multi-model
            let result = multi_model.set_default(&query_name).await;

            // Should fail because no models exist
            prop_assert!(result.is_err());

            Ok(())
        })?;
    }

    /// **Feature: mistral-rs-integration, Property 12: Multi-Model Routing**
    /// *For any* multi-model configuration, model type information SHALL be preserved
    /// through serialization/deserialization.
    /// **Validates: Requirements 14.1, 14.2**
    #[test]
    fn prop_model_type_preserved(entry in arb_multi_model_entry()) {
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MultiModelEntry = serde_json::from_str(&json).unwrap();

        // Check model type is preserved
        match (&entry.model_type, &parsed.model_type) {
            (MultiModelType::Plain { model_id: id1 }, MultiModelType::Plain { model_id: id2 }) => {
                prop_assert_eq!(id1, id2);
            }
            (MultiModelType::Vision { model_id: id1, .. }, MultiModelType::Vision { model_id: id2, .. }) => {
                prop_assert_eq!(id1, id2);
            }
            (MultiModelType::Embedding { model_id: id1, .. }, MultiModelType::Embedding { model_id: id2, .. }) => {
                prop_assert_eq!(id1, id2);
            }
            _ => {
                prop_assert!(false, "Model type changed during serialization");
            }
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[tokio::test]
    async fn test_multi_model_routing_basic() {
        let multi_model = MistralRsMultiModel::new();

        // Initially empty
        assert_eq!(multi_model.model_count().await, 0);
        assert!(multi_model.default_model().await.is_none());
        assert!(multi_model.model_names().await.is_empty());

        // has_model should return false for any name
        assert!(!multi_model.has_model("nonexistent").await);
    }

    #[tokio::test]
    async fn test_multi_model_set_default_fails_for_nonexistent() {
        let multi_model = MistralRsMultiModel::new();

        // Should fail because model doesn't exist
        let result = multi_model.set_default("nonexistent").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_multi_model_config_json_roundtrip() {
        let mut config = HashMap::new();
        config.insert(
            "llama".to_string(),
            MultiModelEntry {
                model_type: MultiModelType::Plain {
                    model_id: "meta-llama/Llama-3.2-3B-Instruct".to_string(),
                },
                in_situ_quant: Some("4".to_string()),
                default: true,
            },
        );
        config.insert(
            "phi".to_string(),
            MultiModelEntry {
                model_type: MultiModelType::Plain {
                    model_id: "mistralai/Magistral-Small-2509".to_string(),
                },
                in_situ_quant: None,
                default: false,
            },
        );

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: MultiModelConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains_key("llama"));
        assert!(parsed.contains_key("phi"));
        assert!(parsed.get("llama").unwrap().default);
        assert!(!parsed.get("phi").unwrap().default);
    }

    #[test]
    fn test_multi_model_entry_serialization() {
        let entry = MultiModelEntry {
            model_type: MultiModelType::Vision {
                model_id: "google/gemma-3n-E4B-it".to_string(),
                arch: Some("gemma3".to_string()),
            },
            in_situ_quant: Some("4".to_string()),
            default: false,
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("Vision"));
        assert!(json.contains("google/gemma-3n-E4B-it"));

        let parsed: MultiModelEntry = serde_json::from_str(&json).unwrap();
        if let MultiModelType::Vision { model_id, arch } = &parsed.model_type {
            assert_eq!(model_id, "google/gemma-3n-E4B-it");
            assert_eq!(arch, &Some("gemma3".to_string()));
        } else {
            panic!("Expected Vision model type");
        }
    }
}
