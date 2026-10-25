//! `StaticBundle`: the static-profile files a release carries — the signed contract,
//! `keys.json` and the charter's public terms (spec 3.5 §7.1, §7.2, §7.6).

use std::collections::BTreeMap;
use std::path::Path;

use awp_types::v2::contract::{CHARTER_PATH, CONTRACT_PATH, KEYS_PATH};
use awp_types::{CharterLink, KeySet, PublicTerms, SiteContract};

use super::SigningError;
use super::jws::{DocumentSigner, document_digest, sign};

/// The static-profile files of a site, by path: `/.well-known/awp.json`,
/// `/.well-known/awp/keys.json` and `/.well-known/awp/charter.json` (spec 3.5 §7.2).
///
/// Output is deterministic: the same inputs and key give the same bytes, so a release that
/// changes nothing here uploads nothing.
///
/// # Example
///
/// ```
/// use adk_awp::signing::bundle::StaticBundle;
/// use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
/// use awp_types::{KeySet, Profile, PublicTerms, RootKey, SiteContract, SiteRef};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let root = Ed25519DocumentSigner::from_seed("site-root-2026", [1; 32]).unwrap();
/// let keys = KeySet {
///     root: RootKey { kid: "site-root-2026".into(), public_key: root.public_key(), endorsement: None },
///     delegations: vec![], retired_roots: vec![], revocations: vec![],
/// };
/// let mut contract = SiteContract::new(SiteRef {
///     id: uuid::Uuid::nil(), name: "Example".into(), domain: "example.com".into(), languages: vec![],
/// });
/// contract.profile = Some(Profile::Static);
/// let bundle = StaticBundle::build(contract, &keys, &PublicTerms::default(), &root).await.unwrap();
/// assert_eq!(bundle.files().len(), 3);
/// assert!(bundle.contract().proof.is_some());
/// # }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct StaticBundle {
    contract: SiteContract,
    files: BTreeMap<String, Vec<u8>>,
}

fn pretty<T: serde::Serialize>(document: &T) -> Result<Vec<u8>, SigningError> {
    let mut bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| SigningError::Canonicalize(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

impl StaticBundle {
    /// Builds the bundle: sets the contract's `keys` and `charter` links on the site's domain
    /// with the public terms' digest, signs the contract with `root`, and serializes the three
    /// documents.
    ///
    /// # Errors
    ///
    /// [`SigningError::Key`] when `root` is not the key set's root; other [`SigningError`]s
    /// when a document is not I-JSON or the key cannot sign.
    pub async fn build(
        mut contract: SiteContract,
        keys: &KeySet,
        terms: &PublicTerms,
        root: &dyn DocumentSigner,
    ) -> Result<Self, SigningError> {
        if root.kid() != keys.root.kid || root.public_key() != keys.root.public_key {
            return Err(SigningError::Key(
                "the contract is signed by the key set's root key only (spec 3.5 A-I5)".to_owned(),
            ));
        }
        let origin = format!("https://{}", contract.site.domain);
        contract.keys = Some(format!("{origin}{KEYS_PATH}"));
        contract.charter = Some(CharterLink {
            public: format!("{origin}{CHARTER_PATH}"),
            digest: document_digest(terms)?,
        });
        contract.proof = None;
        contract.proof = Some(sign(&contract, root).await?);
        let files = BTreeMap::from([
            (CONTRACT_PATH.to_owned(), pretty(&contract)?),
            (KEYS_PATH.to_owned(), pretty(keys)?),
            (CHARTER_PATH.to_owned(), pretty(terms)?),
        ]);
        Ok(Self { contract, files })
    }

    /// The signed contract.
    pub fn contract(&self) -> &SiteContract {
        &self.contract
    }

    /// The files, by absolute URL path.
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    /// Writes the files under `dir`, creating directories as needed.
    ///
    /// # Errors
    ///
    /// The I/O error of the first file that cannot be written.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        for (path, bytes) in &self.files {
            let target = dir.join(path.trim_start_matches('/'));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, bytes)?;
        }
        Ok(())
    }
}
