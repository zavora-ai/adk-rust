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
//! The edge serves them through [`crate::edge::handlers`] (spec 3.5 §7.9). An owner's
//! [`ApprovalThresholds`] hold the orders, bookings and payments above an amount for the
//! owner's decision (`ApprovalHint::AboveAmount`, spec 3.5 §7.4, S9).
//!
//! A pack is a signed, installable bundle (spec 3.5 §7.23):
//!
//! | Module | Contents |
//! |---|---|
//! | [`bundle`] | The bundle: declarations, [`HandlerConfig`](bundle::HandlerConfig)s, playbooks and driver manifests, sealed with a content digest and the publisher's AWP 2.0 JWS; verification and the format's rules |
//! | [`trust`] | The trust list of publisher keys a site installs packs from |
//! | [`builtin`] | The six packs above as bundles signed with a test key whose trust is explicit |
//! | [`installed`] | The packs a site loads ([`PackSet`](installed::PackSet)) and the project's `packs/` directory ([`PackDir`](installed::PackDir)): install, upgrade, remove |

pub mod builtin;
pub mod bundle;
pub mod installed;
pub mod trust;

use std::collections::BTreeMap;

use adk_core::{AdkError, Result};
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

/// The policy version every pack's consent requirement names: the charter's public terms
/// (spec 3.5 §7.5).
pub const POLICY_VERSION: &str = "1";

fn consent(scope: ConsentScope) -> Option<ConsentRequirement> {
    Some(ConsentRequirement { scope, policy_version: POLICY_VERSION.into() })
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
                json!({
                    "orderId": text(100),
                    "rail": text(50),
                    "mandate": {
                        "type": "object",
                        "required": ["payment_mandate_contents"],
                        "description": "The buyer's AP2 payment mandate for the cart the order returned, with its user authorization"
                    },
                    "credential": {
                        "type": "string",
                        "maxLength": 4096,
                        "description": "A single-use payment token from the buyer's payment provider, for rails that take one"
                    }
                }),
                &["orderId", "rail"],
            )),
            ..declared(
                "Pay for an order through an accepted rail, with the buyer's AP2 payment mandate or rail credential",
                NonIdempotent,
            )
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

/// The capabilities whose calls carry an amount the edge reads: the quote's total of
/// `place_order`, the order's total of `pay_order`, the slot's price of `book` and
/// `book_appointment`, and the `amount` of `donate` (spec 3.5 §7.4).
pub const AMOUNTED: &[&str] = &["place_order", "pay_order", "book", "book_appointment", "donate"];

/// The capabilities a commerce threshold covers: the calls that commit a buyer to an order,
/// a booking or a gift. A threshold on `pay_order` as well would hold an order the owner
/// approved a second time, when it is paid.
const COMMITTING: &[&str] = &["place_order", "book", "book_appointment", "donate"];

/// The owner's amount thresholds (spec 3.5 §7.4, S9): a call of a named capability whose
/// amount is above its threshold answers `202 Accepted` and waits for the owner's decision
/// (`ApprovalHint::AboveAmount`). Amounts are minor units of the site's currency. A served
/// site reads them from the project's `approvals.json`, such as
/// `{"place_order": 20000, "book": 15000}`.
///
/// A threshold tightens a declaration and never loosens one: a capability every call of
/// which is held stays held, and of two thresholds the lower holds.
///
/// # Example
///
/// ```rust
/// use adk_website::packs::{ApprovalThresholds, capability};
/// use awp_types::ApprovalHint;
///
/// let thresholds = ApprovalThresholds::commerce(20_000).above("pay_order", 50_000);
/// thresholds.validate().unwrap();
/// let order = thresholds.apply(capability("place_order").unwrap());
/// assert_eq!(order.approval, ApprovalHint::AboveAmount(20_000));
/// let ret = thresholds.apply(capability("request_return").unwrap());
/// assert_eq!(ret.approval, ApprovalHint::Always);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApprovalThresholds(BTreeMap<String, u64>);

impl ApprovalThresholds {
    /// No thresholds: only the declarations hold calls.
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds orders, bookings, appointments and donations above `amount` (spec 3.5 S9).
    pub fn commerce(amount: u64) -> Self {
        COMMITTING.iter().fold(Self::new(), |thresholds, name| thresholds.above(*name, amount))
    }

