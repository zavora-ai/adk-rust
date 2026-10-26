//! `awp verify` as the back office runs it: published documents fetched with `GET` only,
//! signed with the site's key (spec 3.5 §7.3 "Outbound", §7.7, A-I9).
//!
//! | Verifier | Checks | Use |
//! |---|---|---|
//! | `adk_awp::verify::Verifier` | The whole AWP 2.0 §15 procedure: contract, keys, DNS root, delegations, card, log, releases, pages, public terms | A site that publishes its contract |
//! | [`ReleaseLogVerifier`] | The release log's checkpoint and inclusion, and sampled pages | A site whose contract is not published yet: onboarding, a migration's first releases |

use std::sync::Arc;
use std::time::Duration;

use adk_auth::signed_audit::NoteVerifier;
use adk_core::Result;
use adk_site::log::{
    CHECKPOINT_PATH, LOG_PREFIX, TileFetch, TileProofs, verify_checkpoint, verify_release_inclusion,
};
use adk_site::release::{file_digest, manifest_path};
use async_trait::async_trait;
use awp_types::ReleaseManifest;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::web::RequestSigner;

/// One check of a verification (spec 3.5 §7.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCheck {
    /// What was checked: `checkpoint`, `inclusion`, `pages`.
    pub name: String,
    /// Whether it passed.
    pub passed: bool,
    /// What was found.
    pub detail: String,
}

/// What `awp verify` found (spec 3.5 §7.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    /// The origin verified.
    pub origin: String,
    /// The release the log's checkpoint names, when it verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<u64>,
    /// Every check, in order.
    pub checks: Vec<VerifyCheck>,
}

impl VerifyReport {
    /// Whether every check passed.
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.passed)
    }
}

/// Verifies a served site from what it publishes (spec 3.5 §7.7): the contract, cards,
/// checkpoint, release inclusion and sampled pages: `adk-awp`'s `Verifier`, the library
/// behind `adk-rust awp verify`, or [`ReleaseLogVerifier`] before the site publishes a
/// contract.
#[async_trait]
pub trait SiteVerifier: Send + Sync {
    /// Verifies the site at `origin` with `GET` requests for published documents only
    /// (A-I9).
    ///
    /// # Errors
    ///
    /// Returns an error when the verification could not run; a site that fails a check is
    /// an `Ok` report that did not pass.
    async fn verify(&self, origin: &str) -> Result<VerifyReport>;
}

/// The release log and page checks of `awp verify` for a site that publishes no contract yet
/// (spec 3.5 §7.7, §7.18 stage 7): the checkpoint's
/// signature by a trusted release key and its origin, the current release's inclusion in
/// the log, and the digests of up to `samples` pages against the release manifest. Every
/// request is a `GET`, signed when a signer is set (spec 3.5 §7.3 "Outbound").
///
/// # Example
///
/// ```rust,ignore
/// let verifier = ReleaseLogVerifier::new("bakery.example", vec![release_key]).with_signer(signer);
/// let report = verifier.verify("https://bakery.example").await?;
/// assert!(report.passed());
/// ```
#[derive(Clone)]
pub struct ReleaseLogVerifier {
    domain: String,
    keys: Vec<NoteVerifier>,
    samples: usize,
    signer: Option<Arc<dyn RequestSigner>>,
    timeout: Duration,
}

impl std::fmt::Debug for ReleaseLogVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseLogVerifier")
            .field("domain", &self.domain)
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}

impl ReleaseLogVerifier {
    /// A verifier of `domain`'s log, trusting `keys` (its release keys), sampling 20 pages.
    pub fn new(domain: impl Into<String>, keys: Vec<NoteVerifier>) -> Self {
        Self {
            domain: domain.into(),
            keys,
            samples: 20,
            signer: None,
            timeout: Duration::from_secs(20),
        }
    }

    /// Samples `n` pages.
    #[must_use]
    pub fn with_samples(mut self, n: usize) -> Self {
        self.samples = n;
        self
    }

    /// Signs every request with `signer`.
    #[must_use]
    pub fn with_signer(mut self, signer: Arc<dyn RequestSigner>) -> Self {
        self.signer = Some(signer);
        self
    }
}

/// `GET`s published documents of one origin, signed when a signer is set.
pub(crate) struct OriginReader {
    origin: String,
    client: reqwest::Client,
    signer: Option<Arc<dyn RequestSigner>>,
}

impl OriginReader {
    pub(crate) fn new(
        origin: &str,
        signer: Option<Arc<dyn RequestSigner>>,
        timeout: Duration,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(timeout)
            .build()
            .map_err(|e| adk_core::AdkError::tool(format!("http client: {e}")))?;
        Ok(Self { origin: origin.trim_end_matches('/').to_string(), client, signer })
    }

