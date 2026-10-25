//! Document rules the types deliberately accept and conformance enforces (spec 3.5 §7.2–§7.7;
//! AWP 2.0 §16).
//!
//! | Rule | Document | AWP 2.0 |
//! |---|---|---|
//! | `document.schema` | any: it must parse as its type | §1.2 |
//! | `contract.proof`, `contract.profile`, `contract.keys` | site contract: required by every profile | §4.2, §16.1 S1 |
//! | `contract.release` | site contract, `static` and `static+edge` | §4.2 |
//! | `contract.identity`, `contract.status`, `contract.manifest`, `contract.surfaces` | site contract, `static+edge` | §16.1 E1 |
//! | `capability.name` | capability, manifest | §7.1 |
//! | `held_call.outcome` | held call: a completed call has a result or an error | §7.6 |
//! | `consent.explicit` | consent receipt: `proactive_outreach` needs `explicit_consent` | §11.2 |
//! | `release.previous`, `release.removed`, `release.base`, `release.redirect_status`, `release.path`, `release.executable` | release manifest | §14.1, §14.3, §14.4 |
//! | `charter.*` | charter: `Charter::validate` | §13 |
//! | `event.data` | event envelope: known types carry their data | §9.6 |

use awp_types::{
    CapabilityManifestV2, CapabilityV2, CatalogChanged, Charter, ConsentReceipt, ConsentRevoked,
    ConsentScope, EventEnvelope, EventSubscription, EventType, HeldCall, HeldCallState, KeySet,
    LegalBasis, LogEntry, MachineView, OrderUpdated, Profile, PublicTerms, QuoteDocument, Receipt,
    ReleaseManifest, ReleasePublished, SiteContract, SiteStatus, StatusChanged, StreamEntry,
    SubscriptionSuspended, TrustRules,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::v2::capability::is_valid_capability_name;
use crate::verify::is_executable_path;

/// The kinds of AWP 2.0 document the rules know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DocumentKind {
    /// `/.well-known/awp.json`.
    SiteContract,
    /// `keys.json`.
    KeySet,
    /// The trust rules.
    TrustRules,
    /// One capability.
    Capability,
    /// The capability manifest.
    CapabilityManifest,
    /// A machine view.
    MachineView,
    /// A held call.
    HeldCall,
    /// An event envelope.
    EventEnvelope,
    /// An event subscription.
    Subscription,
    /// The site status.
    SiteStatus,
    /// A consent receipt.
    ConsentReceipt,
    /// A quote.
    Quote,
    /// A commerce receipt.
    Receipt,
    /// A release manifest.
    ReleaseManifest,
    /// A site log entry.
    LogEntry,
    /// A stream entry.
    StreamEntry,
    /// The full charter.
    Charter,
    /// The charter's public terms.
    PublicTerms,
}

/// A rule a document breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleViolation {
    /// The rule's name, such as `release.previous`.
    pub rule: &'static str,
    /// What is wrong.
    pub detail: String,
}

fn violation(rule: &'static str, detail: impl Into<String>) -> RuleViolation {
    RuleViolation { rule, detail: detail.into() }
}

fn parse<T: DeserializeOwned>(value: &Value) -> Result<T, Vec<RuleViolation>> {
    serde_json::from_value(value.clone())
        .map_err(|error| vec![violation("document.schema", error.to_string())])
}

/// Returns every rule `value` breaks as a `kind` document; an empty list conforms.
///
/// # Example
///
/// ```
/// use adk_awp::conformance::rules::{DocumentKind, check_document};
/// use serde_json::json;
///
/// let entry = json!({ "seq": 1, "manifest": format!("sha256:{}", "0".repeat(64)), "note": "x" });
/// assert_eq!(check_document(DocumentKind::LogEntry, &entry)[0].rule, "document.schema");
/// ```
pub fn check_document(kind: DocumentKind, value: &Value) -> Vec<RuleViolation> {
    let result = match kind {
        DocumentKind::SiteContract => parse::<SiteContract>(value).map(|c| contract_rules(&c)),
        DocumentKind::KeySet => parse::<KeySet>(value).map(|_| Vec::new()),
        DocumentKind::TrustRules => parse::<TrustRules>(value).map(|_| Vec::new()),
        DocumentKind::Capability => parse::<CapabilityV2>(value).map(|c| capability_rules(&[c])),
        DocumentKind::CapabilityManifest => {
            parse::<CapabilityManifestV2>(value).map(|m| capability_rules(&m.capabilities))
        }
        DocumentKind::MachineView => parse::<MachineView>(value).map(|_| Vec::new()),
        DocumentKind::HeldCall => parse::<HeldCall>(value).map(|call| held_call_rules(&call)),
        DocumentKind::EventEnvelope => {
            parse::<EventEnvelope>(value).map(|event| event_rules(&event))
        }
        DocumentKind::Subscription => parse::<EventSubscription>(value).map(|_| Vec::new()),
        DocumentKind::SiteStatus => parse::<SiteStatus>(value).map(|_| Vec::new()),
        DocumentKind::ConsentReceipt => parse::<ConsentReceipt>(value).map(|r| consent_rules(&r)),
        DocumentKind::Quote => parse::<QuoteDocument>(value).map(|_| Vec::new()),
        DocumentKind::Receipt => parse::<Receipt>(value).map(|_| Vec::new()),
        DocumentKind::ReleaseManifest => parse::<ReleaseManifest>(value).map(|m| release_rules(&m)),
        DocumentKind::LogEntry => parse::<LogEntry>(value).map(|_| Vec::new()),
        DocumentKind::StreamEntry => parse::<StreamEntry>(value).map(|_| Vec::new()),
        DocumentKind::Charter => parse::<Charter>(value).map(|charter| match charter.validate() {
            Ok(()) => Vec::new(),
            Err(error) => error
                .violations
                .iter()
                .map(|v| violation("charter.validate", v.to_string()))
                .collect(),
        }),
        DocumentKind::PublicTerms => parse::<PublicTerms>(value).map(|_| Vec::new()),
    };
    result.unwrap_or_else(|violations| violations)
}

