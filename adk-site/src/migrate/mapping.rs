//! Feature mapping (spec 3.5 §7.8 "Feature mapping", N5): every dynamic feature of the legacy
//! site becomes a [`CapabilityV2`] draft on the edge or a named owner decision.
//!
//! | Feature | Becomes |
//! |---------|---------|
//! | Contact form | `submit_inquiry`: `NonIdempotent`, consent `contact` |
//! | Search | A static index plus `search_site`: `ReadOnly` |
//! | Comments | An owner decision: off (the default) or a held `submit_comment` |
//! | Shop | Products, plus `get_quote` and `place_order` |
//! | Bookings | `check_availability` and `book` |
//! | Logins and member areas | A named owner decision (N5): no account area on the content origin |
//! | Newsletter sign-up | `subscribe_newsletter`, consent `newsletter` (S17) |

use awp_types::{
    ApprovalHint, CapabilityEffect, CapabilityV2, ConsentRequirement, ConsentScope, TrustTier,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::inventory::{FeatureEvidence, FeatureKind, FormField, SiteInventory};

/// A capability drafted for a legacy feature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDraft {
    /// The capability.
    pub capability: CapabilityV2,
    /// The feature it replaces.
    pub replaces: FeatureKind,
    /// Where the feature was seen.
    pub evidence: Vec<FeatureEvidence>,
}

/// One choice of an owner decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOption {
    /// A stable id.
    pub id: String,
    /// What it does, in plain words.
    pub label: String,
}

/// A decision the owner makes about a feature the static site cannot carry as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerDecision {
    /// A stable id, `migration.<feature>`.
    pub id: String,
    /// The feature.
    pub feature: FeatureKind,
    /// The question.
    pub question: String,
    /// The choices.
    pub options: Vec<DecisionOption>,
    /// The choice that applies until the owner decides.
    pub default: String,
    /// Where the feature was seen.
    pub evidence: Vec<FeatureEvidence>,
}

/// What the legacy features map to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureMap {
    /// Capability drafts for the edge.
    pub capabilities: Vec<CapabilityDraft>,
    /// Decisions for the owner.
    pub decisions: Vec<OwnerDecision>,
    /// Whether the build writes a static search index.
    pub static_search_index: bool,
    /// Whether the site's products migrate as product items.
    pub products: bool,
}

impl FeatureMap {
    /// The drafted capability named `name`.
    pub fn capability(&self, name: &str) -> Option<&CapabilityV2> {
        self.capabilities.iter().map(|d| &d.capability).find(|c| c.name == name)
    }

    /// The decision about `feature`.
    pub fn decision(&self, feature: FeatureKind) -> Option<&OwnerDecision> {
        self.decisions.iter().find(|d| d.feature == feature)
    }

    /// Applies an owner's answer: choosing a held `submit_comment` adds that capability.
    pub fn answer(&mut self, feature: FeatureKind, option: &str) {
        if feature == FeatureKind::Comments
            && option == "held"
            && self.capability("submit_comment").is_none()
        {
            let evidence = self.decision(feature).map(|d| d.evidence.clone()).unwrap_or_default();
            self.capabilities.push(CapabilityDraft {
                capability: submit_comment(),
                replaces: FeatureKind::Comments,
                evidence,
            });
        }
        if let Some(decision) = self.decisions.iter_mut().find(|d| d.feature == feature) {
            decision.default = option.to_string();
        }
    }

    /// The capability a form of `feature` posts to, for freeze and the generator.
    pub fn capability_for(&self, feature: FeatureKind) -> Option<&CapabilityV2> {
        self.capabilities.iter().find(|d| d.replaces == feature).map(|d| &d.capability)
    }
}

fn schema(fields: &[FormField], fallback: &[(&str, &str, bool)]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let mut add = |name: &str, kind: &str, needed: bool| {
        let property = match kind {
            "email" => json!({ "type": "string", "format": "email", "maxLength": 254 }),
            "number" | "range" => json!({ "type": "number" }),
            "date" => json!({ "type": "string", "format": "date" }),
            "datetime-local" => json!({ "type": "string", "format": "date-time" }),
            "checkbox" => json!({ "type": "boolean" }),
            "textarea" => json!({ "type": "string", "maxLength": 5000 }),
            _ => json!({ "type": "string", "maxLength": 500 }),
        };
        properties.insert(name.to_string(), property);
        if needed {
            required.push(Value::String(name.to_string()));
        }
    };
    if fields.is_empty() {
        for (name, kind, needed) in fallback {
            add(name, kind, *needed);
        }
    } else {
        for field in fields {
            let name: String = field
                .name
                .trim_start_matches("your-")
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
                .collect();
            add(&name, &field.input_type, field.required);
        }
    }
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn submit_comment() -> CapabilityV2 {
    CapabilityV2 {
        input_schema: Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "maxLength": 500 },
                "name": { "type": "string", "maxLength": 100 },
                "comment": { "type": "string", "maxLength": 5000 }
            },
            "required": ["path", "comment"],
            "additionalProperties": false
        })),
        ..CapabilityV2::new(
            "submit_comment",
            "Submit a comment on a page; every comment is held for moderation",
            CapabilityEffect::NonIdempotent,
        )
        .with_approval(ApprovalHint::Always)
    }
}

