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
//!
//! ## Serialization
//!
//! All wire types use `#[serde(rename_all = "camelCase")]` for JSON serialization.
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
pub use version::{AwpVersion, CURRENT_VERSION, ParseVersionError};
