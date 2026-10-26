//! `verify`: the library behind `adk-rust awp verify <domain>` (feature `verify`; spec 3.5
//! §7.7, A-I9; AWP 2.0 §15).
//!
//! The verifier reads only what a site publishes, with `GET`, and walks the chain of trust:
//!
//! | Step | Check | Failure |
//! |---|---|---|
//! | 1 | The contract: media type, I-JSON, `awp` 2.x, `site.domain` | `contract.unavailable`, `contract.invalid` |
//! | 2 | The key set | `keys.unavailable`, `keys.invalid` |
//! | 3 | The `_awp-root` DNS record names the root; a changed pinned root is endorsed or anchored | `root.unanchored`, `root.changed` |
//! | 4 | The contract's proof by the root | `contract.signature` |
//! | 5 | Every delegation, revocation and endorsement proof | `delegation.signature` |
//! | 6 | The A2A Agent Card's signature by a `card` key | `card.signature` |
//! | 7 | The checkpoint's signature by a `release` key and its origin | `checkpoint.signature` |
//! | 8 | The log's tiles match the checkpoint and extend the pinned one | `log.consistency` |
//! | 9 | `release.seq` does not exceed the tree size | `contract.release_ahead` |
//! | 10 | The latest manifest is the last leaf | `release.inclusion` |
//! | 11 | The full file set through deltas, each manifest included, `previous` linked | `release.manifest` |
//! | 12 | No server-executable path | `release.executable` |
//! | 13 | N sampled files (default 20) match their digests | `page.digest` |
//! | 14 | The public terms match the contract's digest | `charter.digest` |
//!
//! A pass pins the root and the checkpoint through a [`PinStore`]. After the
//! procedure the report carries a posture and readiness summary.
//!
//! [`Verifier::verify_preview`] runs the same procedure against a preview of the next
//! release, whose manifest the log does not hold yet: steps 7 and 8 cover the logged
//! releases, and step 10 binds the preview's manifest by the digest its staging recorded
//! (spec 3.5 §7.8.1).

pub mod fetch;
pub mod pins;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_auth::signed_audit::merkle::root;
use adk_auth::signed_audit::{CheckpointBody, Note, canonicalize};
use adk_core::audit_log::AuditHash;
use awp_types::v2::contract::CONTRACT_PATH;
use awp_types::{
    ContractRelease, Digest, FileEntry, KeyScope, KeySet, LogEntry, Profile, PublicTerms,
    ReleaseManifest, SiteContract,
};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::signing::anchor::{DnsTxtResolver, check_root_anchor};
use crate::signing::card::verify_agent_card;
use crate::signing::jws::{document_digest, jwk_thumbprint, public_key_bytes, verify};
use crate::signing::keys::{Dated, delegated_key, verify_key_set};
use crate::signing::log::{
    LOG_PATH, checkpoint_verifier, entry_leaf, hash_tile_path, level0_tiles, log_origin,
};

use fetch::{Fetched, Fetcher};
use pins::{Pin, PinStore};

/// The default number of files sampled in step 13.
pub const DEFAULT_SAMPLES: usize = 20;

/// File name suffixes a release never carries (AWP 2.0 §14.3).
pub const EXECUTABLE_SUFFIXES: &[&str] =
    &[".php", ".phtml", ".phar", ".cgi", ".pl", ".py", ".asp", ".aspx", ".jsp", ".shtml"];

/// Returns `true` for a path a release must never carry: a server-executable name or
/// `.htaccess` (AWP 2.0 §14.3, A-I1).
pub fn is_executable_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.rsplit('/').next() == Some(".htaccess")
        || EXECUTABLE_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

/// A step that failed, by its AWP 2.0 §15.2 name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyFailure {
    /// The failure name, such as `contract.signature`.
    pub name: &'static str,
    /// What was wrong.
    pub detail: String,
}

impl std::fmt::Display for VerifyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.detail)
    }
}

fn fail(name: &'static str, detail: impl Into<String>) -> VerifyFailure {
    VerifyFailure { name, detail: detail.into() }
}

