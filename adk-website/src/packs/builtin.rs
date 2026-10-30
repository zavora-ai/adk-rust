//! The built-in packs as signed bundles (spec 3.5 §7.16 `packs`, §7.23).
//!
//! Each [`Pack`] ships as a bundle in the crate's `packs/` directory, compiled in and
//! verified when it is loaded. The bundles are generated from [`capability`](super::capability)
//! and [`Pack::handler`]; a test regenerates them and fails when a shipped file differs
//! (`ADK_WEBSITE_PACKS_RECORD=1` writes them again).
//!
//! They are signed with a **test key** whose seed is public in this source file, so the
//! signature proves only that the compiled-in copy is the generated one. Its trust is
//! explicit: [`builtin_trust`] names it for the built-in bundles alone, no site's trust list
//! holds it, and `adk-rust site packs trust` refuses it, so a pack anyone signs with it never
//! installs.

use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use awp_types::ContentKind;

use super::Pack;
use super::bundle::{
    BookingOperation, HandlerConfig, OrderOperation, PackBundle, PackError, PackPublisher,
    VerifiedPack,
};
use super::trust::{TrustList, TrustedKey};

/// The publisher of the built-in packs.
pub const BUILTIN_PUBLISHER: &str = "adk-website";

/// The key id the built-in packs are signed with.
pub const BUILTIN_KID: &str = "adk-website-builtin-packs-test";

/// The version every built-in pack carries in this release; raised when a pack's content
/// changes.
pub const BUILTIN_VERSION: &str = "1.0.0";

/// The built-in packs' test key. Public on purpose: see the module documentation.
const BUILTIN_TEST_SEED: [u8; 32] = *b"adk-website builtin packs test!!";

/// The test key the built-in packs are signed with.
pub(crate) fn builtin_test_key() -> Ed25519DocumentSigner {
    // The seed and key id are constants that `from_seed` accepts.
    Ed25519DocumentSigner::from_seed(BUILTIN_KID, BUILTIN_TEST_SEED)
        .unwrap_or_else(|error| unreachable!("the built-in test key is valid: {error}"))
}

/// The trust list the built-in bundles are verified against: the built-in packs' test key,
/// and nothing else (spec 3.5 §7.23).
///
/// # Example
///
/// ```rust
/// use adk_website::packs::builtin::{BUILTIN_KID, BUILTIN_PUBLISHER, builtin_trust};
///
/// let trust = builtin_trust();
/// assert_eq!(trust.keys.len(), 1);
/// assert!(trust.key(BUILTIN_PUBLISHER, BUILTIN_KID).is_some());
/// ```
pub fn builtin_trust() -> TrustList {
    TrustList::new().with(
        TrustedKey::new(BUILTIN_PUBLISHER, BUILTIN_KID, builtin_test_key().public_key()).noted(
            "The built-in packs' test key: its seed is public in adk-website's source, so it \
             authenticates only the bundles compiled into adk-website",
        ),
    )
}

/// Whether `key` is the built-in packs' test key, which no site's trust list may hold.
pub fn is_builtin_test_key(key: &awp_types::PublicKeyJwk) -> bool {
    *key == builtin_test_key().public_key()
}

