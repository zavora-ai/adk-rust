//! Agent card types for the registry.
//!
//! Defines [`AgentCard`] and related metadata types following the A2A protocol
//! agent card schema with camelCase JSON serialization.

use serde::{Deserialize, Serialize};

/// Metadata document describing a registered agent's capabilities and endpoint.
///
/// Follows the A2A protocol agent card schema with camelCase JSON serialization.
///
/// `owner` is the user who registered the card, set by the registry routes from the
/// authenticated caller; a value sent in a request body is ignored. A card without an owner
/// — one stored before cards had owners, or inserted through an [`AgentRegistryStore`]
/// directly — is readable by every authenticated caller and changed by no one through the
/// routes; remove or replace it through the store.
///
/// [`AgentRegistryStore`]: super::store::AgentRegistryStore
///
/// # Example
///
/// ```rust
/// use adk_server::registry::types::AgentCard;
///
/// let card = AgentCard {
///     name: "my-agent".to_string(),
///     version: "1.0.0".to_string(),
///     description: Some("A helpful agent".to_string()),
///     tags: vec!["search".to_string(), "qa".to_string()],
///     endpoint_url: Some("https://example.com/agents/my-agent".to_string()),
///     capabilities: vec!["text".to_string(), "tool-calling".to_string()],
///     input_modes: vec!["text".to_string()],
///     output_modes: vec!["text".to_string()],
///     created_at: "2025-01-15T10:30:00Z".to_string(),
///     updated_at: None,
///     owner: None,
/// };
///
/// let json = serde_json::to_string(&card).unwrap();
/// assert!(json.contains("endpointUrl"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// Unique agent name used as the primary identifier.
    pub name: String,
    /// Semantic version string (e.g. `"1.0.0"`).
    pub version: String,
    /// Optional human-readable description of the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tags for categorization and filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// URL where the agent can be reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    /// Capabilities the agent supports (e.g. `"text"`, `"tool-calling"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Supported input modes (e.g. `"text"`, `"audio"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    /// Supported output modes (e.g. `"text"`, `"audio"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
    /// ISO 8601 timestamp of when the card was created.
    pub created_at: String,
    /// ISO 8601 timestamp of the last update, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// The user ID of the caller who registered the card; `None` on a card no one owns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_card_serialization_camel_case() {
        let card = AgentCard {
            name: "test-agent".to_string(),
            version: "1.0.0".to_string(),
            description: Some("A test agent".to_string()),
            tags: vec!["test".to_string()],
            endpoint_url: Some("https://example.com".to_string()),
            capabilities: vec!["text".to_string()],
            input_modes: vec!["text".to_string()],
            output_modes: vec!["text".to_string()],
            created_at: "2025-01-15T10:30:00Z".to_string(),
            updated_at: Some("2025-01-15T11:00:00Z".to_string()),
            owner: Some("alice".to_string()),
        };

        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"endpointUrl\""));
        assert!(json.contains("\"createdAt\""));
        assert!(json.contains("\"updatedAt\""));
        assert!(json.contains("\"inputModes\""));
        assert!(json.contains("\"outputModes\""));
        assert!(!json.contains("endpoint_url"));
        assert!(!json.contains("created_at"));
    }

    #[test]
    fn test_agent_card_round_trip() {
        let card = AgentCard {
            name: "my-agent".to_string(),
            version: "2.0.0".to_string(),
            description: None,
            tags: vec![],
            endpoint_url: None,
            capabilities: vec![],
            input_modes: vec![],
            output_modes: vec![],
            created_at: "2025-06-01T00:00:00Z".to_string(),
            updated_at: None,
            owner: None,
        };

        let json = serde_json::to_string(&card).unwrap();
        let deserialized: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(card, deserialized);
    }

    #[test]
    fn test_agent_card_skips_empty_optional_fields() {
        let card = AgentCard {
            name: "minimal".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            tags: vec![],
            endpoint_url: None,
            capabilities: vec![],
            input_modes: vec![],
            output_modes: vec![],
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: None,
            owner: None,
        };

        let json = serde_json::to_string(&card).unwrap();
        assert!(!json.contains("description"));
        assert!(!json.contains("tags"));
        assert!(!json.contains("endpointUrl"));
        assert!(!json.contains("capabilities"));
        assert!(!json.contains("inputModes"));
        assert!(!json.contains("outputModes"));
        assert!(!json.contains("updatedAt"));
        assert!(!json.contains("owner"));
    }

    #[test]
    fn test_agent_card_deserialize_from_camel_case_json() {
        let json = r#"{
            "name": "from-json",
            "version": "1.0.0",
            "description": "Deserialized agent",
            "tags": ["search", "qa"],
            "endpointUrl": "https://example.com/agents/from-json",
            "capabilities": ["text", "tool-calling"],
            "inputModes": ["text"],
            "outputModes": ["text"],
            "createdAt": "2025-01-15T10:30:00Z",
            "updatedAt": "2025-01-15T10:30:00Z"
        }"#;

        let card: AgentCard = serde_json::from_str(json).unwrap();
        assert_eq!(card.name, "from-json");
        assert_eq!(card.endpoint_url, Some("https://example.com/agents/from-json".to_string()));
        assert_eq!(card.tags, vec!["search", "qa"]);
        assert_eq!(card.capabilities, vec!["text", "tool-calling"]);
        assert_eq!(card.owner, None);
    }

    #[test]
    fn test_agent_card_owner_round_trips() {
        let json = r#"{"name": "owned", "version": "1.0.0", "createdAt": "2025-01-15T10:30:00Z",
            "owner": "alice"}"#;

        let card: AgentCard = serde_json::from_str(json).unwrap();
        assert_eq!(card.owner.as_deref(), Some("alice"));
        assert_eq!(serde_json::to_value(&card).unwrap()["owner"], "alice");
    }
}
