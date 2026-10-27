//! The capability pack bundle (spec 3.5 §7.23): one JSON document that carries a pack's
//! capability declarations, the configuration of the handlers that serve them, its playbooks
//! and its driver manifests, sealed with a content digest and its publisher's signature.
//!
//! ```json
//! {
//!   "format": "adk-website.pack/1",
//!   "name": "waitlist",
//!   "version": "1.0.0",
//!   "publisher": { "id": "tables.example", "name": "Tables" },
//!   "description": "A waiting list for fully booked evenings",
//!   "capabilities": [ { "name": "join_waitlist", "effect": "non_idempotent", "…": "…" } ],
//!   "handlers": { "join_waitlist": { "handler": "form" } },
//!   "playbooks": [ { "id": "waitlist", "job": "capture", "agents": ["concierge"], "text": "---\nname: waitlist\n…" } ],
//!   "drivers": [ { "id": "tables-pos", "manifest": "id = \"tables-pos\"\n…" } ],
//!   "digest": "sha256:…",
//!   "proof": { "alg": "EdDSA", "kid": "tables-2026", "jws": "<detached JWS>" }
//! }
//! ```
//!
//! | Member | Holds |
//! |---|---|
//! | `digest` | The content digest: the SHA-256 of the JCS form of the bundle without `digest` and `proof` (AWP 2.0 §1.2) |
//! | `proof` | The publisher's detached JWS (EdDSA) over the JCS form of the bundle without `proof`, so it covers the digest (AWP 2.0 §5.4) |
//!
//! A pack runs no code in the site's process: each capability is served by a built-in handler
//! its [`HandlerConfig`] names, and a pack's drivers are MCP servers the kernel runs sandboxed
//! once the owner mounts them. [`PackBundle::verify`] authenticates a bundle against a
//! [`TrustList`] before [`PackBundle::check`] judges its content; installation refuses a
//! bundle that is unsigned, tampered, from an unknown publisher, invalid, or in conflict with
//! the site's packs.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_awp::signing::jws::{DocumentSigner, document_digest, jwk_thumbprint, sign, verify};
use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityTool};
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::drivers::{DriverManifest, DriverSandbox, EffectClass};
use async_trait::async_trait;
use awp_types::{CapabilityEffect, CapabilityV2, ContentKind, Digest, Proof};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::trust::TrustList;
use crate::team::AgentRole;

/// The bundle format this release writes and reads (spec 3.5 §7.23).
pub const PACK_FORMAT: &str = "adk-website.pack/1";

/// The largest bundle file installation reads.
pub const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024;

const MAX_PLAYBOOK_BYTES: usize = 64 * 1024;
const MAX_CAPABILITIES: usize = 256;
const MAX_PLAYBOOKS: usize = 64;
const MAX_DRIVERS: usize = 32;
const MAX_DESCRIPTION_CHARS: usize = 2_000;

/// Why a pack is refused (spec 3.5 §7.23). [`code`](Self::code) names each refusal for
/// scripts and conformance vectors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// The file is not a pack bundle.
    #[error("not a capability pack bundle: {0}")]
    Malformed(String),
    /// The bundle's format is not one this release reads.
    #[error("the bundle's format is `{0}`; this release reads `{PACK_FORMAT}`")]
    Format(String),
    /// The bundle carries no signature.
    #[error(
        "the pack is not signed: a pack installs only with its publisher's signature over its \
         content digest; ask the publisher for the signed bundle"
    )]
    Unsigned,
    /// The content does not match its digest, or the signature does not verify.
    #[error(
        "the pack's content is not what its publisher signed: {0}; get the pack again from \
         its publisher"
    )]
    Tampered(String),
    /// No trusted key of the trust list belongs to the bundle's publisher and key id.
    #[error(
        "the pack's publisher `{publisher}` (key `{kid}`) is not in the trust list; add the \
         publisher's public key with `adk-rust site packs trust` only if you trust them"
    )]
    UnknownPublisher {
        /// The publisher the bundle names.
        publisher: String,
        /// The key id its proof names.
        kid: String,
    },
    /// The offered version is older than the installed one.
    #[error(
        "`{name}` {installed} is installed and {offered} is older; a pack is never downgraded \
         in place: remove it first to go back"
    )]
    Downgrade {
        /// The pack.
        name: String,
        /// The installed version.
        installed: String,
        /// The offered version.
        offered: String,
    },
    /// The offered version is installed with other content.
    #[error(
        "`{name}` {version} is installed with other content; a published version never \
         changes, so its publisher must release a new version"
    )]
    Republished {
        /// The pack.
        name: String,
        /// The version.
        version: String,
    },
    /// The content breaks a rule of the format.
    #[error("the pack is invalid: {0}")]
    Invalid(String),
    /// The pack takes a name, capability, playbook or driver another pack holds.
    #[error("the pack conflicts with the site's packs: {0}")]
    Conflict(String),
    /// No installed pack has the name.
    #[error("no installed pack is named `{0}`; `adk-rust site packs list` shows the site's packs")]
    NotInstalled(String),
    /// Built-in packs are not installed or removed.
    #[error(
        "`{0}` is a built-in pack: it ships with adk-website, and the site's kind chooses which \
         of its capabilities the site offers"
    )]
    BuiltIn(String),
    /// A file could not be read or written.
    #[error("{0}")]
    Io(String),
}

