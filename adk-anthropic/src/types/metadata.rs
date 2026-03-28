use serde::{Deserialize, Serialize};

/// Metadata that can be included with requests.
///
/// This can be used to provide additional context or client information with requests.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Metadata {
    /// An external identifier for the user who is associated with the request.
    ///
    /// This should be a uuid, hash value, or other opaque identifier. Anthropic may use
    /// this id to help detect abuse. Do not include any identifying information such as
    /// name, email address, or phone number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

impl Metadata {
    /// Creates a new empty Metadata instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new Metadata instance with the specified user_id
    pub fn with_user_id<S: Into<String>>(user_id: S) -> Self {
        Self { user_id: Some(user_id.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_metadata_serialization() {
        let metadata = Metadata::new();
        let json = serde_json::to_string(&metadata).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn with_user_id_serialization() {
        let metadata = Metadata::with_user_id("user-123");
        let json = serde_json::to_value(&metadata).unwrap();
        assert_eq!(json, serde_json::json!({"user_id":"user-123"}));
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({"user_id":"user-123"});
        let metadata: Metadata = serde_json::from_value(json).unwrap();
        assert_eq!(metadata.user_id, Some("user-123".to_string()));

        let json = serde_json::json!({});
        let metadata: Metadata = serde_json::from_value(json).unwrap();
        assert_eq!(metadata.user_id, None);
    }
}
