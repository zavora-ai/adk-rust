//! The site's signed AWP documents (spec 3.5 §7.2, §7.3, §7.7, D-11, A-I5): the contract at
//! `/.well-known/awp.json`, the key set with the root's delegations, and the public charter
//! terms, built with `adk-awp`'s `StaticBundle` and released with every release so `awp
//! verify` can walk the chain from the DNS-anchored root to every served page.
//!
//! | Key | Scope | Signs |
//! |---|---|---|
//! | Root | — | The contract and the delegations; outside every agent (A-I5) |
//! | Release | `release` | The site log's checkpoints (`adk-site`'s release signer, same seed) |
//! | Edge | `card`, `receipt` | The A2A Agent Card, the status, consent receipts and stream entries |
//!
//! Signing is the owner's step at a terminal, as `adk-rust awp keys rotate` and `delegate`
//! do it: a served site never holds the root and never signs its own contract or
//! delegations; it publishes the `well-known/` files the owner wrote (D-11, A-I5).

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_awp::signing::anchor::{root_record, root_record_name};
use adk_awp::signing::bundle::StaticBundle;
use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use adk_awp::signing::keys::delegate;
use adk_core::{AdkError, Result};
use adk_site::content::SiteProject;
use awp_types::v2::contract::{CHARTER_PATH, CHECKPOINT_PATH, release_manifest_path};
use awp_types::{
    ContactLinks, ContractRelease, ContractStatus, IdentityMethod, IdentityPolicy, KeyScope,
    KeySet, LanguageTag, Profile, PublicTerms, RootKey, SiteContract, SiteState, SurfaceLinks,
};
use chrono::{DateTime, Duration, Utc};

fn signing(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("signing the site's documents failed: {error}"))
}

/// The site's keys (spec 3.5 D-11): the root that signs the contract and delegations, the
/// release key the site log's checkpoints are signed with, and the edge's key.
#[derive(Clone)]
pub struct SiteKeys {
    /// The root key; it never enters an agent's context (A-I5).
    pub root: Arc<Ed25519DocumentSigner>,
    /// The release key, as `adk-awp` sees it; `adk-site`'s release signer holds the same seed.
    pub release: Arc<Ed25519DocumentSigner>,
    /// The edge's key, delegated `card` and `receipt`.
    pub edge: Arc<Ed25519DocumentSigner>,
}

impl std::fmt::Debug for SiteKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteKeys")
            .field("root", &self.root.kid())
            .field("release", &self.release.kid())
            .field("edge", &self.edge.kid())
            .finish()
    }
}

impl SiteKeys {
    /// Keys from seeds; the release seed is the one `adk-site`'s `Ed25519ReleaseSigner`
    /// signs the site log with.
    ///
    /// # Errors
    ///
    /// A seed that does not make a key.
    pub fn from_seeds(
        domain: &str,
        root: [u8; 32],
        release: [u8; 32],
        edge: [u8; 32],
    ) -> Result<Self> {
        Ok(Self {
            root: Arc::new(
                Ed25519DocumentSigner::from_seed(format!("{domain}-root"), root)
                    .map_err(signing)?,
            ),
            release: Arc::new(
                Ed25519DocumentSigner::from_seed(format!("{domain}-release"), release)
                    .map_err(signing)?,
            ),
            edge: Arc::new(
                Ed25519DocumentSigner::from_seed(format!("{domain}-edge"), edge)
                    .map_err(signing)?,
            ),
        })
    }

    /// The `_awp-root.{domain}` TXT record that anchors the root (spec 3.5 §7.7): its name
    /// and value.
    pub fn root_record(&self, domain: &str) -> (String, String) {
        (root_record_name(domain), root_record(self.root.kid(), &self.root.public_key()))
    }

    /// The key set: the root and its delegations, valid from `from` for `days` (spec 3.5
    /// §7.3).
    ///
    /// # Errors
    ///
    /// The root's signing error.
    pub async fn key_set(&self, from: DateTime<Utc>, days: i64) -> Result<KeySet> {
        let until = from + Duration::days(days);
        let release = delegate(
            self.root.as_ref(),
            self.release.kid(),
            self.release.public_key(),
            vec![KeyScope::Release],
            from,
            until,
        )
        .await
        .map_err(signing)?;
        let edge = delegate(
            self.root.as_ref(),
            self.edge.kid(),
            self.edge.public_key(),
            vec![KeyScope::Card, KeyScope::Receipt],
            from,
            until,
        )
        .await
        .map_err(signing)?;
        Ok(KeySet {
            root: RootKey {
                kid: self.root.kid().to_string(),
                public_key: self.root.public_key(),
                endorsement: None,
            },
            delegations: vec![release, edge],
            retired_roots: vec![],
            revocations: vec![],
        })
    }
}