impl Pack {
    /// What the pack adds, in plain words.
    pub fn description(self) -> &'static str {
        match self {
            Self::Contact => {
                "Enquiries, newsletter sign-ups, prospectus requests, licensing \
                              enquiries, volunteering and service forms"
            }
            Self::Quote => "Quotes at the release's published prices",
            Self::Booking => "Availability, bookings, appointments and registrations",
            Self::Order => {
                "The catalogue, orders, payments, returns, donations, subscriptions \
                            and tickets"
            }
            Self::Search => "Search over the current release",
            Self::Components => {
                "Read-only components: practice information, status lookups, \
                                 notices, events and schedules"
            }
        }
    }

    /// The built-in handler serving the built-in capability `name` (spec 3.5 §7.23); `None`
    /// for a name no built-in pack declares.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::packs::Pack;
    /// use adk_website::packs::bundle::{HandlerConfig, OrderOperation};
    ///
    /// assert_eq!(Pack::handler("place_order"), Some(HandlerConfig::Order { operation: OrderOperation::Place }));
    /// assert_eq!(Pack::handler("delete_everything"), None);
    /// ```
    pub fn handler(name: &str) -> Option<HandlerConfig> {
        Some(match name {
            "submit_inquiry"
            | "subscribe_newsletter"
            | "request_prospectus"
            | "licensing_enquiry"
            | "volunteer"
            | "submit_service_form"
            | "register" => HandlerConfig::Form,
            "get_quote" => HandlerConfig::Quote,
            "check_availability" => {
                HandlerConfig::Booking { operation: BookingOperation::Availability }
            }
            "book" | "book_appointment" => {
                HandlerConfig::Booking { operation: BookingOperation::Book }
            }
            "list_products" => HandlerConfig::Listing { content: ContentKind::Product },
            "place_order" => HandlerConfig::Order { operation: OrderOperation::Place },
            "order_status" => HandlerConfig::Order { operation: OrderOperation::Status },
            "request_return" => HandlerConfig::Order { operation: OrderOperation::Return },
            "pay_order" | "donate" | "subscribe" | "buy_ticket" => HandlerConfig::Payment,
            "search_site" => HandlerConfig::Search,
            "practice_information" => HandlerConfig::Listing { content: ContentKind::Service },
            "lookup_status" => HandlerConfig::Status,
            "list_notices" => HandlerConfig::Listing { content: ContentKind::Notice },
            "list_events" | "get_schedule" => {
                HandlerConfig::Listing { content: ContentKind::Event }
            }
            _ => return None,
        })
    }

    /// The pack's signed bundle as it ships in the crate's `packs/` directory.
    pub fn bundle_json(self) -> &'static str {
        match self {
            Self::Contact => include_str!("../../packs/contact.pack.json"),
            Self::Quote => include_str!("../../packs/quote.pack.json"),
            Self::Booking => include_str!("../../packs/booking.pack.json"),
            Self::Order => include_str!("../../packs/order.pack.json"),
            Self::Search => include_str!("../../packs/search.pack.json"),
            Self::Components => include_str!("../../packs/components.pack.json"),
        }
    }

    /// The pack's bundle, verified against [`builtin_trust`].
    ///
    /// # Errors
    ///
    /// The [`PackError`] of a shipped bundle that does not verify, which the crate's tests
    /// rule out.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::packs::Pack;
    ///
    /// let booking = Pack::Booking.bundle().unwrap();
    /// assert_eq!(booking.bundle.publisher.id, "adk-website");
    /// assert_eq!(booking.bundle.capabilities, Pack::Booking.capabilities());
    /// ```
    pub fn bundle(self) -> Result<VerifiedPack, PackError> {
        PackBundle::verify(self.bundle_json().as_bytes(), &builtin_trust())
    }

    /// The pack's bundle before it is sealed: its declarations and handlers.
    pub fn unsigned_bundle(self) -> PackBundle {
        let publisher = PackPublisher::new(BUILTIN_PUBLISHER).named("ADK-Rust adk-website");
        // Every name a pack lists has a handler (`the_built_in_handlers_cover_every_capability`).
        self.capabilities()
            .into_iter()
            .filter_map(|declaration| Some((Self::handler(&declaration.name)?, declaration)))
            .fold(
                PackBundle::new(self.as_str(), BUILTIN_VERSION, publisher, self.description()),
                |bundle, (handler, declaration)| bundle.capability(declaration, handler),
            )
    }

    /// The pack's bundle sealed with the built-in test key, as the shipped file holds it.
    ///
    /// # Errors
    ///
    /// [`PackError::Malformed`] when a declaration is not I-JSON.
    pub async fn sealed_bundle(self) -> Result<PackBundle, PackError> {
        self.unsigned_bundle().seal(&builtin_test_key()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_handlers_cover_every_capability() {
        for pack in Pack::ALL {
            for name in pack.names() {
                assert!(Pack::handler(name).is_some(), "{name}");
            }
        }
    }

    /// The shipped bundles are the generated ones; `ADK_WEBSITE_PACKS_RECORD=1` writes them.
    #[tokio::test]
    async fn the_shipped_bundles_are_the_generated_ones() {
        let record = std::env::var("ADK_WEBSITE_PACKS_RECORD").is_ok_and(|v| v == "1");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packs");
        for pack in Pack::ALL {
            let generated = pack.sealed_bundle().await.unwrap().to_json().unwrap();
            let generated = String::from_utf8(generated).unwrap();
            if record {
                std::fs::write(dir.join(format!("{}.pack.json", pack.as_str())), &generated)
                    .unwrap();
                continue;
            }
            // Compared as JSON values: key order follows serde_json's `preserve_order`, which
            // other workspace crates turn on, and the digest and proof are over JCS anyway.
            let shipped: serde_json::Value = serde_json::from_str(pack.bundle_json()).unwrap();
            let generated: serde_json::Value = serde_json::from_str(&generated).unwrap();
            assert_eq!(
                shipped,
                generated,
                "packs/{}.pack.json is not the generated bundle: run the test with \
                 ADK_WEBSITE_PACKS_RECORD=1, and raise BUILTIN_VERSION when its content changed",
                pack.as_str()
            );
        }
    }

    #[test]
    fn every_shipped_bundle_verifies_with_the_built_in_trust_only() {
        for pack in Pack::ALL {
            let verified = pack.bundle().unwrap();
            assert_eq!(verified.bundle.name, pack.as_str());
            assert_eq!(verified.bundle.version, BUILTIN_VERSION);
            assert_eq!(verified.bundle.capabilities, pack.capabilities());
            for declaration in &verified.bundle.capabilities {
                assert_eq!(
                    verified.bundle.handlers.get(&declaration.name).copied(),
                    Pack::handler(&declaration.name)
                );
            }
            assert_eq!(
                PackBundle::verify(pack.bundle_json().as_bytes(), &TrustList::new())
                    .unwrap_err()
                    .code(),
                "unknown_publisher"
            );
        }
        assert!(is_builtin_test_key(&builtin_test_key().public_key()));
    }
}
