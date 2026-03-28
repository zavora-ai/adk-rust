use serde::{Deserialize, Serialize};

/// Configuration for enabling or disabling citations in the response.
///
/// This type allows controlling whether citations will be included in the model's response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationsConfig {
    /// Whether citations are enabled
    pub enabled: bool,
}

impl CitationsConfig {
    /// Creates a new CitationsConfig with the specified enabled state
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Returns a CitationsConfig with citations enabled
    pub fn enabled() -> Self {
        Self::new(true)
    }

    /// Returns a CitationsConfig with citations disabled
    pub fn disabled() -> Self {
        Self::new(false)
    }
}

impl Default for CitationsConfig {
    /// By default, citations are disabled
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_enabled() {
        let config = CitationsConfig::enabled();
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json, serde_json::json!({"enabled": true}));
    }

    #[test]
    fn serialization_disabled() {
        let config = CitationsConfig::disabled();
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json, serde_json::json!({"enabled": false}));
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({"enabled": true});
        let config: CitationsConfig = serde_json::from_value(json).unwrap();
        assert!(config.enabled);

        let json = serde_json::json!({"enabled": false});
        let config: CitationsConfig = serde_json::from_value(json).unwrap();
        assert!(!config.enabled);
    }
}