/// Maps the inventory's features to capability drafts and owner decisions.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::mapping::map_features;
/// use adk_site::migrate::{DetectedFeature, FeatureKind, SiteInventory, SourceKind};
///
/// let mut inventory = SiteInventory::new(SourceKind::Crawl, chrono::Utc::now());
/// inventory.add_feature(DetectedFeature { kind: FeatureKind::ContactForm, evidence: vec![], fields: vec![] });
/// let map = map_features(&inventory);
/// let inquiry = map.capability("submit_inquiry").unwrap();
/// assert!(inquiry.requires_idempotency());
/// assert_eq!(inquiry.consent.as_ref().unwrap().scope.as_str(), "contact");
/// ```
pub fn map_features(inventory: &SiteInventory) -> FeatureMap {
    let mut map = FeatureMap::default();
    let mut features = inventory.features.clone();
    features.sort_by_key(|f| f.kind);
    for feature in features {
        let evidence = feature.evidence.clone();
        let mut draft = |capability: CapabilityV2| {
            map.capabilities.push(CapabilityDraft {
                capability,
                replaces: feature.kind,
                evidence: evidence.clone(),
            });
        };
        match feature.kind {
            FeatureKind::ContactForm => {
                let mut capability = CapabilityV2::new(
                    "submit_inquiry",
                    "Send the site's owner an enquiry",
                    CapabilityEffect::NonIdempotent,
                );
                capability.consent = Some(ConsentRequirement {
                    scope: ConsentScope::Custom("contact".into()),
                    policy_version: "1".into(),
                });
                capability.input_schema = Some(schema(
                    &feature.fields,
                    &[("name", "text", true), ("email", "email", true), ("message", "textarea", true)],
                ));
                draft(capability);
            }
            FeatureKind::Search => {
                let mut capability =
                    CapabilityV2::new("search_site", "Search the site's pages", CapabilityEffect::ReadOnly);
                capability.input_schema = Some(json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "maxLength": 200 } },
                    "required": ["query"],
                    "additionalProperties": false
                }));
                draft(capability);
                map.static_search_index = true;
            }
            FeatureKind::Shop => {
                let mut quote = CapabilityV2::new(
                    "get_quote",
                    "Quote products at their published prices",
                    CapabilityEffect::Idempotent,
                )
                .with_min_trust(TrustTier::Known);
                quote.input_schema = Some(json!({
                    "type": "object",
                    "properties": { "items": { "type": "array", "items": {
                        "type": "object",
                        "properties": { "sku": { "type": "string" }, "quantity": { "type": "integer", "minimum": 1 } },
                        "required": ["sku", "quantity"]
                    } } },
                    "required": ["items"]
                }));
                draft(quote);
                let mut order = CapabilityV2::new(
                    "place_order",
                    "Place an order against a quote",
                    CapabilityEffect::NonIdempotent,
                )
                .with_min_trust(TrustTier::Known);
                order.input_schema = Some(json!({
                    "type": "object",
                    "properties": { "quoteId": { "type": "string" } },
                    "required": ["quoteId"]
                }));
                draft(order);
                map.products = true;
            }
            FeatureKind::Bookings => {
                let mut availability = CapabilityV2::new(
                    "check_availability",
                    "List open booking slots",
                    CapabilityEffect::ReadOnly,
                );
                availability.input_schema = Some(json!({
                    "type": "object",
                    "properties": { "from": { "type": "string", "format": "date" }, "to": { "type": "string", "format": "date" } }
                }));
                draft(availability);
                let mut book = CapabilityV2::new("book", "Book a slot", CapabilityEffect::NonIdempotent)
                    .with_min_trust(TrustTier::Known);
                book.input_schema = Some(schema(
                    &feature.fields,
                    &[("slot", "text", true), ("name", "text", true), ("email", "email", true)],
                ));
                draft(book);
            }
            FeatureKind::Newsletter => {
                let mut capability = CapabilityV2::new(
                    "subscribe_newsletter",
                    "Subscribe to the site's newsletter",
                    CapabilityEffect::Idempotent,
                );
                capability.consent = Some(ConsentRequirement {
                    scope: ConsentScope::Newsletter,
                    policy_version: "1".into(),
                });
                capability.input_schema = Some(schema(&[], &[("email", "email", true)]));
                draft(capability);
            }
            FeatureKind::Comments => map.decisions.push(OwnerDecision {
                id: "migration.comments".into(),
                feature: FeatureKind::Comments,
                question: "The old site takes comments. Turn comments off, or accept them through a capability that holds every comment for your moderation?".into(),
                options: vec![
                    DecisionOption { id: "off".into(), label: "Turn comments off; existing comments are not published".into() },
                    DecisionOption { id: "held".into(), label: "Accept comments, each held until you approve it".into() },
                ],
                default: "off".into(),
                evidence,
            }),
            FeatureKind::Login | FeatureKind::MemberArea => {
                if map.decision(FeatureKind::MemberArea).is_some() || map.decision(FeatureKind::Login).is_some() {
                    if let Some(existing) = map.decisions.iter_mut().find(|d| matches!(d.feature, FeatureKind::Login | FeatureKind::MemberArea)) {
                        existing.evidence.extend(evidence);
                    }
                    continue;
                }
                map.decisions.push(OwnerDecision {
                    id: "migration.accounts".into(),
                    feature: feature.kind,
                    question: "The old site has logins or a member area. The new site serves no accounts from its pages. What happens to that area?".into(),
                    options: vec![
                        DecisionOption { id: "remove".into(), label: "Remove the login and member pages".into() },
                        DecisionOption { id: "named_service".into(), label: "Link to a service you name that holds the accounts".into() },
                        DecisionOption { id: "later".into(), label: "Leave the account area out until authenticated areas are supported".into() },
                    ],
                    default: "later".into(),
                    evidence,
                });
            }
        }
    }
    if inventory.items.iter().any(|i| i.kind == awp_types::ContentKind::Product) {
        map.products = true;
    }
    map
}
