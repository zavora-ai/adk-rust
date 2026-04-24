//! Discovery document generation from [`BusinessContext`].

use awp_types::{AwpDiscoveryDocument, BusinessContext, CURRENT_VERSION, TrustLevel};

/// Generate an AWP discovery document from a business context.
///
/// URLs are derived from the context's `domain` field.
///
/// # Example
///
/// ```
/// use awp_types::{BusinessContext, TrustLevel, CURRENT_VERSION};
/// use adk_awp::generate_discovery_document;
///
/// let ctx = BusinessContext::core("My Site", "A site", "example.com");
/// let doc = generate_discovery_document(&ctx);
/// assert_eq!(doc.version, CURRENT_VERSION);
/// assert_eq!(doc.site_name, "My Site");
/// ```
pub fn generate_discovery_document(ctx: &BusinessContext) -> AwpDiscoveryDocument {
    AwpDiscoveryDocument {
        version: CURRENT_VERSION,
        site_name: ctx.site_name.clone(),
        site_description: ctx.site_description.clone(),
        capability_manifest_url: format!("https://{}/awp/manifest", ctx.domain),
        a2a_endpoint_url: format!("https://{}/awp/a2a", ctx.domain),
        events_endpoint_url: format!("https://{}/awp/events/subscribe", ctx.domain),
        health_endpoint_url: format!("https://{}/awp/health", ctx.domain),
        supported_trust_levels: vec![
            TrustLevel::Anonymous,
            TrustLevel::Known,
            TrustLevel::Partner,
            TrustLevel::Internal,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> BusinessContext {
        BusinessContext::core("Test Site", "A test site", "example.com")
    }

    #[test]
    fn test_generate_discovery_document_version() {
        let doc = generate_discovery_document(&sample_context());
        assert_eq!(doc.version, CURRENT_VERSION);
    }

    #[test]
    fn test_generate_discovery_document_urls() {
        let doc = generate_discovery_document(&sample_context());
        assert_eq!(doc.capability_manifest_url, "https://example.com/awp/manifest");
        assert_eq!(doc.a2a_endpoint_url, "https://example.com/awp/a2a");
        assert_eq!(doc.events_endpoint_url, "https://example.com/awp/events/subscribe");
        assert_eq!(doc.health_endpoint_url, "https://example.com/awp/health");
    }

    #[test]
    fn test_generate_discovery_document_trust_levels() {
        let doc = generate_discovery_document(&sample_context());
        assert_eq!(
            doc.supported_trust_levels,
            vec![
                TrustLevel::Anonymous,
                TrustLevel::Known,
                TrustLevel::Partner,
                TrustLevel::Internal,
            ]
        );
    }

    #[test]
    fn test_generate_discovery_document_site_info() {
        let doc = generate_discovery_document(&sample_context());
        assert_eq!(doc.site_name, "Test Site");
        assert_eq!(doc.site_description, "A test site");
    }
}
