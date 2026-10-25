use serde::{Deserialize, Serialize};

use crate::TrustLevel;

/// JSON-LD capability manifest describing what a site offers to agents.
///
/// The `@context` field is always `"https://schema.org"` and `@type` is `"WebAPI"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityManifest {
    #[serde(rename = "@context")]
    pub context: String,
    #[serde(rename = "@type")]
    pub type_: String,
    pub name: String,
    pub description: String,
    pub capabilities: Vec<CapabilityEntry>,
}

/// A single capability entry within a [`CapabilityManifest`].
///
/// `access_level` is the lowest trust level the capability declares; manifests written
/// before it existed omit it (spec 3.5 §10 X1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEntry {
    pub name: String,
    pub description: String,
    pub endpoint: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_level: Option<TrustLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_round_trip() {
        let manifest = CapabilityManifest {
            context: "https://schema.org".to_string(),
            type_: "WebAPI".to_string(),
            name: "Test API".to_string(),
            description: "A test API".to_string(),
            capabilities: vec![CapabilityEntry {
                name: "get_data".to_string(),
                description: "Get data".to_string(),
                endpoint: "/api/data".to_string(),
                method: "GET".to_string(),
                access_level: Some(TrustLevel::Known),
                input_schema: None,
                output_schema: Some("DataResponse".to_string()),
            }],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: CapabilityManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn test_json_ld_fields() {
        let manifest = CapabilityManifest {
            context: "https://schema.org".to_string(),
            type_: "WebAPI".to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            capabilities: vec![],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"@context\""));
        assert!(json.contains("\"@type\""));
    }

    #[test]
    fn test_optional_fields_skipped() {
        let entry = CapabilityEntry {
            name: "test".to_string(),
            description: "test".to_string(),
            endpoint: "/test".to_string(),
            method: "GET".to_string(),
            access_level: None,
            input_schema: None,
            output_schema: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("inputSchema"));
        assert!(!json.contains("outputSchema"));
        assert!(!json.contains("accessLevel"));
    }

    #[test]
    fn test_access_level_is_camel_case_and_optional() {
        let json = r#"{"name":"n","description":"d","endpoint":"/e","method":"GET","accessLevel":"partner"}"#;
        let entry: CapabilityEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.access_level, Some(TrustLevel::Partner));
        assert_eq!(serde_json::to_string(&entry).unwrap(), json);

        let older = r#"{"name":"n","description":"d","endpoint":"/e","method":"GET"}"#;
        let entry: CapabilityEntry = serde_json::from_str(older).unwrap();
        assert_eq!(entry.access_level, None);
    }
}
