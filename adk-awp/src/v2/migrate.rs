//! `business.toml` (AWP 1.x) to 2.0 site data and a draft charter: the library behind
//! `adk-rust awp migrate-config` (spec 3.5 §4.1, §8.1).
//!
//! The migration is a draft for onboarding to confirm, not a signed result: the contract is
//! unsigned, the charter is version 0 with no owner, and every inference is listed in
//! [`ConfigMigration::warnings`].

use awp_types::{
    ApprovalHint, Budget, BusinessContext, Cadence, CapabilityEffect, CapabilityV2, Charter,
    ConsentScope, ContactLinks, Governance, IdempotencySource, IdentityMethod, IdentityPolicy,
    LanguageTag, PriceRule, Profile, PublicTerms, Reporting, SiteContract, SiteKind, SiteRef,
    TrustLevel, TrustTier, Voice,
};
use uuid::Uuid;

use super::capability::is_valid_capability_name;

/// The result of migrating a `business.toml` (spec 3.5 §8.1).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigMigration {
    /// The site.
    pub site: SiteRef,
    /// An unsigned draft contract for the `static+edge` profile.
    pub contract: SiteContract,
    /// The capabilities, as 2.0 declarations.
    pub capabilities: Vec<CapabilityV2>,
    /// A draft charter, version 0, with no owner and no signature.
    pub charter: Charter,
    /// Every inference and everything not carried over, for the owner to confirm.
    pub warnings: Vec<String>,
}

/// Infers the site kind from a 1.x context: a public-sector domain is `government`, products
/// or payments make `ecommerce`, anything else is `business`.
pub fn infer_site_kind(ctx: &BusinessContext) -> SiteKind {
    let domain = ctx.domain.to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').collect();
    let public_sector = labels
        .iter()
        .rev()
        .take(3)
        .any(|label| matches!(*label, "gov" | "go" | "gouv" | "gob" | "govt"));
    if public_sector {
        SiteKind::Government
    } else if !ctx.products.is_empty() || ctx.payments.is_some() {
        SiteKind::Ecommerce
    } else {
        SiteKind::Business
    }
}

/// Turns a 1.x capability name into a valid 2.0 one (`^[a-z][a-z0-9_]{0,63}$`).
fn capability_name(name: &str) -> String {
    let mut mapped: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '_' })
        .collect();
    if !mapped.starts_with(|c: char| c.is_ascii_lowercase()) {
        mapped.insert_str(0, "c_");
    }
    mapped.truncate(64);
    mapped
}