/// The posture and readiness summary (AWP 2.0 §15.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Posture {
    /// The contract's profile.
    pub profile: Option<Profile>,
    /// Signs of server code: a `dynamic` profile, executable paths, or headers such as
    /// `X-Powered-By`.
    pub server_code: Vec<String>,
    /// The identity methods the edge accepts.
    pub identity: Vec<String>,
    /// Whether a signed A2A Agent Card is published.
    pub a2a: bool,
    /// Whether an MCP Server Card is named.
    pub mcp: bool,
    /// The public terms' usage preferences.
    pub usage: Option<String>,
    /// Whether `robots.txt` states `Content-Usage`.
    pub robots_content_usage: bool,
    /// Whether `security.txt` is published and not expired.
    pub security_txt: bool,
    /// Whether the content origin sends HSTS.
    pub hsts: bool,
    /// Whether sampled HTML pages send a Content Security Policy.
    pub csp: bool,
}

/// The result of verifying a domain (AWP 2.0 §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// The domain.
    pub domain: String,
    /// The first failed step, or `None` on a pass.
    pub failure: Option<VerifyFailure>,
    /// The latest release verified.
    pub release_seq: Option<u64>,
    /// The files sampled in step 13.
    pub sampled: Vec<String>,
    /// `true` when the root changed without an endorsement by the pinned root and DNS
    /// anchored the new one: a recovery (AWP 2.0 §5.7).
    pub recovery: bool,
    /// The posture and readiness summary.
    pub posture: Posture,
}

impl VerifyReport {
    /// Returns `true` when every step passed.
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// Options of a verification.
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    /// Files sampled in step 13.
    pub samples: usize,
    /// The time live documents are checked at; `None` is now.
    pub now: Option<DateTime<Utc>>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self { samples: DEFAULT_SAMPLES, now: None }
    }
}

/// The verifier (AWP 2.0 §15).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_awp::signing::anchor::FixedTxtResolver;
/// use adk_awp::verify::fetch::HttpFetcher;
/// use adk_awp::verify::pins::FilePinStore;
/// use adk_awp::verify::Verifier;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let verifier = Verifier::new(
///     Arc::new(HttpFetcher::new()?),
///     Arc::new(FixedTxtResolver::default()),
///     Arc::new(FilePinStore::new("awp-pins.json")),
/// );
/// let report = verifier.verify("example.com").await;
/// println!("{}", report.failure.map_or("pass".to_owned(), |f| f.to_string()));
/// # Ok(())
/// # }
/// ```
pub struct Verifier {
    fetcher: Arc<dyn Fetcher>,
    resolver: Arc<dyn DnsTxtResolver>,
    pins: Arc<dyn PinStore>,
    options: VerifyOptions,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier").field("options", &self.options).finish_non_exhaustive()
    }
}

struct Run<'a> {
    verifier: &'a Verifier,
    domain: String,
    origin: String,
    now: DateTime<Utc>,
    posture: Posture,
    /// The manifest digest of the unlogged release a preview serves at the head of the log.
    preview: Option<Digest>,
}

impl Verifier {
    /// Creates a verifier.
    pub fn new(
        fetcher: Arc<dyn Fetcher>,
        resolver: Arc<dyn DnsTxtResolver>,
        pins: Arc<dyn PinStore>,
    ) -> Self {
        Self { fetcher, resolver, pins, options: VerifyOptions::default() }
    }

    /// Sets the options.
    pub fn with_options(mut self, options: VerifyOptions) -> Self {
        self.options = options;
        self
    }

    fn run(&self, domain: &str, preview: Option<Digest>) -> (Run<'_>, VerifyReport) {
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        let run = Run {
            verifier: self,
            origin: format!("https://{domain}"),
            domain: domain.clone(),
            now: self.options.now.unwrap_or_else(Utc::now),
            posture: Posture::default(),
            preview,
        };
        let report = VerifyReport {
            domain,
            failure: None,
            release_seq: None,
            sampled: Vec::new(),
            recovery: false,
            posture: Posture::default(),
        };
        (run, report)
    }

