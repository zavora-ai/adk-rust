use serde::{Deserialize, Serialize};

use crate::types::{EffortLevel, OutputFormat};

/// Wrapper for structured output configuration.
///
/// Contains both the output format and an optional effort level that controls
/// response thoroughness. Maps to the API's `output_config` parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Effort level controlling response thoroughness (`low`, `medium`, `high`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortLevel>,
    /// The output format configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<OutputFormat>,
}

impl OutputConfig {
    /// Create a new `OutputConfig` with the given format.
    pub fn new(format: OutputFormat) -> Self {
        Self { effort: None, format: Some(format) }
    }

    /// Create a new `OutputConfig` with only an effort level.
    pub fn with_effort(effort: EffortLevel) -> Self {
        Self { effort: Some(effort), format: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization_json() {
        let config = OutputConfig::new(OutputFormat::Json);
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json, json!({"format": {"type": "json_object"}}));
    }

    #[test]
    fn serialization_json_schema() {
        let schema = json!({"type": "object", "properties": {"name": {"type": "string"}}});
        let config = OutputConfig::new(OutputFormat::json_schema(schema));
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json,
            json!({
                "format": {
                    "type": "json_schema",
                    "schema": {"type": "object", "properties": {"name": {"type": "string"}}}
                }
            })
        );
    }

    #[test]
    fn serialization_effort_only() {
        let config = OutputConfig::with_effort(EffortLevel::High);
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json, json!({"effort": "high"}));
    }

    #[test]
    fn deserialization() {
        let json = json!({"format": {"type": "json_object"}});
        let config: OutputConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.format, Some(OutputFormat::Json));
    }

    #[test]
    fn deserialization_with_effort() {
        let json = json!({"effort": "low", "format": {"type": "json_object"}});
        let config: OutputConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.effort, Some(EffortLevel::Low));
        assert_eq!(config.format, Some(OutputFormat::Json));
    }
}
