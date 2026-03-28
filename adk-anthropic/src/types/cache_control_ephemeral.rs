use serde::{Deserialize, Serialize};

/// TTL configuration for cache control.
///
/// Specifies the cache tier: `"standard"` (5-minute) or `"long"` (1-hour).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheTtl {
    /// The TTL type: `"standard"` or `"long"`.
    #[serde(rename = "type")]
    pub ttl_type: String,
}

impl CacheTtl {
    /// Create a standard TTL (5-minute cache tier).
    pub fn standard() -> Self {
        Self { ttl_type: "standard".to_string() }
    }

    /// Create a long TTL (1-hour cache tier).
    pub fn long() -> Self {
        Self { ttl_type: "long".to_string() }
    }
}

/// CacheControlEphemeral specifies that content should be cached ephemerally.
///
/// The `type` field is always `"ephemeral"`. An optional `ttl` field distinguishes
/// between standard (5-minute) and long (1-hour) cache tiers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheControlEphemeral {
    /// The type is always "ephemeral" for this struct.
    #[serde(default = "default_type")]
    pub r#type: String,

    /// Optional TTL configuration for cache tier selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<CacheTtl>,
}

fn default_type() -> String {
    "ephemeral".to_string()
}

impl CacheControlEphemeral {
    /// Creates a new CacheControlEphemeral instance with no TTL.
    pub fn new() -> Self {
        Self { r#type: default_type(), ttl: None }
    }

    /// Creates a CacheControlEphemeral with a specific TTL.
    pub fn with_ttl(mut self, ttl: CacheTtl) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

impl Default for CacheControlEphemeral {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let cache_control = CacheControlEphemeral::new();
        let json = serde_json::to_value(&cache_control).unwrap();
        assert_eq!(json, serde_json::json!({"type": "ephemeral"}));
    }

    #[test]
    fn serialization_with_ttl() {
        let cache_control = CacheControlEphemeral::new().with_ttl(CacheTtl::long());
        let json = serde_json::to_value(&cache_control).unwrap();
        assert_eq!(json, serde_json::json!({"type": "ephemeral", "ttl": {"type": "long"}}));
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({"type": "ephemeral"});
        let cache_control: CacheControlEphemeral = serde_json::from_value(json).unwrap();
        assert_eq!(cache_control.r#type, "ephemeral");
        assert!(cache_control.ttl.is_none());
    }

    #[test]
    fn deserialization_with_ttl() {
        let json = serde_json::json!({"type": "ephemeral", "ttl": {"type": "standard"}});
        let cache_control: CacheControlEphemeral = serde_json::from_value(json).unwrap();
        assert_eq!(cache_control.ttl.as_ref().unwrap().ttl_type, "standard");
    }
}