impl PackError {
    /// The refusal's code: `malformed`, `format`, `unsigned`, `tampered`, `unknown_publisher`,
    /// `downgrade`, `republished`, `invalid`, `conflict`, `not_installed`, `built_in` or `io`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::Format(_) => "format",
            Self::Unsigned => "unsigned",
            Self::Tampered(_) => "tampered",
            Self::UnknownPublisher { .. } => "unknown_publisher",
            Self::Downgrade { .. } => "downgrade",
            Self::Republished { .. } => "republished",
            Self::Invalid(_) => "invalid",
            Self::Conflict(_) => "conflict",
            Self::NotInstalled(_) => "not_installed",
            Self::BuiltIn(_) => "built_in",
            Self::Io(_) => "io",
        }
    }
}

impl From<PackError> for AdkError {
    fn from(error: PackError) -> Self {
        let (category, code) = match &error {
            PackError::Malformed(_) => (ErrorCategory::InvalidInput, "website.pack.malformed"),
            PackError::Format(_) => (ErrorCategory::Unsupported, "website.pack.format"),
            PackError::Unsigned => (ErrorCategory::Forbidden, "website.pack.unsigned"),
            PackError::Tampered(_) => (ErrorCategory::Forbidden, "website.pack.tampered"),
            PackError::UnknownPublisher { .. } => {
                (ErrorCategory::Forbidden, "website.pack.unknown_publisher")
            }
            PackError::Downgrade { .. } => (ErrorCategory::Conflict, "website.pack.downgrade"),
            PackError::Republished { .. } => (ErrorCategory::Conflict, "website.pack.republished"),
            PackError::Invalid(_) => (ErrorCategory::InvalidInput, "website.pack.invalid"),
            PackError::Conflict(_) => (ErrorCategory::Conflict, "website.pack.conflict"),
            PackError::NotInstalled(_) => (ErrorCategory::NotFound, "website.pack.not_installed"),
            PackError::BuiltIn(_) => (ErrorCategory::InvalidInput, "website.pack.built_in"),
            PackError::Io(_) => (ErrorCategory::Internal, "website.pack.io"),
        };
        AdkError::new(ErrorComponent::Deploy, category, code, error.to_string())
    }
}

/// Who publishes a pack (spec 3.5 §7.23): the id a trust list names and a display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackPublisher {
    /// The publisher's id: lowercase letters, digits, `.` and `-`, such as `tables.example`.
    pub id: String,
    /// The publisher's name, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl PackPublisher {
    /// The publisher `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), name: None }
    }

    /// With a display name.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// What an order handler does with a call (spec 3.5 §7.16 `order`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderOperation {
    /// Places an order against an open quote, as `place_order` does.
    Place,
    /// Reports an order's status, as `order_status` does.
    Status,
    /// Requests a return, as `request_return` does.
    Return,
}

/// What a booking handler does with a call (spec 3.5 §7.16 `booking`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookingOperation {
    /// Lists open slots, as `check_availability` does.
    Availability,
    /// Takes a slot and keeps the contact, as `book` does.
    Book,
}

/// The built-in handler that serves a capability, with its configuration (spec 3.5 §7.23
/// "handlers"). A pack configures handlers and ships no handler code: the edge serves each
/// capability with the handler named here, over the site's own stores, calendar and payment
/// desk, after its governed path has decided the call.
///
/// | Handler | Serves | Effect at least | Input members it reads |
/// |---|---|---|---|
/// | `form` | Enquiries and sign-ups: the contact, a lead and a `lead_captured` signal | `idempotent` | — |
/// | `quote` | Quotes at the release's prices | `idempotent` | `items` |
/// | `order` with `operation` `place`, `status` or `return` | Orders against quotes | `non_idempotent`; `read_only` for `status` | `quoteId`; `orderId` |
/// | `payment` | Payments through the owner's payment desk | `non_idempotent` | — |
/// | `booking` with `operation` `availability` or `book` | The owner's calendar | `read_only`; `non_idempotent` for `book` | —; `slot` |
/// | `search` | The release's index | `read_only` | `query` |
/// | `listing` with `content` | The published items of a content kind | `read_only` | — |
/// | `status` | An application's status by its reference | `read_only` | `reference` |
///
/// # Example
///
/// ```rust
/// use adk_website::packs::bundle::{BookingOperation, HandlerConfig};
///
/// let config: HandlerConfig =
///     serde_json::from_str(r#"{"handler": "booking", "operation": "book"}"#).unwrap();
/// assert_eq!(config, HandlerConfig::Booking { operation: BookingOperation::Book });
/// assert_eq!(config.reads(), ["slot"]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "handler", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HandlerConfig {
    /// The forms handler.
    Form,
    /// The quotes handler.
    Quote,
    /// The orders handler.
    Order {
        /// What it does with the call.
        operation: OrderOperation,
    },
    /// The payments handler.
    Payment,
    /// The bookings handler.
    Booking {
        /// What it does with the call.
        operation: BookingOperation,
    },
    /// The search handler.
    Search,
    /// The listings handler.
    Listing {
        /// The content kind it lists.
        content: ContentKind,
    },
    /// The application status handler.
    Status,
}

impl HandlerConfig {
    /// The handler's name, with its operation or content kind, such as `order/place`.
    pub fn label(&self) -> String {
        match self {
            Self::Form => "form".into(),
            Self::Quote => "quote".into(),
            Self::Order { operation } => format!("order/{}", json_name(operation)),
            Self::Payment => "payment".into(),
            Self::Booking { operation } => format!("booking/{}", json_name(operation)),
            Self::Search => "search".into(),
            Self::Listing { content } => format!("listing/{}", json_name(content)),
            Self::Status => "status".into(),
        }
    }