/// The contract rules every profile's conformance list adds to the document format (AWP 2.0
/// §4.2, §16.1).
pub fn contract_rules(contract: &SiteContract) -> Vec<RuleViolation> {
    let mut found = Vec::new();
    if contract.proof.is_none() {
        found.push(violation("contract.proof", "every profile requires the root's proof"));
    }
    if contract.keys.is_none() {
        found.push(violation("contract.keys", "every profile requires `keys`"));
    }
    let Some(profile) = contract.profile else {
        found.push(violation("contract.profile", "every profile requires `profile`"));
        return found;
    };
    if matches!(profile, Profile::Static | Profile::StaticEdge) && contract.release.is_none() {
        found.push(violation("contract.release", "a static profile names its release"));
    }
    if matches!(profile, Profile::StaticEdge | Profile::Dynamic) {
        if contract.identity.is_none() {
            found.push(violation("contract.identity", "an edge profile names its identity policy"));
        }
        if contract.status.is_none() {
            found.push(violation("contract.status", "an edge profile names its status"));
        }
        let surfaces = contract.surfaces.as_ref();
        if surfaces.and_then(|s| s.manifest.as_ref()).is_none() {
            found.push(violation(
                "contract.manifest",
                "an edge profile names its capability manifest",
            ));
        }
        if surfaces.is_none_or(|s| s.a2a_card.is_none() && s.mcp_server_card.is_none()) {
            found.push(violation(
                "contract.surfaces",
                "an edge profile names an A2A or MCP surface",
            ));
        }
    }
    found
}

fn capability_rules(capabilities: &[CapabilityV2]) -> Vec<RuleViolation> {
    capabilities
        .iter()
        .filter(|capability| !is_valid_capability_name(&capability.name))
        .map(|capability| {
            violation(
                "capability.name",
                format!("`{}` is not ^[a-z][a-z0-9_]{{0,63}}$", capability.name),
            )
        })
        .collect()
}

fn held_call_rules(call: &HeldCall) -> Vec<RuleViolation> {
    if call.state == HeldCallState::Completed && call.result.is_none() && call.error.is_none() {
        vec![violation("held_call.outcome", "a completed call carries a result or an error")]
    } else {
        Vec::new()
    }
}

fn consent_rules(receipt: &ConsentReceipt) -> Vec<RuleViolation> {
    if receipt.scope == ConsentScope::ProactiveOutreach
        && receipt.legal_basis != LegalBasis::ExplicitConsent
    {
        vec![violation("consent.explicit", "proactive_outreach requires explicit_consent")]
    } else {
        Vec::new()
    }
}

/// The release manifest rules (AWP 2.0 §14.1, §14.3, §14.4).
pub fn release_rules(manifest: &ReleaseManifest) -> Vec<RuleViolation> {
    let mut found = Vec::new();
    if manifest.seq == 0 {
        found.push(violation("release.previous", "releases are numbered from 1"));
    }
    if manifest.seq > 1 && manifest.previous.is_none() {
        found.push(violation(
            "release.previous",
            format!("release {} names no previous manifest", manifest.seq),
        ));
    }
    if manifest.seq == 1 && manifest.previous.is_some() {
        found.push(violation("release.previous", "release 1 has no previous manifest"));
    }
    if manifest.is_snapshot() && !manifest.removed.is_empty() {
        found.push(violation("release.removed", "a snapshot removes nothing"));
    }
    if manifest.base.is_some_and(|base| base == 0 || base >= manifest.seq) {
        found.push(violation("release.base", "a delta's base is an earlier release"));
    }
    for redirect in &manifest.redirects {
        if !matches!(redirect.status, 301 | 302 | 307 | 308 | 410) {
            found.push(violation(
                "release.redirect_status",
                format!("redirect {} has status {}", redirect.from, redirect.status),
            ));
        }
    }
    for path in manifest.files.keys().chain(manifest.removed.iter()) {
        if !path.starts_with('/') || path.contains('?') || path.contains('#') {
            found.push(violation("release.path", format!("`{path}` is not an absolute URL path")));
        }
        if is_executable_path(path) {
            found.push(violation("release.executable", format!("`{path}` is server-executable")));
        }
    }
    found
}

fn event_rules(event: &EventEnvelope) -> Vec<RuleViolation> {
    fn typed<T: DeserializeOwned>(data: &Value) -> Option<String> {
        serde_json::from_value::<T>(data.clone()).err().map(|error| error.to_string())
    }
    let error = match &event.event_type {
        EventType::SiteReleasePublished => typed::<ReleasePublished>(&event.data),
        EventType::SiteStatusChanged => typed::<StatusChanged>(&event.data),
        EventType::CatalogChanged => typed::<CatalogChanged>(&event.data),
        EventType::OrderUpdated => typed::<OrderUpdated>(&event.data),
        EventType::ConsentRevoked => typed::<ConsentRevoked>(&event.data),
        EventType::SubscriptionSuspended => typed::<SubscriptionSuspended>(&event.data),
        _ => None,
    };
    error.map(|error| vec![violation("event.data", error)]).unwrap_or_default()
}