    /// The body at `path`, `None` on 404.
    pub(crate) async fn get(&self, path: &str) -> Result<Option<Bytes>> {
        let url = format!("{}/{}", self.origin, path.trim_start_matches('/'));
        let mut request = self
            .client
            .get(&url)
            .build()
            .map_err(|e| adk_core::AdkError::tool(format!("request {url}: {e}")))?;
        if let Some(signer) = &self.signer {
            signer.sign(&mut request).map_err(|reason| {
                adk_core::AdkError::tool(format!("the request could not be signed: {reason}"))
            })?;
        }
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|e| adk_core::AdkError::tool(format!("GET {url}: {}", e.without_url())))?;
        match response.status().as_u16() {
            404 | 410 => Ok(None),
            200..=299 => Ok(Some(response.bytes().await.map_err(|e| {
                adk_core::AdkError::tool(format!("GET {url}: {}", e.without_url()))
            })?)),
            status => Err(adk_core::AdkError::tool(format!("GET {url} answered {status}"))),
        }
    }
}

struct HttpTiles<'a>(&'a OriginReader);

#[async_trait]
impl TileFetch for HttpTiles<'_> {
    async fn fetch_tile(&self, path: &str) -> adk_site::Result<Option<Bytes>> {
        self.0
            .get(&format!("{LOG_PREFIX}/{path}"))
            .await
            .map_err(|e| adk_site::SiteError::Log(e.to_string()))
    }
}

fn check(name: &str, passed: bool, detail: impl Into<String>) -> VerifyCheck {
    VerifyCheck { name: name.to_string(), passed, detail: detail.into() }
}

#[async_trait]
impl SiteVerifier for ReleaseLogVerifier {
    async fn verify(&self, origin: &str) -> Result<VerifyReport> {
        let reader = OriginReader::new(origin, self.signer.clone(), self.timeout)?;
        let mut report =
            VerifyReport { origin: origin.to_string(), release: None, checks: Vec::new() };
        let Some(note) = reader.get(CHECKPOINT_PATH).await? else {
            report.checks.push(check("checkpoint", false, "the site publishes no log checkpoint"));
            return Ok(report);
        };
        let checkpoint =
            match verify_checkpoint(&String::from_utf8_lossy(&note), &self.domain, &self.keys) {
                Ok(checkpoint) => checkpoint,
                Err(error) => {
                    report.checks.push(check("checkpoint", false, error.to_string()));
                    return Ok(report);
                }
            };
        let seq = checkpoint.size;
        report.release = Some(seq);
        report.checks.push(check(
            "checkpoint",
            true,
            format!("signed checkpoint of release {seq}"),
        ));
        let Some(manifest_bytes) = reader.get(&manifest_path(seq)).await? else {
            report.checks.push(check(
                "inclusion",
                false,
                format!("release {seq}'s manifest is not served"),
            ));
            return Ok(report);
        };
        let tiles = HttpTiles(&reader);
        let proofs = TileProofs::new(&tiles, seq);
        let included = async {
            let entry = proofs.entry(seq - 1).await?;
            let proof = proofs.inclusion_proof(seq - 1).await?;
            verify_release_inclusion(
                &checkpoint,
                &entry,
                &proof,
                seq,
                &file_digest(&manifest_bytes),
            )
        }
        .await;
        report.checks.push(match included {
            Ok(()) => check("inclusion", true, format!("release {seq} is in the log")),
            Err(error) => check("inclusion", false, error.to_string()),
        });
        let manifest: ReleaseManifest = match serde_json::from_slice(&manifest_bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                report.checks.push(check(
                    "pages",
                    false,
                    format!("the manifest does not parse: {error}"),
                ));
                return Ok(report);
            }
        };
        // A delta lists only its changes; the pages it lists are still the release's.
        let mut mismatched = Vec::new();
        let mut sampled = 0;
        for (path, entry) in
            manifest.files.iter().filter(|(p, _)| p.ends_with(".html")).take(self.samples)
        {
            sampled += 1;
            match reader.get(path).await? {
                Some(bytes) if file_digest(&bytes) == entry.sha256 => {}
                _ => mismatched.push(path.clone()),
            }
        }
        report.checks.push(if mismatched.is_empty() {
            check("pages", true, format!("{sampled} sampled pages match release {seq}"))
        } else {
            check(
                "pages",
                false,
                format!("pages differ from release {seq}: {}", mismatched.join(", ")),
            )
        });
        Ok(report)
    }
}

/// The whole `awp verify` (AWP 2.0 §15) as the back office's verifier. `origin` names the
/// domain, with or without its scheme.
#[async_trait]
impl SiteVerifier for adk_awp::verify::Verifier {
    async fn verify(&self, origin: &str) -> Result<VerifyReport> {
        let domain = origin
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let report = adk_awp::verify::Verifier::verify(self, domain).await;
        let mut checks = vec![match &report.failure {
            None => check("awp", true, format!("{} sampled files match", report.sampled.len())),
            Some(failure) => check(failure.name, false, failure.detail.clone()),
        }];
        if !report.posture.server_code.is_empty() {
            checks.push(check("server_code", false, report.posture.server_code.join(", ")));
        }
        Ok(VerifyReport { origin: origin.to_string(), release: report.release_seq, checks })
    }
}