    /// The least strict effect a capability this handler serves may declare: a handler that
    /// writes never serves a capability declared read-only.
    pub fn minimum_effect(&self) -> CapabilityEffect {
        match self {
            Self::Form | Self::Quote => CapabilityEffect::Idempotent,
            Self::Order { operation: OrderOperation::Place | OrderOperation::Return }
            | Self::Payment
            | Self::Booking { operation: BookingOperation::Book } => {
                CapabilityEffect::NonIdempotent
            }
            Self::Order { operation: OrderOperation::Status }
            | Self::Booking { operation: BookingOperation::Availability }
            | Self::Search
            | Self::Listing { .. }
            | Self::Status => CapabilityEffect::ReadOnly,
        }
    }

    /// The input members the handler reads, which the capability's input schema must declare.
    pub fn reads(&self) -> &'static [&'static str] {
        match self {
            Self::Quote => &["items"],
            Self::Order { operation: OrderOperation::Place } => &["quoteId"],
            Self::Order { operation: OrderOperation::Status | OrderOperation::Return } => {
                &["orderId"]
            }
            Self::Booking { operation: BookingOperation::Book } => &["slot"],
            Self::Search => &["query"],
            Self::Status => &["reference"],
            Self::Form
            | Self::Payment
            | Self::Booking { operation: BookingOperation::Availability }
            | Self::Listing { .. } => &[],
        }
    }

    /// The built-in capability whose name the shared handler dispatches on, when a pack's
    /// capability of another name must reach it as that one: the order operations and
    /// availability.
    pub fn dispatches_as(&self) -> Option<&'static str> {
        match self {
            Self::Order { operation: OrderOperation::Place } => Some("place_order"),
            Self::Order { operation: OrderOperation::Status } => Some("order_status"),
            Self::Order { operation: OrderOperation::Return } => Some("request_return"),
            Self::Booking { operation: BookingOperation::Availability } => {
                Some("check_availability")
            }
            Self::Form
            | Self::Quote
            | Self::Payment
            | Self::Booking { operation: BookingOperation::Book }
            | Self::Search
            | Self::Listing { .. }
            | Self::Status => None,
        }
    }
}

/// A playbook a pack adds (spec 3.5 §7.15, §7.23): a skill file for a job's agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackPlaybook {
    /// The skill's name, which its front matter's `name` repeats.
    pub id: String,
    /// The job it changes how is done (spec 3.5 §7.20).
    pub job: String,
    /// The agents that use it; each runs the job.
    pub agents: Vec<AgentRole>,
    /// The skill file: YAML front matter with `name` and `description`, then the body.
    pub text: String,
}

/// A driver manifest a pack ships (spec 3.5 §7.13, §7.23): the kernel's TOML manifest of an
/// MCP server, mounted only by the owner (`adk-rust drivers mount`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackDriver {
    /// The driver's id, which the manifest's `id` repeats.
    pub id: String,
    /// The manifest, as `adk-rust drivers mount` reads it.
    pub manifest: String,
}

/// A capability pack bundle (spec 3.5 §7.23).
///
/// # Example
///
/// ```rust
/// use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
/// use adk_website::packs::bundle::{HandlerConfig, PackBundle, PackPublisher};
/// use adk_website::packs::trust::{TrustList, TrustedKey};
/// use awp_types::{CapabilityEffect, CapabilityV2, ContentKind};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let key = Ed25519DocumentSigner::from_seed("tables-2026", [4; 32]).unwrap();
/// let menu = CapabilityV2::new("list_menu", "Tonight's menu", CapabilityEffect::ReadOnly);
/// let bundle = PackBundle::new("menu", "1.0.0", PackPublisher::new("tables.example"), "The menu")
///     .capability(menu, HandlerConfig::Listing { content: ContentKind::Product });
/// bundle.check().unwrap();
/// let sealed = bundle.seal(&key).await.unwrap();
///
/// let trust = TrustList::new().with(TrustedKey::new("tables.example", "tables-2026", key.public_key()));
/// let verified = PackBundle::verify(&sealed.to_json().unwrap(), &trust).unwrap();
/// assert_eq!(verified.bundle.name, "menu");
/// assert!(PackBundle::verify(&sealed.to_json().unwrap(), &TrustList::new()).is_err());
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackBundle {
    /// The bundle format, [`PACK_FORMAT`].
    pub format: String,
    /// The pack's name: lowercase letters, digits and `-`.
    pub name: String,
    /// The pack's version (Semantic Versioning, no build metadata).
    pub version: String,
    /// Its publisher.
    pub publisher: PackPublisher,
    /// What it adds, for the person installing it.
    pub description: String,
    /// The capabilities it declares (spec 3.5 §7.4).
    #[serde(default)]
    pub capabilities: Vec<CapabilityV2>,
    /// The handler serving each capability, by capability name.
    #[serde(default)]
    pub handlers: std::collections::BTreeMap<String, HandlerConfig>,
    /// The playbooks it adds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playbooks: Vec<PackPlaybook>,
    /// The driver manifests it ships.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<PackDriver>,
    /// The content digest: SHA-256 of the JCS form without `digest` and `proof`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    /// The publisher's detached JWS over the JCS form without `proof`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
}

