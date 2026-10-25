//! Capabilities, the capability manifest and machine views (spec 3.5 §7.4).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::consent::ConsentScope;
use super::content::{ContentKind, LanguageTag};
use super::identity::TrustTier;

/// A typed capability: one declaration exposed as an A2A skill, an MCP tool and
/// `POST /awp/v2/capabilities/{name}`, under one policy (spec 3.5 §7.4).
///
/// # Example
///
/// ```
/// use awp_types::{ApprovalHint, CapabilityEffect, CapabilityV2, IdempotencySource, TrustTier};
///
/// let book = CapabilityV2::new("book", "Book an installation visit", CapabilityEffect::NonIdempotent)
///     .with_min_trust(TrustTier::Known)
///     .with_approval(ApprovalHint::AboveAmount(50_000));
/// assert!(book.requires_idempotency());
/// assert!(!book.visible_to(TrustTier::Public));
/// assert_eq!(book.idempotency, IdempotencySource::Binding);
/// assert_eq!(CapabilityV2::invocation_path("book"), "/awp/v2/capabilities/book");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CapabilityV2 {
    /// Stable id: the A2A skill id and the MCP tool name.
    pub name: String,
    /// What the capability does.
    pub description: String,
    /// Side-effect class.
    pub effect: CapabilityEffect,
    /// Lowest trust tier that may see and call the capability.
    pub min_trust: TrustTier,
    /// JSON Schema of the input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// JSON Schema of the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Scope and policy version a live consent receipt must cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<ConsentRequirement>,
    /// Price in minor units, currency and accepted rails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    /// When a call is held for the owner; a held call answers 202.
    pub approval: ApprovalHint,
    /// Where the idempotency key of a call comes from.
    pub idempotency: IdempotencySource,
    /// Rate-limit class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_class: Option<String>,
    /// Successor and sunset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<Deprecation>,
}

impl CapabilityV2 {
    /// Creates a capability visible to every tier, never held, keyed by its binding.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        effect: CapabilityEffect,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            effect,
            min_trust: TrustTier::Public,
            input_schema: None,
            output_schema: None,
            consent: None,
            price: None,
            approval: ApprovalHint::Never,
            idempotency: IdempotencySource::Binding,
            rate_class: None,
            deprecated: None,
        }
    }

    /// Sets the lowest tier that may see and call the capability.
    pub fn with_min_trust(mut self, tier: TrustTier) -> Self {
        self.min_trust = tier;
        self
    }

    /// Sets when calls are held for the owner.
    pub fn with_approval(mut self, approval: ApprovalHint) -> Self {
        self.approval = approval;
        self
    }

    /// Returns `true` when every call must carry an idempotency key: a `NonIdempotent`
    /// capability executes at most once per key (A-I7).
    pub fn requires_idempotency(&self) -> bool {
        self.effect == CapabilityEffect::NonIdempotent
    }

    /// Returns `true` when a caller of `tier` may see and call the capability.
    pub fn visible_to(&self, tier: TrustTier) -> bool {
        tier >= self.min_trust
    }

    /// Returns the HTTP binding's path, `/awp/v2/capabilities/{name}`.
    pub fn invocation_path(name: &str) -> String {
        format!("/awp/v2/capabilities/{name}")
    }
}

/// Side-effect class of a capability: the three tool effect classes of ADK-Rust's governed
/// path (spec 3.5 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CapabilityEffect {
    /// Reads without changing anything.
    ReadOnly,
    /// Repeating a call leaves the same state as making it once.
    Idempotent,
    /// Repeating a call repeats the change; requires an idempotency key.
    NonIdempotent,
}

/// The consent a call needs: a live receipt for `scope` under `policyVersion`
/// (spec 3.5 §7.4, §7.5, A-I8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequirement {
    /// The consent scope.
    pub scope: ConsentScope,
    /// The policy version the receipt must name.
    pub policy_version: String,
}

/// A capability's price (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Price {
    /// Amount in the currency's minor units.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Accepted payment rails, such as `ap2` or `acp`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rails: Vec<String>,
}

/// When a call is held for the owner's decision (spec 3.5 §7.4).
///
/// Serialized with a `mode` tag: `{"mode": "never"}`, `{"mode": "above_amount", "amount":
/// <minor units>}` or `{"mode": "always"}`.
///
/// # Example
///
/// ```
/// use awp_types::ApprovalHint;
///
/// let hint = ApprovalHint::AboveAmount(50_000);
/// assert!(!hint.holds(Some(10_000)));
/// assert!(hint.holds(Some(60_000)));
/// assert_eq!(serde_json::to_string(&hint).unwrap(), r#"{"mode":"above_amount","amount":50000}"#);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "mode", content = "amount", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalHint {
    /// Calls are never held.
    #[default]
    Never,
    /// Calls above this amount in minor units are held.
    AboveAmount(u64),
    /// Every call is held.
    Always,
}

