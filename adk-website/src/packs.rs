//! Default capability packs (spec 3.5 §7.16 `packs`, §7.4, §7.16.1 "Capabilities", §7.23).
//!
//! A pack is a set of typed capability declarations: each is one `CapabilityV2` the agent
//! edge exposes as an A2A skill, an MCP tool and `POST /awp/v2/capabilities/{name}`, under one
//! policy (D-4). The names match the ones migration's feature mapping drafts (§7.8 "Feature
//! mapping"), so a migrated contact form and a template's contact form are one capability.
//!
//! | Pack | Capabilities |
//! |---|---|
//! | [`Pack::Contact`] | `submit_inquiry`, `subscribe_newsletter`, `request_prospectus`, `licensing_enquiry`, `volunteer`, `submit_service_form` |
//! | [`Pack::Quote`] | `get_quote` |
//! | [`Pack::Booking`] | `check_availability`, `book`, `book_appointment`, `register` |
//! | [`Pack::Order`] | `list_products`, `place_order`, `pay_order`, `order_status`, `request_return`, `donate`, `subscribe`, `buy_ticket` |
//! | [`Pack::Search`] | `search_site` |
//! | [`Pack::Components`] | `practice_information`, `lookup_status`, `list_notices`, `list_events`, `get_schedule` |
//!
//! The handlers that serve these on the edge arrive with the edge (spec 3.5 §7.9).

use awp_types::{
    ApprovalHint, CapabilityEffect, CapabilityV2, ConsentRequirement, ConsentScope, TrustTier,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A default capability pack (spec 3.5 §7.16 `packs`).
///
/// # Example
///
/// ```rust
/// use adk_website::packs::Pack;
///
/// let names: Vec<String> = Pack::Quote.capabilities().into_iter().map(|c| c.name).collect();
/// assert_eq!(names, ["get_quote"]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pack {
    /// Enquiries, sign-ups and forms.
    Contact,
    /// Quotes at published prices.
    Quote,
    /// Availability, bookings, appointments and registrations.
    Booking,
    /// Catalogue, orders, payments, returns, donations, subscriptions and tickets.
    Order,
    /// Search over the current release.
    Search,
    /// Read-only components a static page composes: practice information, status lookups,
    /// notices, events and schedules.
    Components,
}

impl Pack {
    /// Every pack.
    pub const ALL: [Pack; 6] =
        [Pack::Contact, Pack::Quote, Pack::Booking, Pack::Order, Pack::Search, Pack::Components];

    /// The pack's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contact => "contact",
            Self::Quote => "quote",
            Self::Booking => "booking",
            Self::Order => "order",
            Self::Search => "search",
            Self::Components => "components",
        }
    }

    /// The names of the pack's capabilities.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Self::Contact => &[
                "submit_inquiry",
                "subscribe_newsletter",
                "request_prospectus",
                "licensing_enquiry",
                "volunteer",
                "submit_service_form",
            ],
            Self::Quote => &["get_quote"],
            Self::Booking => &["check_availability", "book", "book_appointment", "register"],
            Self::Order => &[
                "list_products",
                "place_order",
                "pay_order",
                "order_status",
                "request_return",
                "donate",
                "subscribe",
                "buy_ticket",
            ],
            Self::Search => &["search_site"],
            Self::Components => &[
                "practice_information",
                "lookup_status",
                "list_notices",
                "list_events",
                "get_schedule",
            ],
        }
    }

    /// The pack's capability declarations.
    pub fn capabilities(self) -> Vec<CapabilityV2> {
        self.names().iter().filter_map(|name| capability(name)).collect()
    }

    /// The pack a capability belongs to.
    pub fn of(name: &str) -> Option<Pack> {
        Self::ALL.into_iter().find(|pack| pack.names().contains(&name))
    }
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn consent(scope: ConsentScope) -> Option<ConsentRequirement> {
    Some(ConsentRequirement { scope, policy_version: "1".into() })
}

fn custom(scope: &str) -> ConsentScope {
    ConsentScope::Custom(scope.into())
}

fn email() -> Value {
    json!({ "type": "string", "format": "email", "maxLength": 254 })
}

fn text(max: u32) -> Value {
    json!({ "type": "string", "maxLength": max })
}

