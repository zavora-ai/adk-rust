//! Property-based tests for YAML Agent Definition schema types.
//!
//! Tests two correctness properties from the design document:
//! - Property 1: YAML Agent Definition Round-Trip
//! - Property 2: YAML Validation Error Identifies Invalid Field

#![cfg(feature = "yaml-agent")]

use std::collections::HashMap;

use proptest::prelude::*;
use serde_json::Value as JsonValue;

use adk_server::yaml_agent::schema::{
    McpToolReference, ModelConfig, SubAgentReference, ToolReference, YamlAgentDefinition,
};

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate a simple alphanumeric identifier (avoids YAML special chars).
fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,15}".prop_map(|s| s)
}

/// Generate an optional description string.
fn arb_optional_string() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), "[a-zA-Z0-9 ._-]{1,50}".prop_map(Some),]
}

/// Generate a simple JSON value (no nested objects to keep YAML round-trip clean).
fn arb_json_value() -> impl Strategy<Value = JsonValue> {
    prop_oneof![
        Just(JsonValue::Null),
        any::<bool>().prop_map(JsonValue::Bool),
        // Use integers to avoid floating-point precision issues in YAML round-trip
        (-1000i64..1000).prop_map(|n| JsonValue::Number(serde_json::Number::from(n))),
        "[a-zA-Z0-9 _-]{0,20}".prop_map(|s| JsonValue::String(s)),
    ]
}

/// Generate a config map with simple JSON values.
fn arb_config_map() -> impl Strategy<Value = HashMap<String, JsonValue>> {
    prop::collection::hash_map(arb_identifier(), arb_json_value(), 0..4)
}

/// Generate a metadata map with keys that do NOT collide with known schema fields.
/// Known fields: name, description, model, instructions, tools, sub_agents, config
fn arb_metadata_map() -> impl Strategy<Value = HashMap<String, JsonValue>> {
    let known_fields =
        ["name", "description", "model", "instructions", "tools", "sub_agents", "config"];
    prop::collection::hash_map("meta_[a-z]{1,10}".prop_map(|s| s), arb_json_value(), 0..3)
        .prop_filter("metadata keys must not collide with known fields", move |m| {
            m.keys().all(|k| !known_fields.contains(&k.as_str()))
        })
}

/// Generate a ModelConfig.
fn arb_model_config() -> impl Strategy<Value = ModelConfig> {
    (
        arb_identifier(),
        arb_identifier(),
        prop_oneof![Just(None), (0.0f64..2.0).prop_map(|t| Some((t * 100.0).round() / 100.0))],
        prop_oneof![Just(None), (1u32..8192).prop_map(Some)],
    )
        .prop_map(|(provider, model_id, temperature, max_tokens)| ModelConfig {
            provider,
            model_id,
            temperature,
            max_tokens,
        })
}

/// Generate a ToolReference.
fn arb_tool_reference() -> impl Strategy<Value = ToolReference> {
    prop_oneof![
        arb_identifier().prop_map(|name| ToolReference::Named { name }),
        (arb_identifier(), prop::collection::vec(arb_identifier(), 0..3)).prop_map(
            |(endpoint, args)| ToolReference::Mcp { mcp: McpToolReference { endpoint, args } }
        ),
    ]
}

/// Generate a SubAgentReference.
fn arb_sub_agent_reference() -> impl Strategy<Value = SubAgentReference> {
    arb_identifier().prop_map(|reference| SubAgentReference { reference })
}

/// Generate a complete YamlAgentDefinition.
fn arb_yaml_agent_definition() -> impl Strategy<Value = YamlAgentDefinition> {
    (
        arb_identifier(),
        arb_optional_string(),
        arb_model_config(),
        arb_optional_string(),
        prop::collection::vec(arb_tool_reference(), 0..3),
        prop::collection::vec(arb_sub_agent_reference(), 0..3),
        arb_config_map(),
        arb_metadata_map(),
    )
        .prop_map(
            |(name, description, model, instructions, tools, sub_agents, config, metadata)| {
                YamlAgentDefinition {
                    name,
                    description,
                    model,
                    instructions,
                    tools,
                    sub_agents,
                    config,
                    metadata,
                }
            },
        )
}

// ---------------------------------------------------------------------------
// Property 1: YAML Agent Definition Round-Trip
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// **Feature: competitive-parity-v070, Property 1: YAML Agent Definition Round-Trip**
    ///
    /// *For any* valid YamlAgentDefinition (including arbitrary unknown fields in
    /// the metadata map), serializing to YAML and then parsing back SHALL produce
    /// an equivalent YamlAgentDefinition where all named fields and all metadata
    /// entries are preserved.
    ///
    /// **Validates: Requirements 1.1, 13.1, 13.2**
    #[test]
    fn prop_yaml_agent_definition_round_trip(def in arb_yaml_agent_definition()) {
        // Serialize to YAML
        let yaml_str = serde_yaml::to_string(&def)
            .expect("serialization to YAML should succeed");

        // Deserialize back
        let round_tripped: YamlAgentDefinition = serde_yaml::from_str(&yaml_str)
            .expect("deserialization from YAML should succeed");

        // Assert equality
        prop_assert_eq!(&def.name, &round_tripped.name);
        prop_assert_eq!(&def.description, &round_tripped.description);
        prop_assert_eq!(&def.model, &round_tripped.model);
        prop_assert_eq!(&def.instructions, &round_tripped.instructions);
        prop_assert_eq!(&def.tools, &round_tripped.tools);
        prop_assert_eq!(&def.sub_agents, &round_tripped.sub_agents);
        prop_assert_eq!(&def.config, &round_tripped.config);
        prop_assert_eq!(&def.metadata, &round_tripped.metadata);
    }
}