/// A bundle whose digest and publisher signature verified against a trust list, and whose
/// content passed [`PackBundle::check`].
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedPack {
    /// The bundle.
    pub bundle: PackBundle,
    /// Its content digest.
    pub digest: Digest,
    /// The publisher's key id.
    pub kid: String,
    /// The RFC 7638 thumbprint of the publisher's key.
    pub thumbprint: String,
}

impl VerifiedPack {
    /// The pack's version.
    pub fn version(&self) -> semver::Version {
        // `check` parsed it before the pack was verified.
        semver::Version::parse(&self.bundle.version)
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
    }
}

fn valid_slug(text: &str, max: usize, extra: &[u8]) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || extra.contains(b))
}

/// Whether `name` is a pack, playbook or driver name: 1–64 lowercase letters, digits and `-`,
/// starting with a letter or digit.
pub fn is_valid_pack_name(name: &str) -> bool {
    valid_slug(name, 64, b"-")
}

/// Whether `id` is a publisher id: 1–128 lowercase letters, digits, `.` and `-`, starting with
/// a letter or digit.
pub fn is_valid_publisher_id(id: &str) -> bool {
    valid_slug(id, 128, b".-")
}

fn strictness(effect: CapabilityEffect) -> u8 {
    match effect {
        CapabilityEffect::ReadOnly => 0,
        CapabilityEffect::Idempotent => 1,
        // `NonIdempotent`, and any effect a later release adds, is the strictest.
        _ => 2,
    }
}

/// The JSON name of an enum value, such as `non_idempotent`.
fn json_name<T: Serialize + std::fmt::Debug>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        _ => format!("{value:?}"),
    }
}

/// The front matter's `name`, when the text opens with front matter holding a `name` and a
/// `description`.
fn front_matter_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let header = &rest[..end];
    if !header.lines().any(|line| line.starts_with("description:")) {
        return None;
    }
    header.lines().find_map(|line| {
        line.strip_prefix("name:").map(|value| value.trim().trim_matches('"').trim_matches('\''))
    })
}

/// A handler that refuses every call: [`PackBundle::check`] compiles each declaration with it.
struct Unserved;

#[async_trait]
impl CapabilityHandler for Unserved {
    async fn handle(&self, _call: CapabilityCall) -> Result<Value, CapabilityError> {
        Err(CapabilityError::Failed("not served".into()))
    }
}

