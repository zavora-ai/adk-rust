//! `awp verify` against the preview, as a launch-gate extension (spec 3.5 §7.8.1 "Machine
//! views", feature `verify`).

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_auth::signed_audit::{CheckpointBody, Note};
use adk_awp::signing::anchor::DnsTxtResolver;
use adk_awp::verify::fetch::{FetchError, Fetched, Fetcher};
use adk_awp::verify::pins::InMemoryPinStore;
use adk_awp::verify::{DEFAULT_SAMPLES, Verifier, VerifyOptions};
use async_trait::async_trait;
use awp_types::{ReleaseManifest, ReleaseSource};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use http::{HeaderMap, HeaderValue, header};

use super::{CheckResult, CheckStatus, Finding, GateCheck, GateExtension, GateInput};
use crate::error::{Result, SiteError};
use crate::host::SiteHost;
use crate::log::CHECKPOINT_PATH;
use crate::release::{
    PreviousRelease, ReleaseBuilder, ReleaseOptions, StagedRelease, content_type, is_log_path,
    manifest_digest, manifest_path,
};

/// Runs `awp verify` against a preview of the release a build would publish (spec 3.5 §7.8.1
/// "Machine views": "`awp verify` passes against the preview"). It reports as
/// [`GateCheck::AwpVerify`] in the full tier, with each failure named by its AWP 2.0 §15.2
/// name as the rule, such as `contract.signature`.
///
/// The preview is what a host's preview target serves: the build's files, staged as the
/// next release with its manifest, over the site log the host serves for its production
/// releases. The extension stages the build after the latest release the host's checkpoint
/// names and runs `adk-awp`'s [`Verifier::verify_preview`] against it, so the whole AWP 2.0
/// §15 procedure runs — contract, key set, DNS root, delegations, Agent Card, checkpoint,
/// tiles, manifests, executable paths, sampled pages and public terms — with the preview's
/// manifest bound by its staged digest instead of a log entry it does not have yet.
///
/// Documents on other origins, such as an Agent Card the edge serves, are fetched through
/// [`with_fetcher`](Self::with_fetcher); without one they are unavailable and the check
/// fails. The DNS anchor is checked against the given resolver: before the domain points at
/// the site, give it the `_awp-root` record the owner is about to publish.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_awp::signing::anchor::FixedTxtResolver;
/// use adk_site::gate::{LaunchGate, PreviewVerification};
/// use adk_site::host::DirectoryHost;
///
/// let host = Arc::new(DirectoryHost::new("/srv/example.com"));
/// let check = PreviewVerification::new("example.com", host, Arc::new(FixedTxtResolver::default()));
/// let gate = LaunchGate::new().with_extension(Arc::new(check));
/// # let _ = gate;
/// ```
pub struct PreviewVerification {
    domain: String,
    host: Arc<dyn SiteHost>,
    resolver: Arc<dyn DnsTxtResolver>,
    others: Option<Arc<dyn Fetcher>>,
    samples: usize,
    now: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for PreviewVerification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewVerification")
            .field("domain", &self.domain)
            .field("host", &self.host.name())
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}

impl PreviewVerification {
    /// Verifies previews of `domain`, over the site log `host` serves, with the DNS root
    /// anchor read through `resolver`.
    pub fn new(
        domain: impl Into<String>,
        host: Arc<dyn SiteHost>,
        resolver: Arc<dyn DnsTxtResolver>,
    ) -> Self {
        Self {
            domain: domain.into().trim_end_matches('.').to_ascii_lowercase(),
            host,
            resolver,
            others: None,
            samples: DEFAULT_SAMPLES,
            now: None,
        }
    }

    /// Fetches documents on other origins, such as the edge's Agent Card, through
    /// `fetcher`.
    #[must_use]
    pub fn with_fetcher(mut self, fetcher: Arc<dyn Fetcher>) -> Self {
        self.others = Some(fetcher);
        self
    }

    /// Samples `n` files of the preview (20 by default).
    #[must_use]
    pub fn with_samples(mut self, n: usize) -> Self {
        self.samples = n;
        self
    }

    /// Checks documents' validity at `now` instead of the current time.
    #[must_use]
    pub fn at(mut self, now: DateTime<Utc>) -> Self {
        self.now = Some(now);
        self
    }