/// The declaration of the capability `name`, or `None` for a name no pack declares.
///
/// Every `NonIdempotent` capability executes at most once per idempotency key (A-I7); a
/// consent-bound one refuses without a live receipt for its scope (A-I8); a service form
/// needs a verified identity (A-I6, S16).
///
/// # Example
///
/// ```rust
/// use adk_website::packs::capability;
/// use awp_types::TrustTier;
///
/// let form = capability("submit_service_form").unwrap();
/// assert_eq!(form.min_trust, TrustTier::Verified);
/// assert!(form.requires_idempotency());
/// assert_eq!(capability("subscribe_newsletter").unwrap().consent.unwrap().scope.as_str(), "newsletter");
/// ```
pub fn capability(name: &str) -> Option<CapabilityV2> {
    use CapabilityEffect::{Idempotent, NonIdempotent, ReadOnly};
    let declared = |description: &str, effect| CapabilityV2::new(name, description, effect);
    let capability = match name {
        "submit_inquiry" => CapabilityV2 {
            consent: consent(custom("contact")),
            input_schema: Some(object(
                json!({ "name": text(200), "email": email(), "message": text(5000) }),
                &["name", "email", "message"],
            )),
            ..declared("Send the site's owner an enquiry", NonIdempotent)
        },
        "subscribe_newsletter" => CapabilityV2 {
            consent: consent(ConsentScope::Newsletter),
            input_schema: Some(object(json!({ "email": email() }), &["email"])),
            ..declared("Subscribe to the site's newsletter", Idempotent)
        },
        "request_prospectus" => CapabilityV2 {
            consent: consent(custom("contact")),
            input_schema: Some(object(
                json!({ "name": text(200), "email": email(), "programme": text(200) }),
                &["name", "email"],
            )),
            ..declared("Request a prospectus", NonIdempotent)
        },
        "licensing_enquiry" => CapabilityV2 {
            consent: consent(custom("contact")),
            input_schema: Some(object(
                json!({ "organisation": text(200), "email": email(), "use": text(2000), "items": { "type": "array", "items": text(500), "maxItems": 50 } }),
                &["organisation", "email", "use"],
            )),
            ..declared("Ask to license the site's content", NonIdempotent)
        },
        "volunteer" => CapabilityV2 {
            consent: consent(custom("contact")),
            input_schema: Some(object(
                json!({ "name": text(200), "email": email(), "availability": text(500), "interests": text(1000) }),
                &["name", "email"],
            )),
            ..declared("Offer to volunteer", NonIdempotent)
        },
        "submit_service_form" => CapabilityV2 {
            consent: consent(custom("service")),
            input_schema: Some(object(
                json!({ "service": text(100), "fields": { "type": "object", "maxProperties": 50 } }),
                &["service", "fields"],
            )),
            ..declared(
                "Apply for a public service; the applicant's identity comes from a verified identity provider",
                NonIdempotent,
            )
            .with_min_trust(TrustTier::Verified)
        },
        "get_quote" => CapabilityV2 {
            input_schema: Some(object(
                json!({ "items": { "type": "array", "maxItems": 100, "items": {
                    "type": "object",
                    "properties": { "sku": text(100), "quantity": { "type": "integer", "minimum": 1 } },
                    "required": ["sku", "quantity"]
                } } }),
                &["items"],
            )),
            ..declared("Quote products or services at their published prices", Idempotent)
                .with_min_trust(TrustTier::Known)
        },
        "check_availability" => CapabilityV2 {
            input_schema: Some(object(
                json!({ "from": { "type": "string", "format": "date" }, "to": { "type": "string", "format": "date" } }),
                &[],
            )),
            ..declared("List open booking slots", ReadOnly)
        },
        "book" => CapabilityV2 {
            consent: consent(custom("contact")),
            input_schema: Some(object(
                json!({ "slot": text(100), "name": text(200), "email": email() }),
                &["slot", "name", "email"],
            )),
            ..declared("Book a slot", NonIdempotent).with_min_trust(TrustTier::Known)
        },
        "book_appointment" => CapabilityV2 {
            consent: consent(custom("appointment")),
            input_schema: Some(object(
                json!({ "slot": text(100), "name": text(200), "email": email(), "reason": text(500) }),
                &["slot", "name", "email"],
            )),
            ..declared("Book an appointment", NonIdempotent).with_min_trust(TrustTier::Known)
        },
        "register" => CapabilityV2 {
            consent: consent(custom("contact")),
            input_schema: Some(object(
                json!({ "event": text(200), "name": text(200), "email": email() }),
                &["event", "name", "email"],
            )),
            ..declared("Register for an event or a sign-up list", NonIdempotent)
        },
        "list_products" => CapabilityV2 {
            input_schema: Some(object(
                json!({ "query": text(200), "category": text(100), "cursor": text(200) }),
                &[],
            )),
            ..declared("List the catalogue at its published prices", ReadOnly)
        },
        "place_order" => CapabilityV2 {
            consent: consent(ConsentScope::PaymentData),
            input_schema: Some(object(json!({ "quoteId": text(100) }), &["quoteId"])),
            ..declared("Place an order against a quote", NonIdempotent)
                .with_min_trust(TrustTier::Known)
        },
        "pay_order" => CapabilityV2 {
            consent: consent(ConsentScope::PaymentData),
            input_schema: Some(object(
                json!({ "orderId": text(100), "rail": text(50) }),
                &["orderId", "rail"],
            )),
            ..declared("Pay for an order through an accepted rail", NonIdempotent)
                .with_min_trust(TrustTier::Known)
        },
        "order_status" => CapabilityV2 {
            input_schema: Some(object(json!({ "orderId": text(100) }), &["orderId"])),
            ..declared("Report an order's status", ReadOnly).with_min_trust(TrustTier::Known)
        },
        "request_return" => CapabilityV2 {
            input_schema: Some(object(
                json!({ "orderId": text(100), "reason": text(1000) }),
                &["orderId", "reason"],
            )),
            ..declared("Request a return; every return is held for the owner", NonIdempotent)
                .with_min_trust(TrustTier::Known)
                .with_approval(ApprovalHint::Always)
        },
        "donate" => CapabilityV2 {
            consent: consent(ConsentScope::PaymentData),
            input_schema: Some(object(
                json!({ "amount": { "type": "integer", "minimum": 1 }, "currency": text(3), "giftAid": { "type": "boolean" }, "email": email() }),
                &["amount", "currency", "email"],
            )),
            ..declared("Make a donation and receive a receipt", NonIdempotent)
        },
        "subscribe" => CapabilityV2 {
            consent: consent(ConsentScope::PaymentData),
            input_schema: Some(object(
                json!({ "plan": text(100), "email": email() }),
                &["plan", "email"],
            )),
            ..declared("Start a paid subscription", NonIdempotent).with_min_trust(TrustTier::Known)
        },
        "buy_ticket" => CapabilityV2 {
            consent: consent(ConsentScope::PaymentData),
            input_schema: Some(object(
                json!({ "ticket": text(100), "quantity": { "type": "integer", "minimum": 1, "maximum": 20 }, "email": email() }),
                &["ticket", "quantity", "email"],
            )),
            ..declared("Buy tickets", NonIdempotent).with_min_trust(TrustTier::Known)
        },
        "search_site" => CapabilityV2 {
            input_schema: Some(object(json!({ "query": text(200) }), &["query"])),
            ..declared("Search the site's pages", ReadOnly)
        },
        "practice_information" => {
            declared("Opening hours, services and how to reach the practice", ReadOnly)
        }
        "lookup_status" => CapabilityV2 {
            input_schema: Some(object(json!({ "reference": text(100) }), &["reference"])),
            ..declared("Look up the status of an application by its reference", ReadOnly)
                .with_min_trust(TrustTier::Verified)
        },
        "list_notices" => declared("List the official notices in force", ReadOnly),
        "list_events" => declared("List upcoming events", ReadOnly),
        "get_schedule" => declared("The event's schedule", ReadOnly),
        _ => return None,
    };
    Some(capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pack_name_has_a_declaration_and_one_pack() {
        for pack in Pack::ALL {
            for name in pack.names() {
                let declared = capability(name).unwrap_or_else(|| panic!("{name}"));
                assert_eq!(declared.name, *name);
                assert_eq!(Pack::of(name), Some(pack));
            }
        }
        assert!(capability("delete_everything").is_none());
    }

    #[test]
    fn names_match_what_migration_drafts() {
        use adk_site::migrate::mapping::map_features;
        use adk_site::migrate::{DetectedFeature, FeatureKind, SiteInventory, SourceKind};

        let mut inventory = SiteInventory::new(SourceKind::Crawl, chrono::Utc::now());
        for kind in [
            FeatureKind::ContactForm,
            FeatureKind::Search,
            FeatureKind::Shop,
            FeatureKind::Bookings,
            FeatureKind::Newsletter,
        ] {
            inventory.add_feature(DetectedFeature { kind, evidence: vec![], fields: vec![] });
        }
        for draft in map_features(&inventory).capabilities {
            let ours = capability(&draft.capability.name)
                .unwrap_or_else(|| panic!("{} has no pack", draft.capability.name));
            assert_eq!(ours.effect, draft.capability.effect, "{}", ours.name);
        }
    }

    #[test]
    fn writes_are_keyed_and_forms_ask_for_consent() {
        for pack in Pack::ALL {
            for declared in pack.capabilities() {
                if declared.effect == CapabilityEffect::NonIdempotent {
                    assert!(declared.requires_idempotency(), "{}", declared.name);
                }
                if declared.input_schema.as_ref().is_some_and(|s| s.to_string().contains("email")) {
                    assert!(declared.consent.is_some(), "{} takes an email", declared.name);
                }
            }
        }
    }
}
