//! AWP 2.0 wire types (spec 3.5 §7.2–§7.8.3, §7.10).
//!
//! AWP 2.0 is the signed contract between a website and the people and agents who use it.
//! These modules hold its documents and records as plain data: the site contract, keys and
//! proofs, identity and trust tiers, capabilities and machine views, held calls, events, site
//! status, consent receipts, commerce receipts and quotes, the error envelope, release
//! manifests, the charter, signals, and the content and batching types that appear on the
//! wire. Cryptography and canonicalisation are not here: they live in `adk-awp` and
//! `adk-auth`.
//!
//! Every type is re-exported at the crate root beside the AWP 1.0 types, whose names are
//! unchanged.
//!
//! # Serialization
//!
//! Wire structs use camelCase field names, enum values are snake_case unless the spec names
//! another spelling (`static+edge`, `web-bot-auth`, `EdDSA`), and optional fields are omitted
//! when absent. Durations are whole seconds.
//!
//! # Example
//!
//! ```
//! use awp_types::{SiteContract, SiteRef, TrustTier};
//! use uuid::Uuid;
//!
//! let contract = SiteContract::new(SiteRef {
//!     id: Uuid::nil(),
//!     name: "Example Solar".into(),
//!     domain: "example.com".into(),
//!     languages: vec!["en".parse().unwrap()],
//! });
//! let json = serde_json::to_value(&contract).unwrap();
//! assert_eq!(json["awp"], "2.0");
//! assert!(TrustTier::Public < TrustTier::Partner);
//! ```

/// Declares a string-valued wire enum whose known values are listed and whose other values
/// survive a round trip unchanged (spec 3.5 §7.5 "plus custom").
///
/// Equality, ordering and hashing compare the wire value, so `Other("x")` equals the known
/// variant whose value is `"x"`.
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $value:literal, )*
            @other $(#[$ometa:meta])* $other:ident(String),
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        #[non_exhaustive]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )*
            $(#[$ometa])* $other(String),
        }

        impl $name {
            /// The wire values this version of the protocol names.
            pub const KNOWN: &'static [&'static str] = &[$($value),*];

            /// Returns the wire value.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $value, )*
                    Self::$other(value) => value.as_str(),
                }
            }

            /// Returns `true` when the value is one this version of the protocol names.
            pub fn is_known(&self) -> bool {
                Self::KNOWN.contains(&self.as_str())
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $( $value => Self::$variant, )*
                    other => Self::$other(other.to_owned()),
                }
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                match value.as_str() {
                    $( $value => Self::$variant, )*
                    _ => Self::$other(value),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.as_str() == other.as_str()
            }
        }

        impl Eq for $name {}

        impl std::hash::Hash for $name {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.as_str().hash(state);
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.as_str().cmp(other.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer).map(Self::from)
            }
        }

        #[cfg(feature = "json-schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "minLength": 1,
                    "examples": [$($value),*]
                })
            }
        }
    };
}

pub mod capability;
pub mod charter;
pub mod consent;
pub mod content;
pub mod contract;
pub mod error;
pub mod event;
pub mod held;
pub mod identity;
pub mod receipt;
pub mod release;
#[cfg(feature = "json-schema")]
pub mod schema;
pub mod signal;
pub mod status;

pub use capability::{
    ApprovalHint, CapabilityEffect, CapabilityManifestV2, CapabilityV2, ConsentRequirement,
    Deprecation, IdempotencySource, MachineView, Price,
};
pub use charter::{
    ActionClass, ApprovalChain, AutonomyLevel, Breach, Budget, Cadence, Charter, CharterError,
    CharterViolation, Direction, Goal, Governance, Guardrail, GuardrailAction, JobAuthority,
    JobLimits, Kpi, Measurement, MetricRef, OwnerSignature, PolicyRef, PriceRule, PublicTerms,
    QuietHours, RedLine, RedLineKind, Reporting, Role, RoleHolder, SiteKind, SourceRef,
    SpendCategory, StrategyPillar, Target, Urgency, UrgencyRoute, Voice,
};
pub use consent::{ConsentReceipt, ConsentScope, LegalBasis};
pub use content::{
    BatchPolicy, Bound, ContentId, ContentItem, ContentKind, ContentState, FactRef, FeedBounds,
    LanguageTag, LanguageTagError, Markdown, MediaRef, Rate, Schedule, StreamEntry, Trigger,
};
pub use contract::{
    CharterLink, ContactLinks, ContractRelease, ContractStatus, EventsLink, PaymentsLink, Profile,
    ReleaseRef, SiteContract, SiteRef, SurfaceLinks, VERSION_2_0,
};
pub use error::{ErrorBody, ErrorCode, ErrorEnvelope};
pub use event::{
    CatalogChanged, CatalogChangesPage, ConsentRevoked, EventCatalog, EventEnvelope,
    EventSubscription, EventType, OrderUpdated, ReleasePublished, StatusChanged, SubscriptionState,
    SubscriptionSuspended,
};
pub use held::{HeldCall, HeldCallState};
pub use identity::{
    Delegation, IdentityMethod, IdentityPolicy, KeyScope, KeySet, Proof, PublicKeyJwk, RateClass,
    RequesterId, RetiredRoot, Revocation, RootEndorsement, RootKey, TrustRule, TrustRules,
    TrustTier, without_proof,
};
pub use receipt::{LineItem, QuoteDocument, Receipt};
pub use release::{
    Atomicity, Digest, DigestError, FileEntry, GeneratorRef, HostLimits, LogEntry, Redirect,
    ReleaseManifest, ReleaseSource,
};
pub use signal::{
    AbuseKind, Bounded, BoundedError, Channel, ConsentReceiptRef, ContactRef, LeadRef,
    MessageCategory, Outcome, ReasonCode, Signal, SignalKind, ThreadRef,
};
pub use status::{SiteState, SiteStatus};

/// Serde helpers shared by the 2.0 modules.
pub(crate) mod serde_util {
    /// A [`std::time::Duration`] as whole seconds.
    pub(crate) mod duration_secs {
        use serde::{Deserialize, Deserializer, Serializer};
        use std::time::Duration;

        pub(crate) fn serialize<S: Serializer>(
            duration: &Duration,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serializer.serialize_u64(duration.as_secs())
        }

        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Duration, D::Error> {
            u64::deserialize(deserializer).map(Duration::from_secs)
        }
    }

    /// An [`AwpVersion`](crate::AwpVersion) as its `"major.minor"` string.
    pub(crate) mod version_string {
        use serde::{Deserialize, Deserializer, Serializer, de::Error};

        use crate::AwpVersion;

        pub(crate) fn serialize<S: Serializer>(
            version: &AwpVersion,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serializer.collect_str(version)
        }

        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<AwpVersion, D::Error> {
            String::deserialize(deserializer)?.parse().map_err(D::Error::custom)
        }
    }

    /// `skip_serializing_if` predicate for `bool` fields that default to `false`.
    pub(crate) fn is_false(value: &bool) -> bool {
        !*value
    }
}
