//! Managed-agent configuration types for the Interactions API.
//!
//! This module defines the [`AgentConfig`] enum, which carries agent-specific
//! configuration attached to a create-interaction request. Currently only Deep
//! Research uses this; Antigravity takes no config.
//!
//! # Example
//!
//! ```rust
//! use adk_gemini::interactions::AgentConfig;
//!
//! // Create a Deep Research config with defaults (thinking + visualization enabled)
//! let config = AgentConfig::deep_research();
//!
//! // Serialize to JSON
//! let json = serde_json::to_value(&config).unwrap();
//! assert_eq!(json["type"], "deep-research");
//! assert_eq!(json["thinking_summaries"], true);
//! assert_eq!(json["visualization"], true);
//! ```

use serde::{Deserialize, Serialize};

/// Managed-agent-specific configuration attached to an interaction request.
///
/// Currently only Deep Research uses this; Antigravity takes no config.
/// The enum is tagged on the `"type"` field with kebab-case discriminators,
/// allowing forward-compatible deserialization of unknown agent config types
/// into the [`AgentConfig::Other`] variant.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::AgentConfig;
///
/// let config = AgentConfig::DeepResearch {
///     thinking_summaries: Some(true),
///     visualization: Some(true),
///     collaborative_planning: Some(false),
/// };
///
/// let json = serde_json::to_string(&config).unwrap();
/// assert!(json.contains("\"type\":\"deep-research\""));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AgentConfig {
    /// Deep Research configuration.
    ///
    /// Controls output options for long-running research tasks. All fields are
    /// optional — omitted fields use server defaults.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::AgentConfig;
    ///
    /// let config = AgentConfig::deep_research();
    /// assert!(matches!(config, AgentConfig::DeepResearch { .. }));
    /// ```
    DeepResearch {
        /// Whether to include thinking summaries in the output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_summaries: Option<bool>,
        /// Whether to include visualization in the output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visualization: Option<bool>,
        /// Whether to enable collaborative planning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collaborative_planning: Option<bool>,
    },
    /// An agent config type not modelled by this crate version.
    ///
    /// This variant captures any JSON object whose `type` discriminator is not
    /// explicitly handled, ensuring forward compatibility with future API
    /// revisions.
    #[serde(untagged)]
    Other(serde_json::Value),
}

impl AgentConfig {
    /// Create a Deep Research config with thinking summaries and visualization
    /// enabled.
    ///
    /// This is the recommended starting point for Deep Research interactions.
    /// `collaborative_planning` is left unset (server default).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::AgentConfig;
    ///
    /// let config = AgentConfig::deep_research();
    ///
    /// let json = serde_json::to_value(&config).unwrap();
    /// assert_eq!(json["type"], "deep-research");
    /// assert_eq!(json["thinking_summaries"], true);
    /// assert_eq!(json["visualization"], true);
    /// assert!(json.get("collaborative_planning").is_none());
    /// ```
    pub fn deep_research() -> Self {
        AgentConfig::DeepResearch {
            thinking_summaries: Some(true),
            visualization: Some(true),
            collaborative_planning: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deep_research_serialization() {
        let config = AgentConfig::deep_research();
        let json = serde_json::to_value(&config).unwrap();

        assert_eq!(json["type"], "deep-research");
        assert_eq!(json["thinking_summaries"], true);
        assert_eq!(json["visualization"], true);
        assert!(json.get("collaborative_planning").is_none());
    }

    #[test]
    fn test_deep_research_round_trip() {
        let config = AgentConfig::DeepResearch {
            thinking_summaries: Some(true),
            visualization: Some(false),
            collaborative_planning: Some(true),
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_deep_research_omits_none_fields() {
        let config = AgentConfig::DeepResearch {
            thinking_summaries: None,
            visualization: None,
            collaborative_planning: None,
        };

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["type"], "deep-research");
        assert!(json.get("thinking_summaries").is_none());
        assert!(json.get("visualization").is_none());
        assert!(json.get("collaborative_planning").is_none());
    }

    #[test]
    fn test_unknown_type_deserializes_to_other() {
        let json = r#"{"type": "future-agent", "some_field": 42}"#;
        let config: AgentConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, AgentConfig::Other(_)));
    }

    #[test]
    fn test_deep_research_convenience_constructor() {
        let config = AgentConfig::deep_research();
        match config {
            AgentConfig::DeepResearch {
                thinking_summaries,
                visualization,
                collaborative_planning,
            } => {
                assert_eq!(thinking_summaries, Some(true));
                assert_eq!(visualization, Some(true));
                assert_eq!(collaborative_planning, None);
            }
            _ => panic!("expected DeepResearch variant"),
        }
    }
}