impl PackBundle {
    /// An unsigned bundle with no content yet.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        publisher: PackPublisher,
        description: impl Into<String>,
    ) -> Self {
        Self {
            format: PACK_FORMAT.into(),
            name: name.into(),
            version: version.into(),
            publisher,
            description: description.into(),
            capabilities: Vec::new(),
            handlers: std::collections::BTreeMap::new(),
            playbooks: Vec::new(),
            drivers: Vec::new(),
            digest: None,
            proof: None,
        }
    }

    /// Adds `declaration`, served by `handler`.
    #[must_use]
    pub fn capability(mut self, declaration: CapabilityV2, handler: HandlerConfig) -> Self {
        self.handlers.insert(declaration.name.clone(), handler);
        self.capabilities.push(declaration);
        self
    }

    /// Adds `playbook`.
    #[must_use]
    pub fn playbook(mut self, playbook: PackPlaybook) -> Self {
        self.playbooks.push(playbook);
        self
    }

    /// Adds `driver`.
    #[must_use]
    pub fn driver(mut self, driver: PackDriver) -> Self {
        self.drivers.push(driver);
        self
    }

    /// The pack's version.
    ///
    /// # Errors
    ///
    /// [`PackError::Invalid`] when it is not a Semantic Version without build metadata.
    pub fn semver(&self) -> Result<semver::Version, PackError> {
        let version = semver::Version::parse(&self.version).map_err(|error| {
            PackError::Invalid(format!(
                "version `{}` is not a Semantic Version such as 1.2.0: {error}",
                self.version
            ))
        })?;
        if !version.build.is_empty() {
            return Err(PackError::Invalid(format!(
                "version `{}` carries build metadata, which orders no versions; drop the `+…`",
                self.version
            )));
        }
        Ok(version)
    }

    /// The content digest: the SHA-256 of the JCS form of the bundle without `digest` and
    /// `proof`.
    ///
    /// # Errors
    ///
    /// [`PackError::Malformed`] when the bundle is not I-JSON.
    pub fn content_digest(&self) -> Result<Digest, PackError> {
        let value =
            serde_json::to_value(self).map_err(|error| PackError::Malformed(error.to_string()))?;
        content_digest(&value)
    }

    /// Seals the bundle: sets its content digest and signs it with the publisher's `signer`
    /// (spec 3.5 §7.23, AWP 2.0 §5.4). Sealing judges nothing; [`check`](Self::check) does,
    /// and installation refuses a sealed bundle that fails it.
    ///
    /// # Errors
    ///
    /// [`PackError::Malformed`] when the bundle is not I-JSON; [`PackError::Io`] when the key
    /// cannot sign.
    pub async fn seal(mut self, signer: &dyn DocumentSigner) -> Result<Self, PackError> {
        self.digest = None;
        self.proof = None;
        self.digest = Some(self.content_digest()?);
        let proof = sign(&self, signer).await.map_err(|error| PackError::Io(error.to_string()))?;
        self.proof = Some(proof);
        Ok(self)
    }

    /// The bundle as its file: pretty JSON ending in a newline.
    ///
    /// # Errors
    ///
    /// [`PackError::Malformed`] when the bundle is not I-JSON.
    pub fn to_json(&self) -> Result<Vec<u8>, PackError> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| PackError::Malformed(error.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Reads a bundle file without verifying it.
    ///
    /// # Errors
    ///
    /// [`PackError::Malformed`] for a file over [`MAX_BUNDLE_BYTES`] or that is not a bundle;
    /// [`PackError::Format`] for another format.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackError> {
        Ok(Self::parse_value(bytes)?.0)
    }

    fn parse_value(bytes: &[u8]) -> Result<(Self, Value), PackError> {
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(PackError::Malformed(format!(
                "the file is {} bytes; a bundle is at most {MAX_BUNDLE_BYTES}",
                bytes.len()
            )));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| PackError::Malformed(format!("not JSON: {error}")))?;
        if let Some(format) = value.get("format").and_then(Value::as_str)
            && format != PACK_FORMAT
        {
            return Err(PackError::Format(format.to_string()));
        }
        let bundle: Self = serde_json::from_value(value.clone())
            .map_err(|error| PackError::Malformed(error.to_string()))?;
        Ok((bundle, value))
    }

    /// Verifies a bundle file against `trust` (spec 3.5 §7.23): it must carry a proof, its
    /// content must match its digest, its publisher's key must be in `trust`, the proof must
    /// verify with that key, and the content must pass [`check`](Self::check). The digest and
    /// signature are checked over the file's own JSON, so nothing the publisher did not sign
    /// is read.
    ///
    /// # Errors
    ///
    /// The [`PackError`] naming the first check that fails.
    pub fn verify(bytes: &[u8], trust: &TrustList) -> Result<VerifiedPack, PackError> {
        let (bundle, value) = Self::parse_value(bytes)?;
        let Some(proof) = bundle.proof.clone() else {
            return Err(PackError::Unsigned);
        };
        let Some(digest) = bundle.digest.clone() else {
            return Err(PackError::Malformed(
                "the bundle has a proof but no content digest".into(),
            ));
        };
        let computed = content_digest(&value)?;
        if computed != digest {
            return Err(PackError::Tampered(format!(
                "its content digest is {computed}, not the {digest} it names"
            )));
        }
        let key = trust.key(&bundle.publisher.id, &proof.kid).ok_or_else(|| {
            PackError::UnknownPublisher {
                publisher: bundle.publisher.id.clone(),
                kid: proof.kid.clone(),
            }
        })?;
        verify(&value, &proof, key).map_err(|error| {
            PackError::Tampered(format!(
                "the signature does not verify with key `{}`: {error}",
                proof.kid
            ))
        })?;
        bundle.check()?;
        Ok(VerifiedPack { thumbprint: jwk_thumbprint(key), kid: proof.kid, digest, bundle })
    }

    /// Checks the content against the format's rules (spec 3.5 §7.23): names and version;
    /// every capability declared once, compiling, served by a handler whose effect it does not
    /// understate and whose input it declares, asking for consent where it takes an email
    /// (A-I8) under the site's policy version; every playbook a skill file for a job its
    /// agents run, not one of the product's own; every driver manifest sandboxed, serving the
    /// public or back-office partition, with an effect class for every tool. Neither the
    /// signature nor the site's other packs are checked here.
    ///
    /// # Errors
    ///
    /// [`PackError::Format`], [`PackError::Invalid`] or, for a playbook the product holds,
    /// [`PackError::Conflict`], naming the first rule broken.
    pub fn check(&self) -> Result<(), PackError> {
        let invalid = |reason: String| Err(PackError::Invalid(reason));
        if self.format != PACK_FORMAT {
            return Err(PackError::Format(self.format.clone()));
        }
        if !is_valid_pack_name(&self.name) {
            return invalid(format!(
                "name `{}` is not 1–64 lowercase letters, digits and `-`",
                self.name
            ));
        }
        self.semver()?;
        if !is_valid_publisher_id(&self.publisher.id) {
            return invalid(format!(
                "publisher `{}` is not 1–128 lowercase letters, digits, `.` and `-`",
                self.publisher.id
            ));
        }
        let description = self.description.trim();
        if description.is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
            return invalid(format!(
                "the description must say what the pack adds, in at most {MAX_DESCRIPTION_CHARS} characters"
            ));
        }
        if self.capabilities.is_empty() && self.playbooks.is_empty() && self.drivers.is_empty() {
            return invalid("the pack adds no capability, playbook or driver".into());
        }
        if self.capabilities.len() > MAX_CAPABILITIES
            || self.playbooks.len() > MAX_PLAYBOOKS
            || self.drivers.len() > MAX_DRIVERS
        {
            return invalid(format!(
                "a pack holds at most {MAX_CAPABILITIES} capabilities, {MAX_PLAYBOOKS} playbooks and {MAX_DRIVERS} drivers"
            ));
        }
        self.check_capabilities()?;
        self.check_playbooks()?;
        self.check_drivers()
    }

    fn check_capabilities(&self) -> Result<(), PackError> {
        let invalid = |reason: String| Err(PackError::Invalid(reason));
        let mut names = BTreeSet::new();
        for declaration in &self.capabilities {
            let name = declaration.name.as_str();
            if !names.insert(name) {
                return invalid(format!("capability `{name}` is declared twice"));
            }
            CapabilityTool::new("pack.invalid", declaration.clone(), Arc::new(Unserved))
                .map_err(|error| PackError::Invalid(format!("capability `{name}`: {error}")))?;
            let Some(handler) = self.handlers.get(name) else {
                return invalid(format!(
                    "capability `{name}` names no handler; add it to `handlers`"
                ));
            };
            let least = handler.minimum_effect();
            if strictness(declaration.effect) < strictness(least) {
                return invalid(format!(
                    "capability `{name}` is declared `{}`, but the `{}` handler's calls are at \
                     least `{}`; declare the effect the handler has",
                    json_name(&declaration.effect),
                    handler.label(),
                    json_name(&least)
                ));
            }
            let properties = declaration
                .input_schema
                .as_ref()
                .and_then(|schema| schema.get("properties"))
                .and_then(Value::as_object);
            for member in handler.reads() {
                if !properties.is_some_and(|properties| properties.contains_key(*member)) {
                    return invalid(format!(
                        "capability `{name}`'s input schema declares no `{member}`, which the \
                         `{}` handler reads",
                        handler.label()
                    ));
                }
            }
            if properties.is_some_and(|properties| properties.contains_key("email"))
                && declaration.consent.is_none()
            {
                return invalid(format!(
                    "capability `{name}` takes an email address without asking for consent; \
                     declare `consent` (spec 3.5 A-I8)"
                ));
            }
            if let Some(consent) = &declaration.consent
                && consent.policy_version != super::POLICY_VERSION
            {
                return invalid(format!(
                    "capability `{name}`'s consent names policy version `{}`; the site's \
                     policy, its public terms, is version `{}`",
                    consent.policy_version,
                    super::POLICY_VERSION
                ));
            }
        }
        if let Some(stray) = self.handlers.keys().find(|name| !names.contains(name.as_str())) {
            return invalid(format!("handler for `{stray}`, which the pack does not declare"));
        }
        Ok(())
    }

    fn check_playbooks(&self) -> Result<(), PackError> {
        let invalid = |reason: String| Err(PackError::Invalid(reason));
        let mut ids = BTreeSet::new();
        for playbook in &self.playbooks {
            let id = playbook.id.as_str();
            if !is_valid_pack_name(id) {
                return invalid(format!(
                    "playbook `{id}` is not named with 1–64 lowercase letters, digits and `-`"
                ));
            }
            if !ids.insert(id) {
                return invalid(format!("playbook `{id}` appears twice"));
            }
            if crate::playbooks::playbook(id).is_some() {
                return Err(PackError::Conflict(format!(
                    "playbook `{id}` is the product's own; name the pack's playbook differently"
                )));
            }
            if playbook.text.len() > MAX_PLAYBOOK_BYTES {
                return invalid(format!("playbook `{id}` is over {MAX_PLAYBOOK_BYTES} bytes"));
            }
            if front_matter_name(&playbook.text) != Some(id) {
                return invalid(format!(
                    "playbook `{id}` must open with front matter naming it (`---\\nname: {id}\\n\
                     description: …\\n---`)"
                ));
            }
            let Some(job) = crate::jobs::job(&playbook.job) else {
                return invalid(format!(
                    "playbook `{id}` names job `{}`, which is not a job of spec 3.5 §7.20",
                    playbook.job
                ));
            };
            if playbook.agents.is_empty() {
                return invalid(format!("playbook `{id}` names no agent"));
            }
            if let Some(agent) = playbook.agents.iter().find(|agent| !job.agents.contains(agent)) {
                return invalid(format!(
                    "playbook `{id}` is for {agent}, who does not run the `{}` job",
                    job.id
                ));
            }
        }
        Ok(())
    }

    fn check_drivers(&self) -> Result<(), PackError> {
        let invalid = |reason: String| Err(PackError::Invalid(reason));
        let mut ids = BTreeSet::new();
        for driver in &self.drivers {
            let id = driver.id.as_str();
            if !is_valid_pack_name(id) {
                return invalid(format!(
                    "driver `{id}` is not named with 1–64 lowercase letters, digits and `-`"
                ));
            }
            if !ids.insert(id) {
                return invalid(format!("driver `{id}` appears twice"));
            }
            let manifest = DriverManifest::from_toml(&driver.manifest)
                .map_err(|error| PackError::Invalid(format!("driver `{id}`: {error}")))?;
            if manifest.id != id {
                return invalid(format!("driver `{id}`'s manifest is for `{}`", manifest.id));
            }
            if manifest.sandbox == DriverSandbox::Unsandboxed {
                return invalid(format!(
                    "driver `{id}` runs unsandboxed; a pack's drivers run in the OS sandbox or a \
                     container"
                ));
            }
            let partitions = [adk_drivers::PUBLIC_PARTITION, adk_drivers::BACK_OFFICE_PARTITION];
            if manifest.partitions.is_empty()
                || manifest.partitions.iter().any(|p| !partitions.contains(&p.as_str()))
            {
                return invalid(format!(
                    "driver `{id}` must serve the `{}` or `{}` partition",
                    partitions[0], partitions[1]
                ));
            }
            if manifest.tools.is_empty() {
                return invalid(format!("driver `{id}` lists no tools with their effect classes"));
            }
            if let Some((tool, _)) =
                manifest.tools.iter().find(|(_, class)| **class == EffectClass::Other)
            {
                return invalid(format!(
                    "driver `{id}`'s tool `{tool}` is classed `other`; declare its effect class"
                ));
            }
        }
        Ok(())
    }
}