    /// Holds calls of `capability` above `amount`.
    #[must_use]
    pub fn above(mut self, capability: impl Into<String>, amount: u64) -> Self {
        self.0.insert(capability.into(), amount);
        self
    }

    /// The threshold of `capability`, when there is one.
    pub fn get(&self, capability: &str) -> Option<u64> {
        self.0.get(capability).copied()
    }

    /// Whether no threshold is set.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Checks that each threshold names a capability whose calls carry an amount
    /// ([`AMOUNTED`]).
    ///
    /// # Errors
    ///
    /// A configuration error naming the capability, since a threshold on a call without an
    /// amount would hold nothing.
    pub fn validate(&self) -> Result<()> {
        match self.0.keys().find(|name| !AMOUNTED.contains(&name.as_str())) {
            Some(name) => Err(AdkError::config(format!(
                "an approval threshold names `{name}`, whose calls carry no amount; thresholds \
                 apply to {}",
                AMOUNTED.join(", ")
            ))),
            None => Ok(()),
        }
    }

    /// `declaration` with its threshold applied: `AboveAmount` with the lower of the two
    /// limits; a declaration that holds every call is kept.
    pub fn apply(&self, mut declaration: CapabilityV2) -> CapabilityV2 {
        if let Some(limit) = self.get(&declaration.name) {
            declaration.approval = match declaration.approval {
                ApprovalHint::Always => ApprovalHint::Always,
                ApprovalHint::AboveAmount(declared) => {
                    ApprovalHint::AboveAmount(declared.min(limit))
                }
                ApprovalHint::Never => ApprovalHint::AboveAmount(limit),
                // A hint this release does not know is kept as declared.
                other => other,
            };
        }
        declaration
    }
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

    #[test]
    fn thresholds_tighten_and_never_loosen() {
        let thresholds = ApprovalThresholds::commerce(20_000).above("place_order", 30_000);
        let order = thresholds.apply(capability("place_order").unwrap());
        assert_eq!(order.approval, ApprovalHint::AboveAmount(30_000), "the owner's last word");
        let tighter = ApprovalThresholds::new()
            .above("book", 10_000)
            .apply(capability("book").unwrap().with_approval(ApprovalHint::AboveAmount(5_000)));
        assert_eq!(tighter.approval, ApprovalHint::AboveAmount(5_000), "the lower limit holds");
        let unpaid = thresholds.apply(capability("pay_order").unwrap());
        assert_eq!(unpaid.approval, ApprovalHint::Never, "a commerce threshold leaves payment");
        assert!(ApprovalThresholds::new().above("search_site", 1).validate().is_err());
        assert!(ApprovalThresholds::commerce(1).above("pay_order", 2).validate().is_ok());
        let parsed: ApprovalThresholds =
            serde_json::from_value(json!({ "place_order": 20000, "book": 15000 })).unwrap();
        assert_eq!(
            parsed,
            ApprovalThresholds::new().above("place_order", 20_000).above("book", 15_000)
        );
    }

    #[test]
    fn pay_order_takes_a_mandate_or_a_credential() {
        use adk_awp::v2::capability::{
            CapabilityCall, CapabilityError, CapabilityHandler, CapabilityTool,
        };

        struct Noop;
        #[async_trait::async_trait]
        impl CapabilityHandler for Noop {
            async fn handle(
                &self,
                call: CapabilityCall,
            ) -> std::result::Result<Value, CapabilityError> {
                Ok(call.input)
            }
        }
        let pay = CapabilityTool::new(
            "shop.example",
            capability("pay_order").unwrap(),
            std::sync::Arc::new(Noop),
        )
        .unwrap();
        let mandate = json!({ "payment_mandate_contents": { "payment_mandate_id": "pm-1" } });
        let valid = |input: Value| pay.validate_input(&input).is_ok();
        assert!(valid(json!({ "orderId": "o-1", "rail": "ap2", "mandate": mandate })));
        assert!(valid(json!({ "orderId": "o-1", "rail": "card", "credential": "tok_1" })));
        assert!(valid(json!({ "orderId": "o-1", "rail": "mpesa" })));
        assert!(!valid(json!({ "orderId": "o-1", "rail": "ap2", "card": "4242" })));
        assert!(!valid(json!({ "orderId": "o-1", "rail": "ap2", "mandate": {} })));
    }
}
