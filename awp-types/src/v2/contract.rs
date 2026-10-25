//! The site contract, `/.well-known/awp.json` (spec 3.5 §7.2).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::content::LanguageTag;
use super::event::EventCatalog;
use super::identity::{IdentityPolicy, Proof};
use super::release::Digest;
use super::status::SiteState;
use crate::AwpVersion;

/// The AWP 2.0 protocol version, carried as `"awp": "2.0"` (spec 3.5 §7.2).
pub const VERSION_2_0: AwpVersion = AwpVersion { major: 2, minor: 0 };

/// Media type of the site contract (spec 3.5 §7.2).
pub const MEDIA_TYPE: &str = "application/awp+json";

/// Well-known path of the site contract (spec 3.5 §7.2).
pub const CONTRACT_PATH: &str = "/.well-known/awp.json";

/// Well-known path of `keys.json` (spec 3.5 §7.2).
pub const KEYS_PATH: &str = "/.well-known/awp/keys.json";

/// Well-known path of the charter's public terms (spec 3.5 §7.6).
pub const CHARTER_PATH: &str = "/.well-known/awp/charter.json";

/// Well-known path of the site log's latest checkpoint (spec 3.5 §7.7).
pub const CHECKPOINT_PATH: &str = "/.well-known/awp/log/checkpoint";

/// Returns the well-known path of release `seq`'s manifest (spec 3.5 §7.7).
///
/// # Example
///
/// ```
/// assert_eq!(awp_types::v2::contract::release_manifest_path(42), "/.well-known/awp/releases/42.json");
/// ```
pub fn release_manifest_path(seq: u64) -> String {
    format!("/.well-known/awp/releases/{seq}.json")
}

/// The site contract served at `/.well-known/awp.json` as `application/awp+json` and signed
/// by the site root key (spec 3.5 §7.2).
///
/// Every field beyond `awp` and `site` is optional, so a read-only static site publishes a
/// minimal contract and a client that speaks only A2A or MCP follows the links and ignores
/// the rest. The `proof` is a detached JWS over the JCS form of the contract without
/// `proof`.
///
/// # Example
///
/// ```
/// use awp_types::{Profile, SiteContract, SiteRef};
/// use uuid::Uuid;
///
/// let mut contract = SiteContract::new(SiteRef {
///     id: Uuid::nil(),
///     name: "Example Solar".into(),
///     domain: "example.com".into(),
///     languages: vec![],
/// });
/// contract.profile = Some(Profile::Static);
/// let json = serde_json::to_string(&contract).unwrap();
/// assert!(json.starts_with(r#"{"awp":"2.0","site":"#));
/// assert!(json.contains(r#""profile":"static""#));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteContract {
    /// Protocol version, `"2.0"`.
    #[serde(with = "super::serde_util::version_string")]
    #[cfg_attr(feature = "json-schema", schemars(with = "String"))]
    pub awp: AwpVersion,
    /// The site this contract describes.
    pub site: SiteRef,
    /// Deployment profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    /// Links to every agent surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surfaces: Option<SurfaceLinks>,
    /// The identities the site accepts and where its trust rules are published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityPolicy>,
    /// The current release and the site log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ContractRelease>,
    /// Where the site status is served and its last known state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ContractStatus>,
    /// Event subscriptions and the event catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<EventsLink>,
    /// Accepted payment rails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payments: Option<PaymentsLink>,
    /// The charter's published public terms and their digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter: Option<CharterLink>,
    /// Contact links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<ContactLinks>,
    /// URL of `keys.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
    /// Detached JWS by the site root key over the JCS form of the contract without `proof`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
}

impl SiteContract {
    /// Creates a minimal 2.0 contract for `site` with every optional field absent.
    pub fn new(site: SiteRef) -> Self {
        Self {
            awp: VERSION_2_0,
            site,
            profile: None,
            surfaces: None,
            identity: None,
            release: None,
            status: None,
            events: None,
            payments: None,
            charter: None,
            contact: None,
            keys: None,
            proof: None,
        }
    }

    /// Returns the contract without its `proof`: the document the root key signs.
    pub fn without_proof(&self) -> Self {
        Self { proof: None, ..self.clone() }
    }
}

/// The site a contract describes (spec 3.5 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteRef {
    /// Stable site identifier; the charter's `siteId`.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// The domain the contract is served from.
    pub domain: String,
    /// Languages the site publishes, as BCP 47 tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<LanguageTag>,
}

/// Deployment profile of a site (spec 3.5 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Profile {
    /// Contract, keys, log, cards and machine views as files; no edge.
    #[serde(rename = "static")]
    Static,
    /// Static content with an agent edge; `adk-website`'s default.
    #[serde(rename = "static+edge")]
    StaticEdge,
    /// An application serving AWP itself.
    #[serde(rename = "dynamic")]
    Dynamic,
}

/// Links to a site's agent surfaces (spec 3.5 §7.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SurfaceLinks {
    /// URL of the signed A2A Agent Card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a2a_card: Option<String>,
    /// URL of the MCP Server Card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_card: Option<String>,
    /// URL of the AWP capability manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// URL of `llms.txt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms_txt: Option<String>,
    /// Whether every page has a machine view (`<path>/index.awp.json`) and a Markdown twin.
    #[serde(default)]
    pub machine_views: bool,
}

/// The contract's link to the current release and the site log (spec 3.5 §7.2, §7.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ContractRelease {
    /// Sequence number of the current release.
    pub seq: u64,
    /// URL of the current release's manifest.
    pub manifest: String,
    /// URL of the site log's latest signed-note checkpoint.
    pub checkpoint: String,
}

/// The name spec 3.5 §7.1 gives the contract's release link.
pub type ReleaseRef = ContractRelease;

/// The contract's link to the site status (spec 3.5 §7.2, §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ContractStatus {
    /// URL of the signed status document.
    pub url: String,
    /// The state when the contract was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known: Option<SiteState>,
}

/// The contract's link to event subscriptions (spec 3.5 §7.2, §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct EventsLink {
    /// URL that accepts subscriptions.
    pub subscribe: String,
    /// Event types the site emits.
    #[serde(default)]
    pub catalog: EventCatalog,
}

/// The contract's payment section (spec 3.5 §7.2, §7.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PaymentsLink {
    /// Accepted payment rails, such as `ap2`, `acp` or `ucp`.
    #[serde(default)]
    pub rails: Vec<String>,
}

/// The contract's link to the charter's public terms (spec 3.5 §7.2, §7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CharterLink {
    /// URL of the published [`PublicTerms`](super::charter::PublicTerms).
    pub public: String,
    /// Digest of the published public terms.
    pub digest: Digest,
}

/// Contact links published in the contract and the charter's public terms (spec 3.5 §7.2,
/// §7.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ContactLinks {
    /// URL of `security.txt` (RFC 9116).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<String>,
    /// Public contact email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// URL of the contact page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}