/// The SHA-256 of the JCS form of `value` without `digest` and `proof`.
fn content_digest(value: &Value) -> Result<Digest, PackError> {
    let mut content = value.clone();
    if let Some(object) = content.as_object_mut() {
        object.remove("digest");
        object.remove("proof");
    }
    document_digest(&content).map_err(|error| PackError::Malformed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_awp::signing::jws::Ed25519DocumentSigner;
    use awp_types::{ConsentRequirement, ConsentScope};
    use serde_json::json;

    use crate::packs::trust::TrustedKey;

    fn key() -> Ed25519DocumentSigner {
        Ed25519DocumentSigner::from_seed("tables-2026", [4; 32]).unwrap()
    }

    fn trust() -> TrustList {
        TrustList::new().with(TrustedKey::new("tables.example", "tables-2026", key().public_key()))
    }

    fn waitlist() -> PackBundle {
        let join = CapabilityV2 {
            consent: Some(ConsentRequirement {
                scope: ConsentScope::Custom("contact".into()),
                policy_version: "1".into(),
            }),
            input_schema: Some(json!({
                "type": "object",
                "properties": { "email": { "type": "string", "maxLength": 254 }, "party": { "type": "integer" } },
                "required": ["email"],
                "additionalProperties": false
            })),
            ..CapabilityV2::new(
                "join_waitlist",
                "Join tonight's waiting list",
                CapabilityEffect::NonIdempotent,
            )
        };
        PackBundle::new(
            "waitlist",
            "1.0.0",
            PackPublisher::new("tables.example").named("Tables"),
            "A waiting list for fully booked evenings",
        )
        .capability(join, HandlerConfig::Form)
        .playbook(PackPlaybook {
            id: "waitlist-capture".into(),
            job: "capture".into(),
            agents: vec![AgentRole::Concierge],
            text: "---\nname: waitlist-capture\ndescription: Offer the waiting list.\n---\n\nOffer it.\n".into(),
        })
        .driver(PackDriver {
            id: "tables-pos".into(),
            manifest: "id = \"tables-pos\"\ncommand = \"mcp-tables\"\npartitions = [\"back-office\"]\n\n[tools]\ntables_list = \"read\"\n".into(),
        })
    }

    async fn sealed(bundle: PackBundle) -> Vec<u8> {
        bundle.seal(&key()).await.unwrap().to_json().unwrap()
    }

    fn refused(bytes: &[u8]) -> String {
        PackBundle::verify(bytes, &trust()).unwrap_err().code().to_string()
    }

    #[tokio::test]
    async fn a_sealed_bundle_verifies_and_reports_its_publisher_key() {
        let verified = PackBundle::verify(&sealed(waitlist()).await, &trust()).unwrap();
        assert_eq!(verified.bundle.name, "waitlist");
        assert_eq!(verified.kid, "tables-2026");
        assert_eq!(verified.thumbprint, jwk_thumbprint(&key().public_key()));
        assert_eq!(Some(verified.digest), verified.bundle.digest);
    }

    #[tokio::test]
    async fn unsigned_unknown_and_tampered_bundles_are_refused() {
        assert_eq!(refused(&waitlist().to_json().unwrap()), "unsigned");
        let bytes = sealed(waitlist()).await;
        assert_eq!(
            PackBundle::verify(&bytes, &TrustList::new()).unwrap_err().code(),
            "unknown_publisher"
        );
        let other = TrustList::new().with(TrustedKey::new(
            "tables.example",
            "tables-2027",
            key().public_key(),
        ));
        assert_eq!(PackBundle::verify(&bytes, &other).unwrap_err().code(), "unknown_publisher");

        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["capabilities"][0]["effect"] = json!("read_only");
        assert_eq!(refused(&serde_json::to_vec(&value).unwrap()), "tampered");

        // The digest made to match the change, the signature still covers the old one.
        let digest = content_digest(&value).unwrap();
        value["digest"] = json!(digest);
        assert_eq!(refused(&serde_json::to_vec(&value).unwrap()), "tampered");

        // Another key under the trusted publisher's id.
        let thief = Ed25519DocumentSigner::from_seed("tables-2026", [5; 32]).unwrap();
        let forged = waitlist().seal(&thief).await.unwrap().to_json().unwrap();
        assert_eq!(refused(&forged), "tampered");
    }

    #[tokio::test]
    async fn verification_reads_the_signed_json_not_a_reformatting() {
        let bytes = sealed(waitlist()).await;
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(PackBundle::verify(&serde_json::to_vec(&value).unwrap(), &trust()).is_ok());
        let mut extra = value.clone();
        extra["capabilities"][0]["x-note"] = json!("added");
        assert_eq!(refused(&serde_json::to_vec(&extra).unwrap()), "tampered");
        let mut top = value;
        top["note"] = json!("added");
        assert_eq!(refused(&serde_json::to_vec(&top).unwrap()), "malformed");
        assert_eq!(refused(b"[1, 2"), "malformed");
        let mut later = serde_json::from_slice::<Value>(&bytes).unwrap();
        later["format"] = json!("adk-website.pack/2");
        assert_eq!(refused(&serde_json::to_vec(&later).unwrap()), "format");
    }

    fn invalid(bundle: PackBundle) -> String {
        bundle.check().unwrap_err().to_string()
    }

    #[test]
    fn handlers_must_fit_their_capabilities() {
        let mut weak = waitlist();
        weak.capabilities[0].effect = CapabilityEffect::ReadOnly;
        assert!(invalid(weak).contains("at least `idempotent`"));

        let mut unhandled = waitlist();
        unhandled.handlers.clear();
        assert!(invalid(unhandled).contains("names no handler"));

        let mut stray = waitlist();
        stray.handlers.insert("other_thing".into(), HandlerConfig::Search);
        assert!(invalid(stray).contains("does not declare"));

        let booking =
            CapabilityV2::new("reserve_table", "Reserve", CapabilityEffect::NonIdempotent);
        let unread = waitlist()
            .capability(booking, HandlerConfig::Booking { operation: BookingOperation::Book });
        assert!(invalid(unread).contains("declares no `slot`"));

        let mut no_consent = waitlist();
        no_consent.capabilities[0].consent = None;
        assert!(invalid(no_consent).contains("A-I8"));

        let mut policy = waitlist();
        policy.capabilities[0].consent.as_mut().unwrap().policy_version = "7".into();
        assert!(invalid(policy).contains("policy version"));

        let mut twice = waitlist();
        twice.capabilities.push(twice.capabilities[0].clone());
        assert!(invalid(twice).contains("declared twice"));

        let mut bad_name = waitlist();
        bad_name.capabilities[0].name = "Join".into();
        bad_name.handlers = [("Join".to_string(), HandlerConfig::Form)].into();
        assert!(invalid(bad_name).contains("capability `Join`"));
    }

    #[test]
    fn names_versions_and_publishers_are_checked() {
        let mut named = waitlist();
        named.name = "Wait List".into();
        assert!(invalid(named).contains("lowercase"));
        for version in ["1.0", "v1.0.0", "1.0.0+build.7"] {
            let mut versioned = waitlist();
            versioned.version = version.into();
            assert_eq!(versioned.check().unwrap_err().code(), "invalid", "{version}");
        }
        let mut publisher = waitlist();
        publisher.publisher.id = "Tables Ltd".into();
        assert!(invalid(publisher).contains("publisher"));
        let empty =
            PackBundle::new("empty", "1.0.0", PackPublisher::new("tables.example"), "Nothing");
        assert!(invalid(empty).contains("adds no"));
        let mut quiet = waitlist();
        quiet.description = " ".into();
        assert!(invalid(quiet).contains("description"));
    }

    #[test]
    fn playbooks_are_skill_files_for_a_job_their_agents_run() {
        let with = |change: &dyn Fn(&mut PackPlaybook)| {
            let mut bundle = waitlist();
            change(&mut bundle.playbooks[0]);
            bundle.check().unwrap_err()
        };
        assert!(with(&|p| p.text = "Offer it.".into()).to_string().contains("front matter"));
        assert!(
            with(&|p| p.text = "---\nname: other\ndescription: x\n---\n".into())
                .to_string()
                .contains("front matter")
        );
        assert!(with(&|p| p.job = "world-domination".into()).to_string().contains("not a job"));
        assert!(
            with(&|p| p.agents = vec![AgentRole::Finance]).to_string().contains("does not run")
        );
        assert!(with(&|p| p.agents.clear()).to_string().contains("names no agent"));
        let product = with(&|p| {
            p.id = "sell".into();
            p.text = "---\nname: sell\ndescription: x\n---\n".into();
        });
        assert_eq!(product.code(), "conflict");
    }

    #[test]
    fn drivers_are_sandboxed_and_declare_every_effect() {
        let with = |manifest: &str| {
            let mut bundle = waitlist();
            bundle.drivers[0].manifest = manifest.into();
            bundle.check().unwrap_err().to_string()
        };
        let base =
            "id = \"tables-pos\"\ncommand = \"mcp-tables\"\npartitions = [\"back-office\"]\n";
        assert!(
            with(&format!("{base}sandbox = \"unsandboxed\"\n[tools]\ntables_list = \"read\"\n"))
                .contains("unsandboxed")
        );
        assert!(
            with(&format!("{base}[tools]\ntables_list = \"other\"\n")).contains("classed `other`")
        );
        assert!(with(base).contains("lists no tools"));
        assert!(
            with("id = \"tables-pos\"\ncommand = \"mcp-tables\"\n[tools]\nx = \"read\"\n")
                .contains("partition")
        );
        assert!(with("id = \"till\"\ncommand = \"mcp-tables\"\npartitions = [\"public\"]\n[tools]\nx = \"read\"\n").contains("is for `till`"));
        assert!(with("not toml at all = = =").contains("driver `tables-pos`"));
    }

    #[test]
    fn handler_configuration_reads_as_json() {
        for (text, config) in [
            (r#"{"handler":"form"}"#, HandlerConfig::Form),
            (
                r#"{"handler":"order","operation":"return"}"#,
                HandlerConfig::Order { operation: OrderOperation::Return },
            ),
            (
                r#"{"handler":"listing","content":"notice"}"#,
                HandlerConfig::Listing { content: ContentKind::Notice },
            ),
        ] {
            assert_eq!(serde_json::from_str::<HandlerConfig>(text).unwrap(), config);
            assert_eq!(serde_json::to_string(&config).unwrap(), text);
        }
        assert_eq!(HandlerConfig::Listing { content: ContentKind::Event }.label(), "listing/event");
        assert!(serde_json::from_str::<HandlerConfig>(r#"{"handler":"shell"}"#).is_err());
    }

    #[test]
    fn refusals_map_to_adk_errors() {
        let error: AdkError = PackError::Unsigned.into();
        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert_eq!(error.code, "website.pack.unsigned");
    }
}
