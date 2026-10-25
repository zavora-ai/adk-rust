//! AWP configuration error types and TOML serialization utilities.

use awp_types::BusinessContext;

/// Errors that can occur when loading or validating AWP configuration.
#[derive(Debug, thiserror::Error)]
pub enum AwpConfigError {
    /// Failed to read a configuration file from disk.
    #[error("failed to read {path}: {source}")]
    FileRead { path: String, source: std::io::Error },

    /// The TOML content could not be parsed.
    #[error("invalid TOML in {path}: {source}")]
    TomlParse { path: String, source: toml::de::Error },

    /// A capability failed validation (e.g. empty name or endpoint).
    #[error("validation error in capability {index}: {field} must not be empty")]
    ValidationError { index: usize, field: String },

    /// An error from the file watcher subsystem.
    #[error("file watcher error: {0}")]
    WatcherError(String),

    /// The business context could not be serialized to TOML (spec 3.5 §10 X4).
    #[error(
        "failed to serialize the business context to TOML: {0}; TOML integers stop at i64::MAX"
    )]
    Serialize(#[source] toml::ser::Error),
}

/// Serialize a [`BusinessContext`] to a pretty-printed TOML string.
///
/// # Errors
///
/// Returns [`AwpConfigError::Serialize`] if the context cannot be represented in TOML, such
/// as a product price above `i64::MAX`.
///
/// # Example
///
/// ```
/// use adk_awp::business_context_to_toml;
/// use awp_types::BusinessContext;
///
/// let toml = business_context_to_toml(&BusinessContext::core("Shop", "A shop", "example.com")).unwrap();
/// assert!(toml.contains("site_name = \"Shop\""));
/// ```
pub fn business_context_to_toml(ctx: &BusinessContext) -> Result<String, AwpConfigError> {
    toml::to_string_pretty(ctx).map_err(AwpConfigError::Serialize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::{BusinessCapability, BusinessPolicy, TrustLevel};

    #[test]
    fn test_business_context_to_toml_round_trip() {
        let mut ctx = BusinessContext::core("Test Site", "A test", "example.com");
        ctx.capabilities = vec![BusinessCapability {
            name: "read".to_string(),
            description: "Read data".to_string(),
            endpoint: "/api/read".to_string(),
            method: "GET".to_string(),
            access_level: TrustLevel::Anonymous,
            input_schema: None,
            output_schema: None,
        }];
        ctx.policies = vec![BusinessPolicy {
            name: "privacy".to_string(),
            description: "Privacy policy".to_string(),
            policy_type: "privacy".to_string(),
        }];
        ctx.contact = Some("admin@example.com".to_string());

        let toml_str = business_context_to_toml(&ctx).unwrap();
        let parsed: BusinessContext = toml::from_str(&toml_str).unwrap();
        assert_eq!(ctx, parsed);
    }

    /// Serialization failures have their own variant, not `WatcherError` (spec 3.5 §10 X4).
    #[test]
    fn test_business_context_to_toml_reports_serialize_errors() {
        let mut ctx = BusinessContext::core("Shop", "A shop", "example.com");
        ctx.products = vec![awp_types::Product {
            sku: "X".to_string(),
            name: "Out of TOML range".to_string(),
            price: u64::MAX,
            inventory: 0,
            tags: vec![],
            description: None,
        }];
        let err = business_context_to_toml(&ctx).unwrap_err();
        assert!(matches!(err, AwpConfigError::Serialize(_)), "{err:?}");
        assert!(err.to_string().starts_with("failed to serialize the business context to TOML"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_business_context_to_toml_no_contact() {
        let ctx = BusinessContext::core("Test", "Test", "example.com");

        let toml_str = business_context_to_toml(&ctx).unwrap();
        assert!(toml_str.contains("site_name"));
        // Round-trip
        let parsed: BusinessContext = toml::from_str(&toml_str).unwrap();
        assert_eq!(ctx, parsed);
    }
}