// ---------------------------------------------------------------------------
// Property 2: YAML Validation Error Identifies Invalid Field
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// **Feature: competitive-parity-v070, Property 2: YAML Validation Error Identifies Invalid Field**
    ///
    /// *For any* YamlAgentDefinition where exactly one required field is replaced
    /// with an invalid type (e.g., `name` set to an integer, `model` set to a bare
    /// string), the validation error message SHALL contain the name of the invalid
    /// field.
    ///
    /// **Validates: Requirements 1.2**
    #[test]
    fn prop_yaml_validation_error_identifies_name_field(
        provider in arb_identifier(),
        model_id in arb_identifier(),
    ) {
        // Replace `name` (a string) with a sequence — definitely not a valid string.
        let yaml_str_invalid = format!(
            "name:\n  - item1\n  - item2\nmodel:\n  provider: {provider}\n  model_id: {model_id}\n"
        );

        let result: Result<YamlAgentDefinition, _> = serde_yaml::from_str(&yaml_str_invalid);
        prop_assert!(
            result.is_err(),
            "Parsing should fail when `name` is a sequence"
        );

        let err_msg = result.unwrap_err().to_string();
        prop_assert!(
            err_msg.contains("name") || err_msg.contains("invalid type"),
            "Error message should reference the invalid field or type, got: {err_msg}"
        );
    }

    /// Property 2 variant: model field replaced with a bare string instead of a map.
    #[test]
    fn prop_yaml_validation_error_identifies_model_field(
        name in arb_identifier(),
        bad_model_value in "[a-z]{3,10}",
    ) {
        // Replace `model` (a struct/map) with a bare string
        let yaml_str = format!("name: {name}\nmodel: {bad_model_value}\n");

        let result: Result<YamlAgentDefinition, _> = serde_yaml::from_str(&yaml_str);
        prop_assert!(
            result.is_err(),
            "Parsing should fail when `model` is a bare string instead of a map"
        );

        let err_msg = result.unwrap_err().to_string();
        prop_assert!(
            err_msg.contains("model") || err_msg.contains("invalid type"),
            "Error message should reference the invalid field or type, got: {err_msg}"
        );
    }

    /// Property 2 variant: model.provider field replaced with a sequence.
    #[test]
    fn prop_yaml_validation_error_identifies_provider_field(
        name in arb_identifier(),
        model_id in arb_identifier(),
    ) {
        // Replace `provider` (a string) with a sequence
        let yaml_str = format!(
            "name: {name}\nmodel:\n  provider:\n    - a\n    - b\n  model_id: {model_id}\n"
        );

        let result: Result<YamlAgentDefinition, _> = serde_yaml::from_str(&yaml_str);
        prop_assert!(
            result.is_err(),
            "Parsing should fail when `provider` is a sequence"
        );

        let err_msg = result.unwrap_err().to_string();
        prop_assert!(
            err_msg.contains("provider") || err_msg.contains("invalid type"),
            "Error message should reference the invalid field or type, got: {err_msg}"
        );
    }

    /// Property 2 variant: model.model_id field replaced with a map.
    #[test]
    fn prop_yaml_validation_error_identifies_model_id_field(
        name in arb_identifier(),
        provider in arb_identifier(),
    ) {
        // Replace `model_id` (a string) with a nested map
        let yaml_str = format!(
            "name: {name}\nmodel:\n  provider: {provider}\n  model_id:\n    nested: value\n"
        );

        let result: Result<YamlAgentDefinition, _> = serde_yaml::from_str(&yaml_str);
        prop_assert!(
            result.is_err(),
            "Parsing should fail when `model_id` is a map"
        );

        let err_msg = result.unwrap_err().to_string();
        prop_assert!(
            err_msg.contains("model_id") || err_msg.contains("invalid type"),
            "Error message should reference the invalid field or type, got: {err_msg}"
        );
    }

    /// Property 2 variant: missing required `model` field entirely.
    #[test]
    fn prop_yaml_validation_error_identifies_missing_model(
        name in arb_identifier(),
    ) {
        // Omit the required `model` field
        let yaml_str = format!("name: {name}\n");

        let result: Result<YamlAgentDefinition, _> = serde_yaml::from_str(&yaml_str);
        prop_assert!(
            result.is_err(),
            "Parsing should fail when `model` is missing"
        );

        let err_msg = result.unwrap_err().to_string();
        prop_assert!(
            err_msg.contains("model") || err_msg.contains("missing field"),
            "Error message should reference the missing field, got: {err_msg}"
        );
    }
}
