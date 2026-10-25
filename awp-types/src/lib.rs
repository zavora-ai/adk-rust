//! # awp-types
//!
//! Shared protocol types for the Agentic Web Protocol (AWP).
//!
//! This crate provides pure protocol types with **zero `adk-*` dependencies**,
//! making it importable by any Rust project that needs to work with AWP messages.
//!
//! ## Types
//!
//! - [`AwpVersion`] / [`CURRENT_VERSION`] — protocol version with compatibility checks
//! - [`TrustLevel`] — request trust classification (Anonymous, Known, Partner, Internal)
//! - [`RequesterType`] — human vs. agent requester distinction
//! - [`AwpError`] — structured error codes with HTTP status mapping
//! - [`AwpRequest`] / [`AwpResponse`] — protocol request/response envelopes
//! - [`A2aMessage`] / [`A2aMessageType`] — agent-to-agent communication
//! - [`AwpDiscoveryDocument`] — well-known discovery endpoint payload
//! - [`CapabilityManifest`] / [`CapabilityEntry`] — JSON-LD capability descriptions
//! - [`BusinessContext`] / [`BusinessCapability`] / [`BusinessPolicy`] — site configuration
//! - [`PaymentIntent`] / [`PaymentIntentState`] / [`PaymentPolicy`] — owner-policy-driven payments
//! - [`AwpTypedMessage`] / [`AwpMessageType`] — typed messages for agent routing
//!
//! ## AWP 2.0
//!
//! The [`v2`] modules hold the AWP 2.0 wire types (spec 3.5 §7.2–§7.8.3, §7.10), all
//! re-exported here beside the 1.0 names, which are unchanged:
//!
//! | Area | Types |
//! |---|---|
//! | Site contract | [`SiteContract`], [`SiteRef`], [`Profile`], [`SurfaceLinks`], [`ContractRelease`], [`ContractStatus`], [`EventsLink`], [`PaymentsLink`], [`CharterLink`], [`ContactLinks`] |
//! | Keys and proofs | [`Proof`], [`KeySet`], [`RootKey`], [`Delegation`], [`KeyScope`], [`PublicKeyJwk`] |
//! | Identity | [`IdentityPolicy`], [`IdentityMethod`], [`TrustTier`], [`TrustRule`], [`RequesterId`] |
//! | Capabilities | [`CapabilityV2`], [`CapabilityManifestV2`], [`MachineView`], [`HeldCall`] |
//! | Events and status | [`EventEnvelope`], [`EventType`], [`EventCatalog`], [`SiteStatus`], [`SiteState`] |
//! | Consent and commerce | [`ConsentReceipt`], [`ConsentScope`], [`Receipt`], [`QuoteDocument`] |
//! | Errors | [`ErrorEnvelope`], [`ErrorBody`], [`ErrorCode`] |
//! | Releases | [`ReleaseManifest`], [`FileEntry`], [`Digest`], [`ReleaseSource`], [`HostLimits`] |
//! | Charter | [`Charter`], [`SiteKind`], [`Governance`], [`Kpi`], [`Guardrail`], [`AutonomyLevel`], [`PublicTerms`], [`PriceRule`] |
//! | Signals | [`Signal`], [`SignalKind`], [`Bounded`] |
//! | Content | [`ContentItem`], [`ContentKind`], [`ContentState`], [`BatchPolicy`], [`FeedBounds`] |
//!
//! The `json-schema` feature derives `schemars::JsonSchema` for every 2.0 type and adds
//! `v2::schema::wire_schemas`.
//!
//! ## Serialization
//!
//! Wire structs use `#[serde(rename_all = "camelCase")]` for JSON serialization; enum
//! values are lowercase or snake_case.
//! [`BusinessContext`] uses standard snake_case for TOML configuration files.

mod a2a;
mod business;
mod capability;
mod discovery;
mod error;
mod payment;
mod request;
mod requester;
mod response;
mod trust;
pub mod v2;
mod version;

pub use a2a::{A2aMessage, A2aMessageType, AwpMessageType, AwpTypedMessage};
pub use business::{
    BrandVoice, BusinessCapability, BusinessContext, BusinessIdentity, BusinessPolicy,
    ChannelConfig, ContentConfig, OutreachConfig, PaymentConfig, Product, ReviewConfig,
    SupportConfig,
};
pub use capability::{CapabilityEntry, CapabilityManifest};
pub use discovery::AwpDiscoveryDocument;
pub use error::AwpError;
pub use payment::{PaymentIntent, PaymentIntentState, PaymentPolicy, PaymentPolicyDecision};
pub use request::AwpRequest;
pub use requester::RequesterType;
pub use response::AwpResponse;
pub use trust::TrustLevel;
pub use v2::{
    AbuseKind, ActionClass, ApprovalChain, ApprovalHint, Atomicity, AutonomyLevel, BatchPolicy,
    Bound, Bounded, BoundedError, Breach, Budget, Cadence, CapabilityEffect, CapabilityManifestV2,
    CapabilityV2, CatalogChanged, CatalogChangesPage, Channel, Charter, CharterError, CharterLink,
    CharterViolation, ConsentReceipt, ConsentReceiptRef, ConsentRequirement, ConsentRevoked,
    ConsentScope, ContactLinks, ContactRef, ContentId, ContentItem, ContentKind, ContentState,
    ContractRelease, ContractStatus, Delegation, Deprecation, Digest, DigestError, Direction,
    ErrorBody, ErrorCode, ErrorEnvelope, EventCatalog, EventEnvelope, EventSubscription, EventType,
    EventsLink, FactRef, FeedBounds, FileEntry, GeneratorRef, Goal, Governance, Guardrail,
    GuardrailAction, HeldCall, HeldCallState, HostLimits, IdempotencySource, IdentityMethod,
    IdentityPolicy, JobAuthority, JobLimits, KeyScope, KeySet, Kpi, LanguageTag, LanguageTagError,
    LeadRef, LegalBasis, LineItem, LogEntry, MachineView, Markdown, Measurement, MediaRef,
    MessageCategory, MetricRef, OrderUpdated, Outcome, OwnerSignature, PaymentsLink, PolicyRef,
    Price, PriceRule, Profile, Proof, PublicKeyJwk, PublicTerms, QuietHours, QuoteDocument, Rate,
    RateClass, ReasonCode, Receipt, RedLine, RedLineKind, Redirect, ReleaseManifest,
    ReleasePublished, ReleaseRef, ReleaseSource, Reporting, RequesterId, RetiredRoot, Revocation,
    Role, RoleHolder, RootEndorsement, RootKey, Schedule, Signal, SignalKind, SiteContract,
    SiteKind, SiteRef, SiteState, SiteStatus, SourceRef, SpendCategory, StatusChanged,
    StrategyPillar, StreamEntry, SubscriptionState, SubscriptionSuspended, SurfaceLinks, Target,
    ThreadRef, Trigger, TrustRule, TrustRules, TrustTier, Urgency, UrgencyRoute, VERSION_2_0,
    Voice, without_proof,
};
pub use version::{AwpVersion, CURRENT_VERSION, ParseVersionError};
