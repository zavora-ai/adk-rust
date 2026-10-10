use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Lifetime of a prompt cache entry.
///
/// Serializes to the Messages API values `"5m"` (the API default) and `"1h"`.
///
/// # Example
///
/// ```
/// use adk_anthropic::{CacheControlEphemeral, CacheTtl};
///
/// let cache_control = CacheControlEphemeral::new().with_ttl(CacheTtl::one_hour());
/// assert_eq!(
///     serde_json::to_value(&cache_control).unwrap(),
///     serde_json::json!({"type": "ephemeral", "ttl": "1h"})
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheTtl {
    /// The TTL value sent to the API: `"5m"` or `"1h"`.
    pub ttl_type: String,
}

impl CacheTtl {
    /// A 5-minute cache entry, refreshed on every read.
    pub fn five_minutes() -> Self {
        Self { ttl_type: "5m".to_string() }
    }

    /// A 1-hour cache entry. Writes cost more than 5-minute writes, so it pays off
    /// when requests sharing a prefix are more than five minutes apart.
    pub fn one_hour() -> Self {
        Self { ttl_type: "1h".to_string() }
    }

    /// Same as [`CacheTtl::five_minutes`].
    pub fn standard() -> Self {
        Self::five_minutes()
    }

    /// Same as [`CacheTtl::one_hour`].
    pub fn long() -> Self {
        Self::one_hour()
    }

    /// The value sent to the API. Earlier `"standard"` and `"long"` values map
    /// to `"5m"` and `"1h"`.
    pub fn as_api_str(&self) -> &str {
        match self.ttl_type.as_str() {
            "standard" => "5m",
            "long" => "1h",
            other => other,
        }
    }
}

impl Serialize for CacheTtl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_api_str())
    }
}

impl<'de> Deserialize<'de> for CacheTtl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Accepts the API string and the `{"type": ...}` object earlier versions wrote.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Value(String),
            Object {
                #[serde(rename = "type")]
                ttl_type: String,
            },
        }
        let ttl_type = match Wire::deserialize(deserializer)? {
            Wire::Value(value) | Wire::Object { ttl_type: value } => value,
        };
        let ttl = CacheTtl { ttl_type };
        Ok(CacheTtl { ttl_type: ttl.as_api_str().to_string() })
    }
}

/// CacheControlEphemeral specifies that content should be cached ephemerally.
///
/// The `type` field is always `"ephemeral"`. The optional `ttl` selects a 5-minute
/// (default) or 1-hour cache entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheControlEphemeral {
    /// The type is always "ephemeral" for this struct.
    #[serde(default = "default_type")]
    pub r#type: String,

    /// Cache entry lifetime; the API uses 5 minutes when absent.
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
        let one_hour = CacheControlEphemeral::new().with_ttl(CacheTtl::one_hour());
        assert_eq!(
            serde_json::to_value(&one_hour).unwrap(),
            serde_json::json!({"type": "ephemeral", "ttl": "1h"})
        );
        let five_minutes = CacheControlEphemeral::new().with_ttl(CacheTtl::five_minutes());
        assert_eq!(
            serde_json::to_value(&five_minutes).unwrap(),
            serde_json::json!({"type": "ephemeral", "ttl": "5m"})
        );
    }

    #[test]
    fn legacy_constructors_serialize_to_api_values() {
        assert_eq!(serde_json::to_value(CacheTtl::standard()).unwrap(), "5m");
        assert_eq!(serde_json::to_value(CacheTtl::long()).unwrap(), "1h");
        let legacy = CacheTtl { ttl_type: "long".to_string() };
        assert_eq!(serde_json::to_value(&legacy).unwrap(), "1h");
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
        let json = serde_json::json!({"type": "ephemeral", "ttl": "1h"});
        let cache_control: CacheControlEphemeral = serde_json::from_value(json).unwrap();
        assert_eq!(cache_control.ttl, Some(CacheTtl::one_hour()));
    }

    #[test]
    fn deserialization_of_legacy_object_ttl() {
        let json = serde_json::json!({"type": "ephemeral", "ttl": {"type": "standard"}});
        let cache_control: CacheControlEphemeral = serde_json::from_value(json).unwrap();
        assert_eq!(cache_control.ttl, Some(CacheTtl::five_minutes()));
    }
}