/// Migrates a 1.x `business.toml` context to 2.0 site data and a draft charter (spec 3.5
/// §8.1).
///
/// | 1.x | 2.0 |
/// |---|---|
/// | `site_name`, `domain`, `business.languages` | `SiteRef` |
/// | `[[capabilities]]` | `CapabilityV2`: effect from the method (`GET`/`HEAD` read-only, `PUT`/`DELETE` idempotent, others non-idempotent), tier from `access_level`, JSON schemas kept |
/// | `access_level = "internal"` | Not published; listed in the warnings |
/// | `[payments] auto_approve_threshold` | `above_amount` approval on non-idempotent capabilities; otherwise `always` |
/// | `site_description`, `brand_voice`, `business.currency`, `contact`, `outreach`, `payments` | Mission, voice, budget currency, contact links and consent scopes of the draft charter |
///
/// # Example
///
/// ```
/// use adk_awp::v2::migrate::migrate_config;
/// use awp_types::{BusinessCapability, BusinessContext, CapabilityEffect, SiteKind, TrustLevel};
/// use uuid::Uuid;
///
/// let mut ctx = BusinessContext::core("Example Solar", "Solar for Nairobi homes", "example.com");
/// ctx.capabilities = vec![BusinessCapability {
///     name: "place-order".into(),
///     description: "Place an order".into(),
///     endpoint: "/api/orders".into(),
///     method: "POST".into(),
///     access_level: TrustLevel::Known,
///     input_schema: None,
///     output_schema: None,
/// }];
/// let migration = migrate_config(&ctx, Uuid::nil());
/// assert_eq!(migration.capabilities[0].name, "place_order");
/// assert_eq!(migration.capabilities[0].effect, CapabilityEffect::NonIdempotent);
/// assert_eq!(migration.charter.kind, SiteKind::Business);
/// assert!(!migration.warnings.is_empty());
/// ```
pub fn migrate_config(ctx: &BusinessContext, site_id: Uuid) -> ConfigMigration {
    let mut warnings = Vec::new();
    let domain = ctx.domain.to_ascii_lowercase();

    let mut languages = Vec::new();
    for language in ctx.business.iter().flat_map(|business| business.languages.iter()) {
        match language.parse::<LanguageTag>() {
            Ok(tag) => languages.push(tag),
            Err(error) => warnings.push(format!("language skipped: {error}")),
        }
    }
    let site =
        SiteRef { id: site_id, name: ctx.site_name.clone(), domain: domain.clone(), languages };

    let kind = infer_site_kind(ctx);
    warnings.push(format!(
        "site kind inferred as `{}`; confirm it during onboarding",
        serde_json::to_value(kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    ));

    let auto_threshold = ctx.payments.as_ref().map(|payments| payments.auto_approve_threshold);
    let mut capabilities = Vec::new();
    for capability in &ctx.capabilities {
        let name = capability_name(&capability.name);
        if name != capability.name {
            warnings.push(format!("capability `{}` renamed `{name}`", capability.name));
        }
        if !is_valid_capability_name(&name)
            || capabilities.iter().any(|c: &CapabilityV2| c.name == name)
        {
            warnings
                .push(format!("capability `{}` skipped: its name is not unique", capability.name));
            continue;
        }
        let min_trust = match capability.access_level {
            TrustLevel::Anonymous => TrustTier::Public,
            TrustLevel::Known => TrustTier::Known,
            TrustLevel::Partner => TrustTier::Partner,
            TrustLevel::Internal => {
                warnings.push(format!(
                    "capability `{name}` is internal; AWP 2.0 publishes no internal capabilities, so it was not carried over"
                ));
                continue;
            }
        };
        let effect = match capability.method.to_ascii_uppercase().as_str() {
            "GET" | "HEAD" => CapabilityEffect::ReadOnly,
            "PUT" | "DELETE" => CapabilityEffect::Idempotent,
            method => {
                warnings.push(format!(
                    "capability `{name}` ({method}) is declared non_idempotent; confirm its effect"
                ));
                CapabilityEffect::NonIdempotent
            }
        };
        let mut schema = |label: &str, value: &Option<String>| -> Option<serde_json::Value> {
            let text = value.as_ref()?;
            match serde_json::from_str::<serde_json::Value>(text) {
                Ok(schema) if schema.is_object() => Some(schema),
                _ => {
                    warnings.push(format!(
                        "capability `{name}` names its {label} schema `{text}`, which is not JSON Schema; add the schema"
                    ));
                    None
                }
            }
        };
        let input_schema = schema("input", &capability.input_schema);
        let output_schema = schema("output", &capability.output_schema);
        let approval = match (effect, auto_threshold) {
            (CapabilityEffect::NonIdempotent, Some(threshold)) => {
                ApprovalHint::AboveAmount(threshold)
            }
            (CapabilityEffect::NonIdempotent, None) => ApprovalHint::Always,
            _ => ApprovalHint::Never,
        };
        capabilities.push(CapabilityV2 {
            name,
            description: capability.description.clone(),
            effect,
            min_trust,
            input_schema,
            output_schema,
            consent: None,
            price: None,
            approval,
            idempotency: IdempotencySource::Binding,
            rate_class: None,
            deprecated: None,
        });
    }

    let contact = ContactLinks {
        security: None,
        email: ctx.contact.clone().filter(|contact| contact.contains('@')),
        page: None,
    };
    let mut contract = SiteContract::new(site.clone());
    contract.profile = Some(Profile::StaticEdge);
    contract.identity =
        Some(IdentityPolicy { accepts: vec![IdentityMethod::WebBotAuth], trust_rules: None });
    contract.contact = Some(contact.clone());
    contract.keys = Some(format!("https://{domain}/.well-known/awp/keys.json"));
    warnings.push(
        "the contract is an unsigned draft: add the edge's surface URLs, publish keys.json and sign it with the root key"
            .to_owned(),
    );

    let mut consent_scopes = Vec::new();
    if ctx.outreach.as_ref().is_some_and(|outreach| outreach.require_consent) {
        consent_scopes.push(ConsentScope::ProactiveOutreach);
    }
    if ctx.payments.is_some() {
        consent_scopes.push(ConsentScope::PaymentData);
    }
    if !ctx.policies.is_empty() {
        let names: Vec<&str> = ctx.policies.iter().map(|policy| policy.name.as_str()).collect();
        warnings.push(format!(
            "policies {} need a URL and a version before the public terms can name them",
            names.join(", ")
        ));
    }
    let currency =
        ctx.business.as_ref().and_then(|business| business.currency.clone()).unwrap_or_else(|| {
            warnings.push(
                "no currency in `[business]`; the budget uses USD until the owner sets one"
                    .to_owned(),
            );
            "USD".to_owned()
        });
    warnings.push(
        "the charter is a draft: name its owner, goals, KPIs with baselines and budget caps, then sign it".to_owned(),
    );

    let charter = Charter {
        version: 0,
        site_id,
        kind,
        governance: Governance::default(),
        mission: ctx.site_description.clone(),
        strategy: Vec::new(),
        goals: Vec::new(),
        kpis: Vec::new(),
        guardrails: Vec::new(),
        authority: Vec::new(),
        budget: Budget { period: Cadence::Monthly, currency, caps: Default::default() },
        voice: Voice {
            tone: ctx.brand_voice.as_ref().and_then(|voice| voice.tone.clone()).unwrap_or_default(),
            languages: site.languages.clone(),
            disclosure: format!("Written by {}'s AI assistant.", ctx.site_name),
        },
        red_lines: Vec::new(),
        reporting: Reporting { review: Cadence::Weekly, routes: Vec::new(), quiet_hours: None },
        public: PublicTerms { usage: None, policies: Vec::new(), consent_scopes, contact },
        price_rule: PriceRule::default(),
        signed: None,
    };

    ConfigMigration { site, contract, capabilities, charter, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::{BusinessCapability, BusinessIdentity, OutreachConfig, PaymentConfig};

    fn capability(name: &str, method: &str, access_level: TrustLevel) -> BusinessCapability {
        BusinessCapability {
            name: name.to_owned(),
            description: format!("{name} capability"),
            endpoint: format!("/api/{name}"),
            method: method.to_owned(),
            access_level,
            input_schema: None,
            output_schema: None,
        }
    }

    #[test]
    fn effects_follow_the_method_and_tiers_the_access_level() {
        let mut ctx = BusinessContext::core("Shop", "A shop", "Shop.Example.com");
        ctx.capabilities = vec![
            capability("browse", "GET", TrustLevel::Anonymous),
            capability("update_cart", "PUT", TrustLevel::Known),
            capability("Place Order", "post", TrustLevel::Partner),
            capability("admin", "POST", TrustLevel::Internal),
        ];
        ctx.capabilities[1].input_schema = Some(r#"{"type":"object"}"#.to_owned());
        ctx.capabilities[1].output_schema = Some("CartView".to_owned());
        ctx.payments = Some(PaymentConfig {
            providers: vec![],
            auto_approve_threshold: 5_000,
            require_approval_threshold: 50_000,
        });
        let migration = migrate_config(&ctx, Uuid::nil());
        let summary: Vec<(&str, CapabilityEffect, TrustTier, &ApprovalHint)> = migration
            .capabilities
            .iter()
            .map(|c| (c.name.as_str(), c.effect, c.min_trust, &c.approval))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("browse", CapabilityEffect::ReadOnly, TrustTier::Public, &ApprovalHint::Never),
                (
                    "update_cart",
                    CapabilityEffect::Idempotent,
                    TrustTier::Known,
                    &ApprovalHint::Never
                ),
                (
                    "place_order",
                    CapabilityEffect::NonIdempotent,
                    TrustTier::Partner,
                    &ApprovalHint::AboveAmount(5_000)
                ),
            ]
        );
        assert_eq!(
            migration.capabilities[1].input_schema,
            Some(serde_json::json!({ "type": "object" }))
        );
        assert_eq!(migration.capabilities[1].output_schema, None);
        assert_eq!(migration.site.domain, "shop.example.com");
        assert_eq!(migration.charter.kind, SiteKind::Ecommerce);
        let warnings = migration.warnings.join("\n");
        assert!(warnings.contains("`admin` is internal"));
        assert!(warnings.contains("`place_order` (POST)"));
        assert!(warnings.contains("`CartView`"));
        assert!(warnings.contains("renamed `place_order`"));
    }

    #[test]
    fn the_draft_charter_carries_voice_scopes_and_currency() {
        let mut ctx = BusinessContext::core("Ministry", "Public services", "services.health.go.ke");
        ctx.business = Some(BusinessIdentity {
            name: None,
            country: Some("KE".to_owned()),
            languages: vec!["en".to_owned(), "sw".to_owned(), "not a tag".to_owned()],
            currency: Some("KES".to_owned()),
            timezone: None,
        });
        ctx.outreach = Some(OutreachConfig { follow_up_delay: None, require_consent: true });
        ctx.contact = Some("info@health.go.ke".to_owned());
        let migration = migrate_config(&ctx, Uuid::nil());
        assert_eq!(migration.charter.kind, SiteKind::Government);
        assert_eq!(migration.charter.budget.currency, "KES");
        assert_eq!(migration.charter.version, 0);
        assert_eq!(migration.charter.public.consent_scopes, vec![ConsentScope::ProactiveOutreach]);
        assert_eq!(migration.charter.public.contact.email.as_deref(), Some("info@health.go.ke"));
        assert_eq!(migration.site.languages.len(), 2);
        assert_eq!(migration.contract.profile, Some(Profile::StaticEdge));
        assert_eq!(
            migration.contract.keys.as_deref(),
            Some("https://services.health.go.ke/.well-known/awp/keys.json")
        );
        assert!(migration.contract.proof.is_none());
        assert!(
            migration.charter.validate().is_err(),
            "a draft has no owner until onboarding names one"
        );
    }
}
