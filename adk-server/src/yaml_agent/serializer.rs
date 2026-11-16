//! YAML serialization for agent definitions.
//!
//! Provides round-trip serialization: a [`YamlAgentDefinition`] can be
//! serialized to YAML and parsed back to produce an equivalent definition.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_server::yaml_agent::serializer::serialize_definition;
//! use adk_server::yaml_agent::schema::YamlAgentDefinition;
//!
//! let def: YamlAgentDefinition = /* ... */;
//! let yaml_str = serialize_definition(&def)?;
//! ```

use super::schema::YamlAgentDefinition;

/// Serialize a [`YamlAgentDefinition`] back to a YAML string.
///
/// The output preserves field ordering consistent with the schema and omits
/// optional fields that hold their default values (empty vecs, None options).
///
/// # Errors
///
/// Returns an error if serialization fails (unlikely for well-formed definitions).
pub fn serialize_definition(def: &YamlAgentDefinition) -> Result<String, adk_core::AdkError> {
    serde_saphyr::to_string(def).map_err(|e| {
        adk_core::AdkError::config(format!("failed to serialize YAML agent definition: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml_agent::schema::{
        McpToolReference, MemoryConfig, ModelConfig, PluginReference, SessionConfig,
        SubAgentReference, ToolReference,
    };
    use std::collections::HashMap;

    fn minimal_definition() -> YamlAgentDefinition {
        YamlAgentDefinition {
            name: "test_agent".to_string(),
            description: None,
            model: ModelConfig {
                provider: "gemini".to_string(),
                model_id: "gemini-2.5-flash".to_string(),
                temperature: None,
                max_tokens: None,
            },
            instructions: None,
            tools: vec![],
            sub_agents: vec![],
            config: HashMap::new(),
            metadata: HashMap::new(),
            plugins: vec![],
            session: None,
            memory: None,
        }
    }

    #[test]
    fn test_serialize_minimal() {
        let def = minimal_definition();
        let yaml = serialize_definition(&def).unwrap();
        assert!(yaml.contains("name: test_agent"));
        assert!(yaml.contains("provider: gemini"));
        assert!(yaml.contains("model_id: gemini-2.5-flash"));
    }

    #[test]
    fn test_round_trip_minimal() {
        let original = minimal_definition();
        let yaml = serialize_definition(&original).unwrap();
        let parsed: YamlAgentDefinition = serde_saphyr::from_str(&yaml).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_round_trip_full() {
        let original = YamlAgentDefinition {
            name: "full_agent".to_string(),
            description: Some("A fully configured agent".to_string()),
            model: ModelConfig {
                provider: "openai".to_string(),
                model_id: "gpt-4".to_string(),
                temperature: Some(0.7),
                max_tokens: Some(4096),
            },
            instructions: Some("You are a helpful assistant.".to_string()),
            tools: vec![
                ToolReference::Named { name: "search".to_string() },
                ToolReference::Mcp {
                    mcp: McpToolReference {
                        endpoint: "npx @mcp/server".to_string(),
                        args: vec!["/data".to_string()],
                    },
                },
            ],
            sub_agents: vec![SubAgentReference { reference: "helper".to_string() }],
            config: {
                let mut m = HashMap::new();
                m.insert("key".to_string(), serde_json::json!("value"));
                m
            },
            metadata: HashMap::new(),
            plugins: vec![PluginReference {
                name: "telemetry".to_string(),
                config: Some(serde_json::json!({"endpoint": "http://localhost:4317"})),
            }],
            session: Some(SessionConfig {
                backend: "postgres".to_string(),
                options: {
                    let mut m = HashMap::new();
                    m.insert(
                        "connection_string".to_string(),
                        serde_json::json!("postgres://localhost/db"),
                    );
                    m
                },
            }),
            memory: Some(MemoryConfig { backend: "inmemory".to_string(), options: HashMap::new() }),
        };

        let yaml = serialize_definition(&original).unwrap();
        let parsed: YamlAgentDefinition = serde_saphyr::from_str(&yaml).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_round_trip_with_plugins() {
        let original = YamlAgentDefinition {
            plugins: vec![
                PluginReference { name: "auth".to_string(), config: None },
                PluginReference {
                    name: "rate_limit".to_string(),
                    config: Some(serde_json::json!({"max_requests": 100})),
                },
            ],
            ..minimal_definition()
        };

        let yaml = serialize_definition(&original).unwrap();
        let parsed: YamlAgentDefinition = serde_saphyr::from_str(&yaml).unwrap();
        assert_eq!(original, parsed);
    }
}
