//! Capability manifest builder from [`BusinessContext`].

use awp_types::{BusinessContext, CapabilityEntry, CapabilityManifest};

/// Build a JSON-LD capability manifest from a business context.
///
/// Produces one [`CapabilityEntry`] per [`BusinessCapability`](awp_types::BusinessCapability)
/// with `@context` set to `"https://schema.org"` and `@type` set to `"WebAPI"`.
///
/// # Example
///
/// ```
/// use awp_types::{BusinessContext, BusinessCapability, TrustLevel};
/// use adk_awp::build_manifest;
///
/// let mut ctx = BusinessContext::core("My API", "An API", "api.example.com");
/// ctx.capabilities = vec![BusinessCapability {
///     name: "get_data".to_string(),
///     description: "Get data".to_string(),
///     endpoint: "/api/data".to_string(),
///     method: "GET".to_string(),
///     access_level: TrustLevel::Anonymous,
/// }];
/// let manifest = build_manifest(&ctx);
/// assert_eq!(manifest.context, "https://schema.org");
/// assert_eq!(manifest.capabilities.len(), 1);
/// ```
pub fn build_manifest(ctx: &BusinessContext) -> CapabilityManifest {
    CapabilityManifest {
        context: "https://schema.org".to_string(),
        type_: "WebAPI".to_string(),
        name: ctx.site_name.clone(),
        description: ctx.site_description.clone(),
        capabilities: ctx
            .capabilities
            .iter()
            .map(|cap| CapabilityEntry {
                name: cap.name.clone(),
                description: cap.description.clone(),
                endpoint: cap.endpoint.clone(),
                method: cap.method.clone(),
                input_schema: None,
                output_schema: None,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::{BusinessCapability, BusinessPolicy, TrustLevel};

    fn sample_context() -> BusinessContext {
        let mut ctx = BusinessContext::core("Test API", "A test API", "api.example.com");
        ctx.capabilities = vec![
            BusinessCapability {
                name: "read_data".to_string(),
                description: "Read data".to_string(),
                endpoint: "/api/data".to_string(),
                method: "GET".to_string(),
                access_level: TrustLevel::Anonymous,
            },
            BusinessCapability {
                name: "write_data".to_string(),
                description: "Write data".to_string(),
                endpoint: "/api/data".to_string(),
                method: "POST".to_string(),
                access_level: TrustLevel::Known,
            },
        ];
        ctx.policies = vec![BusinessPolicy {
            name: "privacy".to_string(),
            description: "Privacy policy".to_string(),
            policy_type: "privacy".to_string(),
        }];
        ctx
    }

    #[test]
    fn test_build_manifest_json_ld_fields() {
        let manifest = build_manifest(&sample_context());
        assert_eq!(manifest.context, "https://schema.org");
        assert_eq!(manifest.type_, "WebAPI");
    }

    #[test]
    fn test_build_manifest_site_info() {
        let manifest = build_manifest(&sample_context());
        assert_eq!(manifest.name, "Test API");
        assert_eq!(manifest.description, "A test API");
    }

    #[test]
    fn test_build_manifest_capability_count() {
        let manifest = build_manifest(&sample_context());
        assert_eq!(manifest.capabilities.len(), 2);
    }

    #[test]
    fn test_build_manifest_capability_mapping() {
        let manifest = build_manifest(&sample_context());
        let first = &manifest.capabilities[0];
        assert_eq!(first.name, "read_data");
        assert_eq!(first.description, "Read data");
        assert_eq!(first.endpoint, "/api/data");
        assert_eq!(first.method, "GET");
        assert!(first.input_schema.is_none());
        assert!(first.output_schema.is_none());
    }

    #[test]
    fn test_build_manifest_empty_capabilities() {
        let ctx = BusinessContext::core("Empty", "No caps", "example.com");
        let manifest = build_manifest(&ctx);
        assert!(manifest.capabilities.is_empty());
        assert_eq!(manifest.context, "https://schema.org");
        assert_eq!(manifest.type_, "WebAPI");
    }
}