    /// Verifies `domain` and, on a pass, pins its root and checkpoint.
    pub async fn verify(&self, domain: &str) -> VerifyReport {
        let (mut run, mut report) = self.run(domain, None);
        match run.steps(&mut report).await {
            Ok(pin) => {
                if let Some(pin) = pin
                    && let Err(error) = self.pins.put(&report.domain, pin).await
                {
                    report.failure = Some(fail("state.unavailable", error.to_string()));
                }
            }
            Err(failure) => report.failure = Some(failure),
        }
        report.posture = run.posture;
        report
    }

    /// Verifies a preview of `domain`'s next release: the site as it will be served once the
    /// release it stages is logged (spec 3.5 §7.8.1 "Machine views"; AWP 2.0 §15).
    ///
    /// Every step of [`verify`](Self::verify) runs against what the preview serves, with one
    /// difference: the release at the head of the log is the preview's, which the log does
    /// not hold yet. Steps 7 and 8 verify the logged releases the preview carries — none
    /// before a site's first release — step 9 allows the contract to name the preview's
    /// release, and step 10 checks that the manifest served for the release after the
    /// checkpoint has the digest `candidate`, the one its staging recorded, in place of its
    /// inclusion in the log. Steps 11 to 13 then resolve and sample the preview's files. A
    /// preview pins nothing.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_awp::signing::anchor::FixedTxtResolver;
    /// use adk_awp::verify::Verifier;
    /// use adk_awp::verify::fetch::HttpFetcher;
    /// use adk_awp::verify::pins::InMemoryPinStore;
    ///
    /// # async fn run(staged: awp_types::Digest) -> Result<(), Box<dyn std::error::Error>> {
    /// let verifier = Verifier::new(
    ///     Arc::new(HttpFetcher::new()?),
    ///     Arc::new(FixedTxtResolver::default()),
    ///     Arc::new(InMemoryPinStore::default()),
    /// );
    /// let report = verifier.verify_preview("example.com", &staged).await;
    /// println!("{}", report.failure.map_or("pass".to_owned(), |f| f.to_string()));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn verify_preview(&self, domain: &str, candidate: &Digest) -> VerifyReport {
        let (mut run, mut report) = self.run(domain, Some(candidate.clone()));
        if let Err(failure) = run.steps(&mut report).await {
            report.failure = Some(failure);
        }
        report.posture = run.posture;
        report
    }
}

