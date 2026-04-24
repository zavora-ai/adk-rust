use serde::{Deserialize, Serialize};

use crate::TrustLevel;

/// Structured description of a site's business domain, capabilities, and policies.
///
/// Parsed from a `business.toml` file and used to auto-generate discovery documents
/// and capability manifests.
///
/// The core fields (`site_name`, `site_description`, `domain`, `capabilities`,
/// `policies`) are required. All extended sections are optional with `#[serde(default)]`
/// so existing minimal `business.toml` files continue to work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessContext {
    pub site_name: String,
    pub site_description: String,
    pub domain: String,
    pub capabilities: Vec<BusinessCapability>,
    pub policies: Vec<BusinessPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,

    // --- Extended schema sections (all optional) ---
    /// Business identity: country, languages, currency, timezone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business: Option<BusinessIdentity>,

    /// Brand voice configuration for agent responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand_voice: Option<BrandVoice>,

    /// Product catalog.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub products: Vec<Product>,

    /// Channel configuration (WhatsApp, email, website, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<ChannelConfig>,

    /// Payment configuration (providers, auto-approve thresholds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payments: Option<PaymentConfig>,

    /// Support configuration (escalation contacts, hours, SLA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<SupportConfig>,

    /// Content management configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentConfig>,

    /// Review platform configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<ReviewConfig>,

    /// Outreach and follow-up configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outreach: Option<OutreachConfig>,
}

/// A business capability with access level requirements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessCapability {
    pub name: String,
    pub description: String,
    pub endpoint: String,
    pub method: String,
    pub access_level: TrustLevel,
}

/// A business policy describing operational rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessPolicy {
    pub name: String,
    pub description: String,
    pub policy_type: String,
}

impl BusinessContext {
    /// Create a `BusinessContext` with only the core required fields.
    ///
    /// All extended sections (business identity, brand voice, products,
    /// channels, payments, support, content, reviews, outreach) are set
    /// to `None` / empty.
    pub fn core(
        site_name: impl Into<String>,
        site_description: impl Into<String>,
        domain: impl Into<String>,
    ) -> Self {
        Self {
            site_name: site_name.into(),
            site_description: site_description.into(),
            domain: domain.into(),
            capabilities: vec![],
            policies: vec![],
            contact: None,
            business: None,
            brand_voice: None,
            products: vec![],
            channels: None,
            payments: None,
            support: None,
            content: None,
            reviews: None,
            outreach: None,
        }
    }
}

/// Business identity and locale information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessIdentity {
    /// Business legal or display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// ISO 3166-1 alpha-2 country code (e.g. "US", "KE").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Supported languages (BCP 47 tags, e.g. ["en", "sw"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    /// ISO 4217 currency code (e.g. "USD", "KES").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// IANA timezone (e.g. "Africa/Nairobi", "America/New_York").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Brand voice configuration for consistent agent responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrandVoice {
    /// Tone descriptor (e.g. "friendly", "professional", "casual").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// Default greeting message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    /// Escalation message when handing off to human support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_message: Option<String>,
}

/// A product or service in the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Product {
    /// Stock keeping unit identifier.
    pub sku: String,
    /// Display name.
    pub name: String,
    /// Price in smallest currency unit (e.g. cents).
    #[serde(default)]
    pub price: u64,
    /// Current inventory count (0 = out of stock).
    #[serde(default)]
    pub inventory: u64,
    /// Searchable tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Channel configuration for multi-channel delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// WhatsApp Business API phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp: Option<String>,
    /// Email address for email channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Website URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// SMS phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sms: Option<String>,
}

/// Payment provider and policy configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentConfig {
    /// Enabled payment providers (e.g. ["stripe", "mpesa"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Auto-approve threshold in smallest currency unit.
    #[serde(default)]
    pub auto_approve_threshold: u64,
    /// Require owner approval above this threshold.
    #[serde(default)]
    pub require_approval_threshold: u64,
}

/// Support and escalation configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupportConfig {
    /// Escalation contact emails or phone numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalation_contacts: Vec<String>,
    /// Business hours (e.g. "Mon-Fri 9:00-17:00 EAT").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours: Option<String>,
    /// SLA response time target (e.g. "4h", "24h").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sla: Option<String>,
}

/// Content management configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentConfig {
    /// Blog or content topics the agent can write about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// Whether to auto-draft content.
    #[serde(default)]
    pub auto_draft: bool,
    /// Delay before publishing auto-drafted content (e.g. "24h").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_delay: Option<String>,
}

/// Review platform configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewConfig {
    /// Review platforms to monitor (e.g. ["google", "yelp", "trustpilot"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// Auto-respond to reviews with rating at or above this threshold (1-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_respond_threshold: Option<u8>,
}