    /// The latest release the host's site log names, resolved through its delta chain;
    /// `None` before the first.
    async fn logged(&self) -> Result<Option<PreviousRelease>> {
        let Some(note) = self.host.fetch(CHECKPOINT_PATH).await? else {
            return Ok(None);
        };
        let text = String::from_utf8_lossy(&note);
        let body = Note::parse(&text)
            .and_then(|note| CheckpointBody::parse(&note.text))
            .map_err(|e| SiteError::Log(format!("the host's checkpoint does not parse: {e}")))?;
        if body.size == 0 {
            return Ok(None);
        }
        let mut chain = vec![self.manifest(body.size).await?];
        while let Some(base) = chain.last().and_then(|manifest| manifest.base) {
            if chain.len() > 10_000 {
                return Err(SiteError::Log("the delta chain does not end in a snapshot".into()));
            }
            chain.push(self.manifest(base).await?);
        }
        let mut files = BTreeMap::new();
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
        }
        let latest = chain[0].clone();
        let snapshot =
            chain.last().map_or((latest.seq, latest.created_at), |m| (m.seq, m.created_at));
        let (digest, _) = manifest_digest(&latest)?;
        Ok(Some(PreviousRelease { manifest: latest, digest, files, last_snapshot: snapshot }))
    }

    async fn manifest(&self, seq: u64) -> Result<ReleaseManifest> {
        let path = format!("/{}", manifest_path(seq));
        let bytes = self.host.fetch(&path).await?.ok_or_else(|| SiteError::ReleaseNotFound {
            seq,
            message: "the host's log names it but serves no manifest".into(),
        })?;
        serde_json::from_slice(&bytes).map_err(|e| SiteError::serde(path, e))
    }
}

/// What a host's preview target serves: the staged release's files and manifest, over the
/// site log of the production releases.
struct PreviewSite {
    origin: String,
    staged: StagedRelease,
    host: Arc<dyn SiteHost>,
    others: Option<Arc<dyn Fetcher>>,
}

impl PreviewSite {
    async fn serve(&self, path: &str) -> std::result::Result<Option<(Vec<u8>, String)>, String> {
        let path = if path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
        if path.trim_start_matches('/') == manifest_path(self.staged.manifest.seq) {
            let bytes = self.staged.manifest_bytes.to_vec();
            return Ok(Some((bytes, "application/awp+json".into())));
        }
        if let Some(entry) = self.staged.files.get(&path) {
            let bytes = self.staged.read(&path).map_err(|e| e.to_string())?;
            return Ok(Some((bytes, entry.content_type.clone())));
        }
        if is_log_path(&path) {
            let bytes = self.host.fetch(&path).await.map_err(|e| e.to_string())?;
            return Ok(bytes.map(|bytes| (bytes.to_vec(), content_type(&path).to_string())));
        }
        Ok(None)
    }
}

#[async_trait]
impl Fetcher for PreviewSite {
    async fn get(
        &self,
        url: &str,
        headers: &HeaderMap,
    ) -> std::result::Result<Fetched, FetchError> {
        let failed = |reason: String| FetchError { url: url.to_string(), reason };
        let Some(rest) = url.strip_prefix(&self.origin).filter(|rest| rest.starts_with('/')) else {
            return match &self.others {
                Some(others) => others.get(url, headers).await,
                None => Err(failed("not on the previewed origin and no fetcher is set".into())),
            };
        };
        let path = rest.split(['?', '#']).next().unwrap_or(rest);
        let Some((body, media)) = self.serve(path).await.map_err(failed)? else {
            return Ok(Fetched { status: 404, headers: HeaderMap::new(), body: Bytes::new() });
        };
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(&media) {
            headers.insert(header::CONTENT_TYPE, value);
        }
        Ok(Fetched { status: 200, headers, body: Bytes::from(body) })
    }
}

#[async_trait]
impl GateExtension for PreviewVerification {
    fn check(&self) -> GateCheck {
        GateCheck::AwpVerify
    }

    async fn run(&self, input: &GateInput<'_>) -> Result<CheckResult> {
        let previous = self.logged().await?;
        let options = ReleaseOptions {
            source: match &previous {
                None => ReleaseSource::Onboarding,
                Some(_) => ReleaseSource::Edit { job: "launch-gate".into(), decision: None },
            },
            generator: input.report.generator.clone(),
            charter_version: input.project.config.charter_version,
            redirects: input.report.redirects.clone(),
            written: None,
            force_snapshot: true,
        };
        let staged = ReleaseBuilder::new().stage(input.out, previous.as_ref(), options).await?;
        let seq = staged.manifest.seq;
        let digest = staged.digest.clone();
        let site = PreviewSite {
            origin: format!("https://{}", self.domain),
            staged,
            host: self.host.clone(),
            others: self.others.clone(),
        };
        let verifier = Verifier::new(
            Arc::new(site),
            self.resolver.clone(),
            Arc::new(InMemoryPinStore::default()),
        )
        .with_options(VerifyOptions { samples: self.samples, now: self.now });
        let report = verifier.verify_preview(&self.domain, &digest).await;
        Ok(match report.failure {
            None => CheckResult {
                check: GateCheck::AwpVerify,
                status: CheckStatus::Passed,
                findings: Vec::new(),
                note: Some(format!(
                    "awp verify passes against the preview of release {seq} ({} files sampled)",
                    report.sampled.len()
                )),
            },
            Some(failure) => CheckResult {
                check: GateCheck::AwpVerify,
                status: CheckStatus::Failed,
                findings: vec![Finding::blocking(
                    failure.name,
                    None,
                    None,
                    format!(
                        "awp verify fails against the preview of release {seq}: {} (AWP 2.0 §15)",
                        failure.detail
                    ),
                )],
                note: None,
            },
        })
    }
}