impl ApprovalHint {
    /// Returns `true` when a call for `amount` minor units is held. A call with no amount is
    /// held only under `Always`.
    pub fn holds(&self, amount: Option<u64>) -> bool {
        match self {
            ApprovalHint::Never => false,
            ApprovalHint::AboveAmount(limit) => amount.is_some_and(|amount| amount > *limit),
            ApprovalHint::Always => true,
        }
    }
}

/// Where a call's idempotency key comes from (spec 3.5 §7.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IdempotencySource {
    /// The binding that carried the call: the A2A message id, the MCP request id, or the
    /// `Idempotency-Key` header of `POST /awp/v2/capabilities/{name}`.
    #[default]
    Binding,
    /// The A2A message id.
    A2aMessageId,
    /// The MCP request id.
    McpRequestId,
    /// The `Idempotency-Key` HTTP header.
    IdempotencyKey,
}

/// A capability's deprecation (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Deprecation {
    /// Name of the capability that replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
    /// When the capability stops answering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sunset: Option<DateTime<Utc>>,
}

/// The capability manifest served at `GET /awp/v2/manifest` (spec 3.5 §7.4).
///
/// The manifest lists only the capabilities the caller's verified tier allows.
///
/// # Example
///
/// ```
/// use awp_types::{CapabilityEffect, CapabilityManifestV2, CapabilityV2, TrustTier};
///
/// let manifest = CapabilityManifestV2::new(vec![
///     CapabilityV2::new("search_site", "Search the site", CapabilityEffect::ReadOnly),
///     CapabilityV2::new("place_order", "Place an order", CapabilityEffect::NonIdempotent)
///         .with_min_trust(TrustTier::Known),
/// ]);
/// let public = manifest.visible_to(TrustTier::Public);
/// assert_eq!(public.capabilities.len(), 1);
/// assert_eq!(public.tier, Some(TrustTier::Public));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CapabilityManifestV2 {
    /// The tier this listing was filtered for; absent on the unfiltered declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<TrustTier>,
    /// The capabilities.
    #[serde(default)]
    pub capabilities: Vec<CapabilityV2>,
}

impl CapabilityManifestV2 {
    /// Creates an unfiltered manifest.
    pub fn new(capabilities: Vec<CapabilityV2>) -> Self {
        Self { tier: None, capabilities }
    }

    /// Returns the listing a caller of `tier` sees: only the capabilities its tier allows.
    pub fn visible_to(&self, tier: TrustTier) -> Self {
        Self {
            tier: Some(tier),
            capabilities: self
                .capabilities
                .iter()
                .filter(|capability| capability.visible_to(tier))
                .cloned()
                .collect(),
        }
    }

    /// Returns the capability named `name`.
    pub fn get(&self, name: &str) -> Option<&CapabilityV2> {
        self.capabilities.iter().find(|capability| capability.name == name)
    }
}

/// A page's structured twin, `<path>/index.awp.json` (spec 3.5 §7.4).
///
/// It carries the page's resource data, the capabilities that apply to it and its schema.org
/// JSON-LD; the HTML links it with `<link rel="alternate">`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MachineView {
    /// The page's path.
    pub path: String,
    /// The release in which this view last changed, so an unchanged view stays
    /// byte-identical across releases.
    pub release_seq: u64,
    /// The page's content kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ContentKind>,
    /// The page's language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LanguageTag>,
    /// The page's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Resource data: the released fields of the page's content item.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resource: BTreeMap<String, Value>,
    /// Names of the capabilities that apply to the page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// schema.org JSON-LD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_ld: Option<Value>,
    /// URL of the Markdown twin, `<path>/index.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

impl MachineView {
    /// Returns the structured twin's path for a page path: `<path>/index.awp.json`.
    ///
    /// # Example
    ///
    /// ```
    /// use awp_types::MachineView;
    ///
    /// assert_eq!(MachineView::twin_path("/products/panel"), "/products/panel/index.awp.json");
    /// assert_eq!(MachineView::twin_path("/"), "/index.awp.json");
    /// ```
    pub fn twin_path(page_path: &str) -> String {
        format!("{}/index.awp.json", page_path.trim_end_matches('/'))
    }

    /// Returns the Markdown twin's path for a page path: `<path>/index.md`.
    pub fn markdown_path(page_path: &str) -> String {
        format!("{}/index.md", page_path.trim_end_matches('/'))
    }
}