/// Outreach and follow-up configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutreachConfig {
    /// Follow-up timing after last interaction (e.g. "48h", "7d").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_delay: Option<String>,
    /// Whether consent is required before outreach.
    #[serde(default)]
    pub require_consent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> BusinessContext {
        let mut ctx = BusinessContext::core("Test Site", "A test site", "example.com");
        ctx.capabilities = vec![BusinessCapability {
            name: "read_data".to_string(),
            description: "Read data".to_string(),
            endpoint: "/api/data".to_string(),
            method: "GET".to_string(),
            access_level: TrustLevel::Anonymous,
        }];
        ctx.policies = vec![BusinessPolicy {
            name: "privacy".to_string(),
            description: "Privacy policy".to_string(),
            policy_type: "privacy".to_string(),
        }];
        ctx.contact = Some("admin@example.com".to_string());
        ctx
    }

    #[test]
    fn test_json_serde_round_trip() {
        let ctx = sample_context();
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: BusinessContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, deserialized);
    }

    #[test]
    fn test_toml_serde_round_trip() {
        let ctx = sample_context();
        let toml_str = toml::to_string(&ctx).unwrap();
        let deserialized: BusinessContext = toml::from_str(&toml_str).unwrap();
        assert_eq!(ctx, deserialized);
    }

    #[test]
    fn test_optional_contact_skipped() {
        let mut ctx = sample_context();
        ctx.contact = None;
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("contact"));
    }

    #[test]
    fn test_minimal_toml_backward_compatible() {
        // Existing minimal business.toml files should still parse
        let toml_str = r#"
site_name = "Minimal"
site_description = "Minimal site"
domain = "example.com"
capabilities = []
policies = []
"#;
        let ctx: BusinessContext = toml::from_str(toml_str).unwrap();
        assert_eq!(ctx.site_name, "Minimal");
        assert!(ctx.business.is_none());
        assert!(ctx.products.is_empty());
        assert!(ctx.payments.is_none());
    }

    #[test]
    fn test_full_schema_toml() {
        let toml_str = r#"
site_name = "Full Shop"
site_description = "A full-featured shop"
domain = "shop.example.com"
contact = "hello@shop.example.com"

[[capabilities]]
name = "browse"
description = "Browse products"
endpoint = "/api/products"
method = "GET"
access_level = "anonymous"

[[policies]]
name = "returns"
description = "30-day return policy"
policy_type = "returns"

[business]
name = "Example Corp"
country = "KE"
languages = ["en", "sw"]
currency = "KES"
timezone = "Africa/Nairobi"

[brand_voice]
tone = "friendly"
greeting = "Karibu! How can I help you today?"
escalation_message = "Let me connect you with our team."

[[products]]
sku = "WIDGET-001"
name = "Premium Widget"
price = 2500
inventory = 100
tags = ["electronics", "gadgets"]

[channels]
whatsapp = "+254700000000"
email = "support@shop.example.com"
website = "https://shop.example.com"

[payments]
providers = ["mpesa", "stripe"]
auto_approve_threshold = 5000
require_approval_threshold = 50000

[support]
escalation_contacts = ["manager@shop.example.com"]
hours = "Mon-Fri 9:00-17:00 EAT"
sla = "4h"

[content]
topics = ["product updates", "how-to guides"]
auto_draft = true
publish_delay = "24h"

[reviews]
platforms = ["google", "trustpilot"]
auto_respond_threshold = 4

[outreach]
follow_up_delay = "48h"
require_consent = true
"#;
        let ctx: BusinessContext = toml::from_str(toml_str).unwrap();
        assert_eq!(ctx.site_name, "Full Shop");
        assert_eq!(ctx.business.as_ref().unwrap().country.as_deref(), Some("KE"));
        assert_eq!(ctx.brand_voice.as_ref().unwrap().tone.as_deref(), Some("friendly"));
        assert_eq!(ctx.products.len(), 1);
        assert_eq!(ctx.products[0].sku, "WIDGET-001");
        assert_eq!(ctx.channels.as_ref().unwrap().whatsapp.as_deref(), Some("+254700000000"));
        assert_eq!(ctx.payments.as_ref().unwrap().providers, vec!["mpesa", "stripe"]);
        assert_eq!(ctx.support.as_ref().unwrap().sla.as_deref(), Some("4h"));
        assert!(ctx.content.as_ref().unwrap().auto_draft);
        assert_eq!(ctx.reviews.as_ref().unwrap().auto_respond_threshold, Some(4));
        assert!(ctx.outreach.as_ref().unwrap().require_consent);

        // Round-trip
        let toml_out = toml::to_string_pretty(&ctx).unwrap();
        let reparsed: BusinessContext = toml::from_str(&toml_out).unwrap();
        assert_eq!(ctx, reparsed);
    }

    #[test]
    fn test_extended_fields_skipped_when_empty() {
        let ctx = sample_context();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("business"));
        assert!(!json.contains("brandVoice"));
        assert!(!json.contains("products"));
        assert!(!json.contains("channels"));
        assert!(!json.contains("payments"));
        assert!(!json.contains("support"));
        assert!(!json.contains("content"));
        assert!(!json.contains("reviews"));
        assert!(!json.contains("outreach"));
    }
}