impl Run<'_> {
    async fn get(&self, url: &str) -> Result<Fetched, String> {
        self.verifier.fetcher.get(url, &HeaderMap::new()).await.map_err(|error| error.to_string())
    }

    async fn get_json(
        &self,
        url: &str,
        name: &'static str,
    ) -> Result<(Value, Fetched), VerifyFailure> {
        let fetched = self.get(url).await.map_err(|error| fail(name, error))?;
        if fetched.status != 200 {
            return Err(fail(name, format!("GET {url} answered {}", fetched.status)));
        }
        let value: Value = serde_json::from_slice(&fetched.body)
            .map_err(|error| fail(name, format!("{url} is not JSON: {error}")))?;
        canonicalize(&value)
            .map_err(|error| fail(name, format!("{url} is not I-JSON: {error}")))?;
        Ok((value, fetched))
    }

    fn note_server_headers(&mut self, fetched: &Fetched, url: &str) {
        for header in ["x-powered-by", "x-aspnet-version", "x-generator"] {
            if let Some(value) = fetched.header(header) {
                let note = format!("{url} sends {header}: {value}");
                if !self.posture.server_code.contains(&note) {
                    self.posture.server_code.push(note);
                }
            }
        }
    }

    async fn steps(&mut self, report: &mut VerifyReport) -> Result<Option<Pin>, VerifyFailure> {
        // 1. The contract.
        let contract_url = format!("{}{CONTRACT_PATH}", self.origin);
        let fetched = self.get(&contract_url).await.map_err(|e| fail("contract.unavailable", e))?;
        if fetched.status != 200 {
            return Err(fail("contract.unavailable", format!("answered {}", fetched.status)));
        }
        self.posture.hsts = fetched.header("strict-transport-security").is_some();
        self.note_server_headers(&fetched, &contract_url);
        if fetched.media_type().as_deref() != Some("application/awp+json") {
            return Err(fail(
                "contract.invalid",
                "the contract is not served as application/awp+json",
            ));
        }
        let contract_value: Value = serde_json::from_slice(&fetched.body)
            .map_err(|error| fail("contract.invalid", error.to_string()))?;
        canonicalize(&contract_value)
            .map_err(|error| fail("contract.invalid", error.to_string()))?;
        let contract: SiteContract = serde_json::from_value(contract_value.clone())
            .map_err(|error| fail("contract.invalid", error.to_string()))?;
        if contract.awp.major != 2 {
            return Err(fail("contract.invalid", format!("awp {} is not 2.x", contract.awp)));
        }
        if contract.site.domain.to_ascii_lowercase() != self.domain {
            return Err(fail(
                "contract.invalid",
                "site.domain is not the host it was fetched from",
            ));
        }
        self.posture.profile = contract.profile;
        if contract.profile == Some(Profile::Dynamic) {
            self.posture
                .server_code
                .push("the dynamic profile composes pages per request".to_owned());
        }
        if let Some(identity) = &contract.identity {
            self.posture.identity = identity
                .accepts
                .iter()
                .filter_map(|method| serde_json::to_value(method).ok())
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect();
        }
        self.posture.mcp =
            contract.surfaces.as_ref().is_some_and(|surfaces| surfaces.mcp_server_card.is_some());

        // 2. The key set.
        let keys_url = contract
            .keys
            .clone()
            .ok_or_else(|| fail("keys.unavailable", "the contract names no keys"))?;
        if keys_url != format!("{}/.well-known/awp/keys.json", self.origin) {
            return Err(fail(
                "keys.invalid",
                "keys must be at /.well-known/awp/keys.json on the domain",
            ));
        }
        let (keys_value, _) = self.get_json(&keys_url, "keys.unavailable").await?;
        let mut keys: KeySet = serde_json::from_value(keys_value)
            .map_err(|error| fail("keys.invalid", error.to_string()))?;

        // 3. The DNS anchor and the pinned root.
        let pin = self
            .verifier
            .pins
            .get(&self.domain)
            .await
            .map_err(|e| fail("state.unavailable", e.to_string()))?;
        check_root_anchor(
            self.verifier.resolver.as_ref(),
            &self.domain,
            &keys.root.kid,
            &keys.root.public_key,
        )
        .await
        .map_err(|error| fail("root.unanchored", error.to_string()))?;
        let thumbprint = jwk_thumbprint(&keys.root.public_key);
        if let Some(pin) = &pin
            && pin.root_thumbprint != thumbprint
        {
            let endorsed = keys.root.endorsement.as_ref().is_some_and(|endorsement| {
                jwk_thumbprint(&endorsement.previous) == pin.root_thumbprint
            });
            report.recovery = !endorsed;
        }

        // 4. The contract's proof.
        let proof = contract
            .proof
            .clone()
            .ok_or_else(|| fail("contract.signature", "the contract is unsigned"))?;
        if proof.kid != keys.root.kid {
            return Err(fail(
                "contract.signature",
                format!("signed by `{}`, not the root", proof.kid),
            ));
        }
        verify(&contract_value, &proof, &keys.root.public_key)
            .map_err(|error| fail("contract.signature", error.to_string()))?;

        // 5. Delegations, revocations, endorsement; revocations seen before stay.
        verify_key_set(&keys).map_err(|error| fail("delegation.signature", error.to_string()))?;
        let mut revoked: BTreeSet<String> =
            pin.as_ref().map(|pin| pin.revoked.clone()).unwrap_or_default();
        revoked.extend(keys.revocations.iter().map(|revocation| revocation.kid.clone()));
        keys.delegations.retain(|delegation| {
            !revoked.contains(&delegation.kid)
                || keys.revocations.iter().any(|revocation| revocation.kid == delegation.kid)
        });

        // 6. The A2A Agent Card.
        if let Some(card_url) = contract.surfaces.as_ref().and_then(|s| s.a2a_card.clone()) {
            let (card, _) = self.get_json(&card_url, "card.signature").await?;
            verify_agent_card(&card, &keys, self.now)
                .map_err(|error| fail("card.signature", error.to_string()))?;
            self.posture.a2a = true;
        }

        // 14 (independent of the log). The public terms.
        let mut terms: Option<PublicTerms> = None;
        if let Some(link) = &contract.charter {
            let (value, _) = self.get_json(&link.public, "charter.digest").await?;
            let digest = document_digest(&value)
                .map_err(|error| fail("charter.digest", error.to_string()))?;
            if digest != link.digest {
                return Err(fail(
                    "charter.digest",
                    "the public terms do not match the contract's digest",
                ));
            }
            terms = serde_json::from_value(value).ok();
        }
        self.posture.usage = terms.as_ref().and_then(|terms| terms.usage.clone());
        self.readiness(&contract).await;

        let Some(release) = contract.release.clone() else {
            if contract.profile == Some(Profile::Dynamic) {
                return Ok(Some(Pin {
                    root_kid: keys.root.kid.clone(),
                    root_thumbprint: thumbprint,
                    tree_size: pin.as_ref().map_or(0, |pin| pin.tree_size),
                    root_hash: pin.as_ref().map(|pin| pin.root_hash.clone()).unwrap_or_default(),
                    revoked,
                }));
            }
            return Err(fail("contract.invalid", "a static profile names its release"));
        };

        // 7. The checkpoint.
        let checkpoint_url = format!("{}{LOG_PATH}checkpoint", self.origin);
        let fetched =
            self.get(&checkpoint_url).await.map_err(|e| fail("checkpoint.signature", e))?;
        if fetched.status == 404
            && let Some(candidate) = self.preview.clone()
        {
            // A preview of a site's first release: nothing is logged yet.
            self.preview_head(report, &release, 0, &[], &candidate).await?;
            return Ok(None);
        }
        if fetched.status != 200 {
            return Err(fail(
                "checkpoint.signature",
                format!("the checkpoint answered {}", fetched.status),
            ));
        }
        let text = String::from_utf8(fetched.body.to_vec())
            .map_err(|_| fail("checkpoint.signature", "not UTF-8"))?;
        let note =
            Note::parse(&text).map_err(|error| fail("checkpoint.signature", error.to_string()))?;
        let mut verifiers = Vec::new();
        for delegation in &keys.delegations {
            if delegated_key(&keys, &delegation.kid, KeyScope::Release, Dated::Live(self.now))
                .is_ok()
                && let Ok(raw) = public_key_bytes(&delegation.public_key)
                && let Ok(verifier) = checkpoint_verifier(&self.domain, &raw)
            {
                verifiers.push(verifier);
            }
        }
        if note.verified_by(&verifiers).is_empty() {
            return Err(fail("checkpoint.signature", "no valid release key signed the checkpoint"));
        }
        let body = CheckpointBody::parse(&note.text)
            .map_err(|error| fail("checkpoint.signature", error.to_string()))?;
        if body.origin != log_origin(&self.domain) {
            return Err(fail(
                "checkpoint.signature",
                format!("origin `{}` is not this site's log", body.origin),
            ));
        }

        // 8. The tiles match the checkpoint and extend the pinned tree.
        let leaves = self.leaves(body.size).await?;
        if root(&leaves) != body.root_hash {
            return Err(fail("log.consistency", "the tiles do not hash to the checkpoint's root"));
        }
        if let Some(pin) = &pin
            && pin.tree_size > 0
        {
            let extends = pin.tree_size <= body.size
                && root(&leaves[..pin.tree_size as usize]).to_string() == pin.root_hash;
            if !extends {
                return Err(fail(
                    "log.consistency",
                    format!(
                        "the checkpoint does not extend the pinned tree of {} entries",
                        pin.tree_size
                    ),
                ));
            }
        }

        if let Some(candidate) = self.preview.clone() {
            self.preview_head(report, &release, body.size, &leaves, &candidate).await?;
            return Ok(None);
        }

        // 9. The contract's release is not ahead of the log.
        if release.seq > body.size {
            return Err(fail(
                "contract.release_ahead",
                format!("release {} exceeds the tree size {}", release.seq, body.size),
            ));
        }

        // 10–11. The latest release and its full file set.
        let latest = body.size;
        let manifest = self.manifest(latest, &leaves, "release.inclusion").await?;
        let files = self.resolve(&manifest, &leaves).await?;
        report.release_seq = Some(latest);

        self.files(report, &files).await?;

        Ok(Some(Pin {
            root_kid: keys.root.kid.clone(),
            root_thumbprint: thumbprint,
            tree_size: body.size,
            root_hash: body.root_hash.to_string(),
            revoked,
        }))
    }

    /// Steps 12 and 13 over a release's full file set.
    async fn files(
        &mut self,
        report: &mut VerifyReport,
        files: &BTreeMap<String, FileEntry>,
    ) -> Result<(), VerifyFailure> {
        // 12. No server-executable path.
        if let Some(path) = files.keys().find(|path| is_executable_path(path)) {
            self.posture.server_code.push(format!("executable path {path}"));
            return Err(fail("release.executable", format!("the release carries {path}")));
        }

        // 13. Sampled files.
        let candidates: Vec<&String> = files
            .keys()
            .filter(|path| {
                !path.starts_with("/.well-known/awp/releases/") && !path.starts_with(LOG_PATH)
            })
            .collect();
        let samples = self.verifier.options.samples.min(candidates.len());
        for index in 0..samples {
            let path = candidates[index * candidates.len() / samples.max(1)];
            report.sampled.push(path.clone());
            self.check_page(path, &files[path]).await?;
        }
        Ok(())
    }

    /// Steps 9 to 13 of a preview: the release after the `size` logged ones is the
    /// preview's, its manifest bound by the digest its staging recorded.
    async fn preview_head(
        &mut self,
        report: &mut VerifyReport,
        release: &ContractRelease,
        size: u64,
        leaves: &[AuditHash],
        candidate: &Digest,
    ) -> Result<(), VerifyFailure> {
        let seq = size + 1;
        // 9. The contract names at most the preview's release.
        if release.seq > seq {
            return Err(fail(
                "contract.release_ahead",
                format!("release {} exceeds the preview's release {seq}", release.seq),
            ));
        }
        // 10. The preview's manifest is the one its staging recorded.
        let url = format!("{}/.well-known/awp/releases/{seq}.json", self.origin);
        let (value, _) = self.get_json(&url, "release.inclusion").await?;
        let digest = document_digest(&value)
            .map_err(|error| fail("release.inclusion", error.to_string()))?;
        if &digest != candidate {
            return Err(fail(
                "release.inclusion",
                format!("release {seq}'s manifest is not the staged preview {candidate}"),
            ));
        }
        let manifest: ReleaseManifest = serde_json::from_value(value)
            .map_err(|error| fail("release.inclusion", error.to_string()))?;
        if manifest.seq != seq {
            return Err(fail("release.inclusion", format!("{url} names release {}", manifest.seq)));
        }
        if seq == 1 && manifest.previous.is_some() {
            return Err(fail("release.manifest", "the first release names a previous one"));
        }
        // 11. Its full file set, linked to the logged releases.
        let files = self.resolve(&manifest, leaves).await?;
        report.release_seq = Some(seq);
        self.files(report, &files).await
    }

    async fn leaves(&self, size: u64) -> Result<Vec<AuditHash>, VerifyFailure> {
        let mut leaves = Vec::with_capacity(size as usize);
        for (index, width) in level0_tiles(size) {
            let url = format!("{}{LOG_PATH}{}", self.origin, hash_tile_path(0, index, width));
            let fetched = self.get(&url).await.map_err(|e| fail("log.consistency", e))?;
            if fetched.status != 200 || fetched.body.len() as u64 != width * 32 {
                return Err(fail(
                    "log.consistency",
                    format!("tile {url} is missing or the wrong size"),
                ));
            }
            for chunk in fetched.body.chunks_exact(32) {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(chunk);
                leaves.push(AuditHash(hash));
            }
        }
        Ok(leaves)
    }

    async fn manifest(
        &self,
        seq: u64,
        leaves: &[AuditHash],
        name: &'static str,
    ) -> Result<ReleaseManifest, VerifyFailure> {
        let url = format!("{}/.well-known/awp/releases/{seq}.json", self.origin);
        let (value, _) = self.get_json(&url, name).await?;
        let digest = document_digest(&value).map_err(|error| fail(name, error.to_string()))?;
        let manifest: ReleaseManifest =
            serde_json::from_value(value).map_err(|error| fail(name, error.to_string()))?;
        if manifest.seq != seq {
            return Err(fail(name, format!("{url} names release {}", manifest.seq)));
        }
        let leaf = entry_leaf(&LogEntry { seq, manifest: digest })
            .map_err(|error| fail(name, error.to_string()))?;
        let index = usize::try_from(seq.saturating_sub(1)).unwrap_or(usize::MAX);
        if seq == 0 || leaves.get(index) != Some(&leaf) {
            return Err(fail(
                name,
                format!("release {seq}'s manifest is not the log's entry {seq}"),
            ));
        }
        Ok(manifest)
    }

    async fn resolve(
        &self,
        manifest: &ReleaseManifest,
        leaves: &[AuditHash],
    ) -> Result<BTreeMap<String, FileEntry>, VerifyFailure> {
        if manifest.seq > 1 {
            let previous = self.manifest(manifest.seq - 1, leaves, "release.manifest").await?;
            let digest = document_digest(&previous)
                .map_err(|error| fail("release.manifest", error.to_string()))?;
            if manifest.previous.as_ref() != Some(&digest) {
                return Err(fail(
                    "release.manifest",
                    format!("release {} does not link release {}", manifest.seq, previous.seq),
                ));
            }
        }
        // Walk deltas back to a snapshot, then apply them forward.
        let mut chain = vec![manifest.clone()];
        while let Some(base) = chain.last().and_then(|last| last.base) {
            let last_seq = chain.last().map_or(0, |last| last.seq);
            if base >= last_seq || chain.len() > 1_000 {
                return Err(fail(
                    "release.manifest",
                    format!("release {last_seq} names base {base}"),
                ));
            }
            chain.push(self.manifest(base, leaves, "release.manifest").await?);
        }
        let mut files = BTreeMap::new();
        for release in chain.iter().rev() {
            if release.is_snapshot() && !release.removed.is_empty() {
                return Err(fail(
                    "release.manifest",
                    format!("snapshot {} removes files", release.seq),
                ));
            }
            files = release.apply_to(&files);
        }
        Ok(files)
    }

    async fn check_page(&mut self, path: &str, entry: &FileEntry) -> Result<(), VerifyFailure> {
        let url = format!("{}{path}", self.origin);
        let fetched = self.get(&url).await.map_err(|e| fail("page.digest", e))?;
        if fetched.status != 200 {
            return Err(fail("page.digest", format!("{path} answered {}", fetched.status)));
        }
        let actual = awp_types::Digest::from_sha256_bytes(Sha256::digest(&fetched.body).into());
        if actual != entry.sha256 {
            return Err(fail("page.digest", format!("{path} does not match its release digest")));
        }
        if entry.content_type.starts_with("text/html") {
            self.posture.csp |= fetched.header("content-security-policy").is_some();
            self.note_server_headers(&fetched, &url);
        }
        Ok(())
    }

    async fn readiness(&mut self, contract: &SiteContract) {
        let security = contract
            .contact
            .as_ref()
            .and_then(|contact| contact.security.clone())
            .unwrap_or_else(|| format!("{}/.well-known/security.txt", self.origin));
        if let Ok(fetched) = self.get(&security).await
            && fetched.status == 200
        {
            let text = String::from_utf8_lossy(&fetched.body).to_string();
            self.posture.security_txt = text.lines().any(|line| {
                line.strip_prefix("Expires:")
                    .and_then(|value| DateTime::parse_from_rfc3339(value.trim()).ok())
                    .is_some_and(|expires| expires.with_timezone(&Utc) > self.now)
            });
        }
        if let Ok(fetched) = self.get(&format!("{}/robots.txt", self.origin)).await
            && fetched.status == 200
        {
            self.posture.robots_content_usage = String::from_utf8_lossy(&fetched.body)
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("content-usage:"));
        }
    }
}