/// What the contract says about the site.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractInputs {
    /// The site's id.
    pub id: uuid::Uuid,
    /// Its name.
    pub name: String,
    /// Its domain.
    pub domain: String,
    /// Its languages.
    pub languages: Vec<LanguageTag>,
    /// The edge's origin, such as `https://agent.example.com`; `None` for a static site.
    pub edge: Option<String>,
    /// The release the contract ships in.
    pub release_seq: u64,
    /// The charter's public terms.
    pub terms: PublicTerms,
}

/// The signed documents of a release, by path (spec 3.5 §7.2): the contract, the key set and
/// the public charter terms. [`add_to_project`] puts them in the project every release is
/// built from.
///
/// # Errors
///
/// The root's signing error.
pub async fn site_documents(
    keys: &SiteKeys,
    inputs: &ContractInputs,
    valid_from: DateTime<Utc>,
) -> Result<(BTreeMap<String, Vec<u8>>, SiteContract)> {
    let origin = format!("https://{}", inputs.domain);
    let key_set = keys.key_set(valid_from, 90).await?;
    let mut contract = SiteContract::new(awp_types::SiteRef {
        id: inputs.id,
        name: inputs.name.clone(),
        domain: inputs.domain.clone(),
        languages: inputs.languages.clone(),
    });
    contract.profile =
        Some(if inputs.edge.is_some() { Profile::StaticEdge } else { Profile::Static });
    contract.surfaces = Some(SurfaceLinks {
        a2a_card: inputs.edge.as_ref().map(|edge| format!("{edge}/.well-known/agent-card.json")),
        mcp_server_card: inputs
            .edge
            .as_ref()
            .map(|edge| format!("{edge}/.well-known/mcp/server-card.json")),
        manifest: inputs.edge.as_ref().map(|edge| format!("{edge}/awp/v2/manifest")),
        llms_txt: Some(format!("{origin}/llms.txt")),
        machine_views: true,
    });
    if let Some(edge) = &inputs.edge {
        contract.identity = Some(IdentityPolicy {
            accepts: vec![IdentityMethod::WebBotAuth],
            trust_rules: Some(format!("{edge}/awp/v2/trust")),
        });
        contract.status = Some(ContractStatus {
            url: format!("{edge}/awp/v2/status"),
            last_known: Some(SiteState::Operational),
        });
    }
    contract.release = Some(ContractRelease {
        seq: inputs.release_seq,
        manifest: format!("{origin}{}", release_manifest_path(inputs.release_seq)),
        checkpoint: format!("{origin}{CHECKPOINT_PATH}"),
    });
    contract.contact = Some(ContactLinks {
        security: Some(format!("{origin}/.well-known/security.txt")),
        ..inputs.terms.contact.clone()
    });
    let bundle = StaticBundle::build(contract, &key_set, &inputs.terms, keys.root.as_ref())
        .await
        .map_err(signing)?;
    Ok((bundle.files().clone(), bundle.contract().clone()))
}

/// Puts the signed `documents` into `project` (spec 3.5 §7.2, AWP 2.0 §14.2): the contract and
/// the key set as release documents, and `terms` as the project's public terms, which the
/// generator renders at the charter path whose digest the contract carries.
///
/// # Example
///
/// ```rust
/// use adk_site::content::{SiteConfig, SiteProject};
/// use adk_website::contract::{ContractInputs, SiteKeys, add_to_project, site_documents};
/// use awp_types::PublicTerms;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let keys = SiteKeys::from_seeds("notes.example", [1; 32], [2; 32], [3; 32]).unwrap();
/// let inputs = ContractInputs {
///     id: uuid::Uuid::nil(),
///     name: "Notes".into(),
///     domain: "notes.example".into(),
///     languages: vec!["en".parse().unwrap()],
///     edge: None,
///     release_seq: 1,
///     terms: PublicTerms::default(),
/// };
/// let (documents, contract) = site_documents(&keys, &inputs, chrono::Utc::now()).await.unwrap();
/// assert!(contract.proof.is_some(), "the root signs the contract");
/// let config = SiteConfig::new("Notes", "notes.example", vec!["en".parse().unwrap()]);
/// let mut project = SiteProject::new(config, Vec::new(), chrono::Utc::now());
/// add_to_project(&mut project, documents, inputs.terms);
/// assert!(project.documents.contains_key(".well-known/awp.json"));
/// assert!(project.public_terms.is_some());
/// # }
/// ```
pub fn add_to_project(
    project: &mut SiteProject,
    documents: BTreeMap<String, Vec<u8>>,
    terms: PublicTerms,
) {
    for (path, bytes) in documents {
        if path != CHARTER_PATH {
            project.documents.insert(path.trim_start_matches('/').to_string(), bytes.into());
        }
    }
    project.public_terms = Some(terms);
}
